use std::collections::HashMap;

use oxc_ast::ast::PropertyKey as OxcPropertyKey;
use quickjs_bytecode::{Binary64Constant, CompilerAtom, CompilerString};
use quickjs_frontend::{Span, decode_oxc_cooked_string};

use crate::storage::{BindingId, ExecutableId};

use super::constants::{CompiledConstantCandidate, CompiledStringLocation};
use super::{LeafCompilationError, ModuleBindingId, RealmGlobalId, checked_function_entry_count};

pub(in crate::lowering) struct CompiledAtomCandidate {
    pub(in crate::lowering) value: CompilerString,
    pub(in crate::lowering) span: Span,
    pub(in crate::lowering) purpose: CompiledAtomPurpose,
    pub(in crate::lowering) property_key: Option<CompiledPropertyAtomKey>,
}

pub(in crate::lowering) struct CompiledStaticPropertyKey {
    pub(in crate::lowering) value: CompilerString,
    pub(in crate::lowering) span: Span,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::lowering) struct CompiledAtomCandidateOrderKey {
    start: u32,
    end: u32,
    purpose: CompiledAtomPurpose,
    property: Option<CompiledPropertyAtomOrderKey>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::lowering) struct CompiledPropertyAtomOrderKey {
    kind: u8,
    array_start: u32,
    array_end: u32,
    index: u32,
}

impl CompiledAtomCandidate {
    pub(in crate::lowering) const fn order_key(&self) -> CompiledAtomCandidateOrderKey {
        CompiledAtomCandidateOrderKey {
            start: self.span.start,
            end: self.span.end,
            purpose: self.purpose,
            property: match self.property_key {
                Some(key) => Some(key.order_key()),
                None => None,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::lowering) enum CompiledAtomPurpose {
    RuntimeString,
    Property,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::lowering) enum CompiledPropertyAtomKey {
    Source(Span),
    ArrayIndex {
        array: Span,
        index: u32,
    },
    ArrayLength {
        array: Span,
    },
    /// The `prototype` read required while evaluating one derived class's
    /// heritage. This is distinct from source-spelled properties so the
    /// verifier can certify the class-heritage stack contract.
    ClassHeritagePrototype {
        class: Span,
    },
    YieldStarDone {
        expression: Span,
    },
    YieldStarValue {
        expression: Span,
    },
}

impl CompiledPropertyAtomKey {
    pub(in crate::lowering) const fn order_key(self) -> CompiledPropertyAtomOrderKey {
        match self {
            Self::Source(span) => CompiledPropertyAtomOrderKey {
                kind: 0,
                array_start: span.start,
                array_end: span.end,
                index: 0,
            },
            Self::ArrayIndex { array, index } => CompiledPropertyAtomOrderKey {
                kind: 1,
                array_start: array.start,
                array_end: array.end,
                index,
            },
            Self::ArrayLength { array } => CompiledPropertyAtomOrderKey {
                kind: 2,
                array_start: array.start,
                array_end: array.end,
                index: 0,
            },
            Self::ClassHeritagePrototype { class } => CompiledPropertyAtomOrderKey {
                kind: 3,
                array_start: class.start,
                array_end: class.end,
                index: 0,
            },
            Self::YieldStarDone { expression } => CompiledPropertyAtomOrderKey {
                kind: 4,
                array_start: expression.start,
                array_end: expression.end,
                index: 0,
            },
            Self::YieldStarValue { expression } => CompiledPropertyAtomOrderKey {
                kind: 5,
                array_start: expression.start,
                array_end: expression.end,
                index: 0,
            },
        }
    }

    pub(in crate::lowering) const fn span(self) -> Span {
        match self {
            Self::Source(span) => span,
            Self::ArrayIndex { array, .. } | Self::ArrayLength { array } => array,
            Self::ClassHeritagePrototype { class } => class,
            Self::YieldStarDone { expression } | Self::YieldStarValue { expression } => expression,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::lowering) enum CompiledMetadataAtomKey {
    FunctionName,
    ScriptCompletion,
    ScriptFinallyCompletion,
    RawParameter(u32),
    Binding(BindingId),
    RealmGlobal(RealmGlobalId),
    ModuleBinding(ModuleBindingId),
    ModuleRequest(u32),
}

pub(in crate::lowering) struct CompiledMetadataAtomCandidate {
    pub(in crate::lowering) key: CompiledMetadataAtomKey,
    pub(in crate::lowering) value: CompilerString,
    pub(in crate::lowering) span: Span,
}

impl CompiledMetadataAtomCandidate {
    pub(in crate::lowering) const fn order_key(&self) -> (CompiledMetadataAtomKey, u32, u32) {
        (self.key, self.span.start, self.span.end)
    }
}

pub(in crate::lowering) fn freeze_atom_candidates(
    candidates: Vec<CompiledAtomCandidate>,
    string_indices: &mut Vec<(Span, CompiledStringLocation)>,
    property_atom_indices: &mut Vec<(CompiledPropertyAtomKey, u32)>,
) -> Result<(Vec<CompilerAtom>, HashMap<CompilerString, u32>), LeafCompilationError> {
    let mut atoms = Vec::with_capacity(candidates.len());
    let mut interner = HashMap::with_capacity(candidates.len());
    for candidate in candidates {
        let static_property_only = candidate.purpose == CompiledAtomPurpose::Property
            && (candidate.value.is_empty() || candidate.value.is_tagged_integer_atom());
        if candidate.purpose == CompiledAtomPurpose::RuntimeString
            && (candidate.value.is_empty() || candidate.value.is_tagged_integer_atom())
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "runtime string atoms are nonempty non-tagged-integer spellings",
                span: Some(candidate.span),
            });
        }
        let atom_index = if let Some(&index) = interner.get(&candidate.value) {
            index
        } else {
            let next_count =
                atoms
                    .len()
                    .checked_add(1)
                    .ok_or(LeafCompilationError::CapacityExceeded {
                        domain: "atom pool entries",
                    })?;
            checked_function_entry_count(next_count, "atom pool entries")?;
            let index =
                u32::try_from(atoms.len()).map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "atom pool entries",
                })?;
            atoms.push(if static_property_only {
                CompilerAtom::new_static_property_only(candidate.value.clone())
            } else {
                CompilerAtom::new(candidate.value.clone())
            });
            interner.insert(candidate.value, index);
            index
        };
        match candidate.purpose {
            CompiledAtomPurpose::RuntimeString => {
                if candidate.property_key.is_some() {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "runtime string atom has no property lookup key",
                        span: Some(candidate.span),
                    });
                }
                string_indices.push((candidate.span, CompiledStringLocation::Atom(atom_index)));
            }
            CompiledAtomPurpose::Property => {
                let property_key =
                    candidate
                        .property_key
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "property atom has one typed lookup key",
                            span: Some(candidate.span),
                        })?;
                if property_atom_indices
                    .iter()
                    .any(|(key, index)| *key == property_key && *index == atom_index)
                {
                    continue;
                }
                property_atom_indices.push((property_key, atom_index));
            }
        }
    }
    Ok((atoms, interner))
}

pub(in crate::lowering) fn freeze_metadata_atom_candidates(
    candidates: Vec<CompiledMetadataAtomCandidate>,
    atoms: &mut Vec<CompilerAtom>,
    interner: &mut HashMap<CompilerString, u32>,
) -> Result<Vec<(CompiledMetadataAtomKey, u32)>, LeafCompilationError> {
    let mut indices = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if candidate.value.is_empty() || candidate.value.is_tagged_integer_atom() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "metadata atom names are nonempty identifiers",
                span: Some(candidate.span),
            });
        }
        let atom_index = if let Some(&index) = interner.get(&candidate.value) {
            index
        } else {
            let next_count =
                atoms
                    .len()
                    .checked_add(1)
                    .ok_or(LeafCompilationError::CapacityExceeded {
                        domain: "atom pool entries",
                    })?;
            checked_function_entry_count(next_count, "atom pool entries")?;
            let index =
                u32::try_from(atoms.len()).map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "atom pool entries",
                })?;
            atoms.push(CompilerAtom::new(candidate.value.clone()));
            interner.insert(candidate.value, index);
            index
        };
        indices.push((candidate.key, atom_index));
    }
    indices.sort_unstable_by_key(|(key, _)| *key);
    if indices.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "one metadata atom index per function field",
            span: None,
        });
    }
    Ok(indices)
}

pub(in crate::lowering) fn decode_compiler_string(
    value: &str,
    lone_surrogates: bool,
    span: Span,
) -> Result<CompilerString, LeafCompilationError> {
    let code_units = decode_oxc_cooked_string(value, lone_surrogates)
        .map_err(|source| LeafCompilationError::CookedStringDecoding { span, source })?;
    CompilerString::try_from_code_units(code_units)
        .map_err(|source| LeafCompilationError::CompilerString { span, source })
}

pub(in crate::lowering) fn compiler_identifier_string(
    value: &str,
    span: Span,
) -> Result<CompilerString, LeafCompilationError> {
    CompilerString::try_from_code_units(value.encode_utf16().collect::<Vec<_>>().into())
        .map_err(|source| LeafCompilationError::CompilerString { span, source })
}

pub(in crate::lowering) fn compiled_static_property_key(
    key: &OxcPropertyKey<'_>,
) -> Result<Option<CompiledStaticPropertyKey>, LeafCompilationError> {
    let (value, span) = match key {
        OxcPropertyKey::StaticIdentifier(identifier) => (
            compiler_identifier_string(identifier.name.as_str(), identifier.span)?,
            identifier.span,
        ),
        OxcPropertyKey::StringLiteral(literal) => (
            decode_compiler_string(
                literal.value.as_str(),
                literal.lone_surrogates,
                literal.span,
            )?,
            literal.span,
        ),
        OxcPropertyKey::NumericLiteral(literal) => {
            let value = Binary64Constant::from_f64(literal.value).to_javascript_string();
            (
                compiler_identifier_string(&value, literal.span)?,
                literal.span,
            )
        }
        OxcPropertyKey::BigIntLiteral(literal) => (
            compiler_identifier_string(literal.value.as_str(), literal.span)?,
            literal.span,
        ),
        _ => return Ok(None),
    };
    Ok(Some(CompiledStaticPropertyKey { value, span }))
}

pub(in crate::lowering) fn record_string_candidate(
    owner: ExecutableId,
    value: CompilerString,
    span: Span,
    constants: &mut [Vec<CompiledConstantCandidate>],
    atoms: &mut [Vec<CompiledAtomCandidate>],
) -> Result<(), LeafCompilationError> {
    if value.is_empty() {
        return Ok(());
    }
    if value.is_tagged_integer_atom() {
        constants
            .get_mut(owner.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable: owner })?
            .push(CompiledConstantCandidate::String { value, span });
    } else {
        atoms
            .get_mut(owner.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable: owner })?
            .push(CompiledAtomCandidate {
                value,
                span,
                purpose: CompiledAtomPurpose::RuntimeString,
                property_key: None,
            });
    }
    Ok(())
}

pub(in crate::lowering) fn record_property_candidate(
    owner: ExecutableId,
    value: CompilerString,
    span: Span,
    atoms: &mut [Vec<CompiledAtomCandidate>],
) -> Result<(), LeafCompilationError> {
    record_property_candidate_for(
        owner,
        value,
        span,
        CompiledPropertyAtomKey::Source(span),
        atoms,
    )
}

pub(in crate::lowering) fn record_property_candidate_for(
    owner: ExecutableId,
    value: CompilerString,
    span: Span,
    property_key: CompiledPropertyAtomKey,
    atoms: &mut [Vec<CompiledAtomCandidate>],
) -> Result<(), LeafCompilationError> {
    atoms
        .get_mut(owner.index())
        .ok_or(LeafCompilationError::InvalidExecutable { executable: owner })?
        .push(CompiledAtomCandidate {
            value,
            span,
            purpose: CompiledAtomPurpose::Property,
            property_key: Some(property_key),
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use quickjs_bytecode::CompilerString;
    use quickjs_frontend::Span;

    use super::{
        CompiledAtomCandidate, CompiledAtomPurpose, CompiledMetadataAtomCandidate,
        CompiledMetadataAtomKey, CompiledPropertyAtomKey, freeze_atom_candidates,
        freeze_metadata_atom_candidates,
    };
    use crate::lowering::constants::CompiledStringLocation;

    fn string(code_units: &[u16]) -> CompilerString {
        CompilerString::try_from_code_units(Arc::from(code_units)).expect("compiler string")
    }

    #[test]
    fn runtime_and_metadata_candidates_share_one_exact_utf16_atom() {
        let lambda = string(&[0x03bb]);
        let mut string_indices = Vec::new();
        let mut property_indices = Vec::new();
        let (mut atoms, mut interner) = freeze_atom_candidates(
            vec![CompiledAtomCandidate {
                value: lambda.clone(),
                span: Span::new(1, 3),
                purpose: CompiledAtomPurpose::RuntimeString,
                property_key: None,
            }],
            &mut string_indices,
            &mut property_indices,
        )
        .expect("runtime atoms");
        let metadata = freeze_metadata_atom_candidates(
            vec![CompiledMetadataAtomCandidate {
                key: CompiledMetadataAtomKey::FunctionName,
                value: lambda,
                span: Span::new(4, 6),
            }],
            &mut atoms,
            &mut interner,
        )
        .expect("metadata atoms");

        assert_eq!(atoms.len(), 1);
        assert_eq!(atoms[0].string().code_units().collect::<Vec<_>>(), [0x03bb]);
        assert!(matches!(
            string_indices.as_slice(),
            [(span, CompiledStringLocation::Atom(0))] if *span == Span::new(1, 3)
        ));
        assert_eq!(metadata, [(CompiledMetadataAtomKey::FunctionName, 0)]);
    }

    #[test]
    fn tagged_integer_property_is_canonicalized_to_a_restricted_atom() {
        let mut string_indices = Vec::new();
        let mut property_indices = Vec::new();
        let (atoms, _) = freeze_atom_candidates(
            vec![CompiledAtomCandidate {
                value: string(&[u16::from(b'7')]),
                span: Span::new(8, 9),
                purpose: CompiledAtomPurpose::Property,
                property_key: Some(CompiledPropertyAtomKey::Source(Span::new(8, 9))),
            }],
            &mut string_indices,
            &mut property_indices,
        )
        .expect("property atom");

        assert!(string_indices.is_empty());
        assert_eq!(property_indices.len(), 1);
        assert!(atoms[0].is_static_property_only());
    }

    #[test]
    fn identical_property_consumers_share_one_typed_lookup_key() {
        let span = Span::new(8, 13);
        let value = string(&[
            u16::from(b'v'),
            u16::from(b'a'),
            u16::from(b'l'),
            u16::from(b'u'),
            u16::from(b'e'),
        ]);
        let candidate = || CompiledAtomCandidate {
            value: value.clone(),
            span,
            purpose: CompiledAtomPurpose::Property,
            property_key: Some(CompiledPropertyAtomKey::Source(span)),
        };
        let mut string_indices = Vec::new();
        let mut property_indices = Vec::new();

        let (atoms, _) = freeze_atom_candidates(
            vec![candidate(), candidate()],
            &mut string_indices,
            &mut property_indices,
        )
        .expect("shared property atom");

        assert!(string_indices.is_empty());
        assert_eq!(atoms.len(), 1);
        assert_eq!(
            property_indices,
            [(CompiledPropertyAtomKey::Source(span), 0)]
        );
    }
}
