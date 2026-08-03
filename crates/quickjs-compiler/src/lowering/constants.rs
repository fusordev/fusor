use std::sync::Arc;

use quickjs_bytecode::{
    AtomPoolIndex, Binary64Constant, CompilerAtom, CompilerConstantValue, CompilerString,
    FinalOpcode, Operands,
};
use quickjs_frontend::Span;

use crate::storage::ExecutableId;

use super::atoms::{
    CompiledAtomCandidate, CompiledMetadataAtomCandidate, CompiledMetadataAtomKey,
    CompiledPropertyAtomKey, freeze_atom_candidates, freeze_metadata_atom_candidates,
};
use super::{
    CompiledConstant, CompiledFunctionConstant, LeafCompilationError, PlannedInstruction,
    checked_function_entry_count,
};

pub(in crate::lowering) struct CompiledConstantPool {
    atoms: Arc<[CompilerAtom]>,
    entries: Arc<[CompiledConstant]>,
    function_indices: Box<[(ExecutableId, u32)]>,
    number_indices: Box<[(Span, u32)]>,
    string_indices: Box<[(Span, CompiledStringLocation)]>,
    property_atom_indices: Box<[(CompiledPropertyAtomKey, u32)]>,
    metadata_atom_indices: Box<[(CompiledMetadataAtomKey, u32)]>,
}

pub(in crate::lowering) enum CompiledConstantCandidate {
    Number {
        value: Binary64Constant,
        span: Span,
    },
    String {
        value: CompilerString,
        span: Span,
    },
    Function {
        executable: ExecutableId,
        span: Span,
    },
}

impl CompiledConstantCandidate {
    const fn order_key(&self) -> (u32, u32, u8) {
        match self {
            Self::Number { span, .. } => (span.start, span.end, 0),
            Self::String { span, .. } => (span.start, span.end, 1),
            Self::Function { span, .. } => (span.start, span.end, 2),
        }
    }
}

pub(in crate::lowering) struct CompiledConstantPoolInput<'tree> {
    pub(in crate::lowering) children: &'tree [ExecutableId],
    pub(in crate::lowering) constant_candidates: Vec<CompiledConstantCandidate>,
    pub(in crate::lowering) atom_candidates: Vec<CompiledAtomCandidate>,
    pub(in crate::lowering) metadata_atom_candidates: Vec<CompiledMetadataAtomCandidate>,
}

#[derive(Clone, Copy)]
pub(in crate::lowering) enum CompiledStringLocation {
    Constant(u32),
    Atom(u32),
}

struct FrozenConstantCandidates {
    entries: Vec<CompiledConstant>,
    function_indices: Vec<(ExecutableId, u32)>,
    number_indices: Vec<(Span, u32)>,
    string_indices: Vec<(Span, CompiledStringLocation)>,
    property_atom_indices: Vec<(CompiledPropertyAtomKey, u32)>,
}

fn freeze_constant_candidates(
    children: &[ExecutableId],
    candidates: Vec<CompiledConstantCandidate>,
    string_capacity: usize,
) -> Result<FrozenConstantCandidates, LeafCompilationError> {
    let mut frozen = FrozenConstantCandidates {
        entries: Vec::with_capacity(candidates.len()),
        function_indices: Vec::with_capacity(children.len()),
        number_indices: Vec::with_capacity(candidates.len().checked_sub(children.len()).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "constant candidates include every direct child",
                span: None,
            },
        )?),
        string_indices: Vec::with_capacity(string_capacity),
        property_atom_indices: Vec::with_capacity(string_capacity),
    };
    for (index, candidate) in candidates.into_iter().enumerate() {
        let index = u32::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "constant pool entries",
        })?;
        match candidate {
            CompiledConstantCandidate::Number { value, span } => {
                frozen
                    .entries
                    .push(CompiledConstant::Value(CompilerConstantValue::Number(
                        value,
                    )));
                frozen.number_indices.push((span, index));
            }
            CompiledConstantCandidate::String { value, span } => {
                if value.is_empty() || !value.is_tagged_integer_atom() {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "string value constants are nonempty tagged-integer spellings",
                        span: Some(span),
                    });
                }
                frozen
                    .entries
                    .push(CompiledConstant::Value(CompilerConstantValue::String(
                        value,
                    )));
                frozen
                    .string_indices
                    .push((span, CompiledStringLocation::Constant(index)));
            }
            CompiledConstantCandidate::Function { executable, span } => {
                if children.binary_search(&executable).is_err() {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "constant-pool function is a direct child",
                        span: Some(span),
                    });
                }
                frozen.function_indices.push((executable, index));
                frozen
                    .entries
                    .push(CompiledConstant::Function(CompiledFunctionConstant {
                        executable,
                    }));
            }
        }
    }
    Ok(frozen)
}

fn validate_frozen_constant_candidates(
    children: &[ExecutableId],
    expected_count: u32,
    frozen: &mut FrozenConstantCandidates,
) -> Result<(), LeafCompilationError> {
    if u32::try_from(frozen.entries.len()) != Ok(expected_count) {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "constant-pool candidate count remains stable",
            span: None,
        });
    }
    frozen
        .function_indices
        .sort_unstable_by_key(|(executable, _)| *executable);
    if frozen.function_indices.len() != children.len()
        || !frozen
            .function_indices
            .iter()
            .map(|(executable, _)| *executable)
            .eq(children.iter().copied())
    {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "constant pool owns every direct child exactly once",
            span: None,
        });
    }
    frozen
        .string_indices
        .sort_unstable_by_key(|(span, _)| (span.start, span.end));
    if let Some(span) = frozen
        .string_indices
        .windows(2)
        .find_map(|pair| (pair[0].0 == pair[1].0).then_some(pair[0].0))
    {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "runtime string literal spans are unique within a function",
            span: Some(span),
        });
    }
    frozen
        .property_atom_indices
        .sort_unstable_by_key(|(key, _)| key.order_key());
    if let Some(key) = frozen
        .property_atom_indices
        .windows(2)
        .find_map(|pair| (pair[0].0 == pair[1].0).then_some(pair[0].0))
    {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "static property lookup keys are unique within a function",
            span: Some(key.span()),
        });
    }
    Ok(())
}

impl CompiledConstantPool {
    pub(in crate::lowering) fn new(
        input: CompiledConstantPoolInput<'_>,
    ) -> Result<Self, LeafCompilationError> {
        let CompiledConstantPoolInput {
            children,
            mut constant_candidates,
            mut atom_candidates,
            mut metadata_atom_candidates,
        } = input;
        constant_candidates.sort_unstable_by_key(CompiledConstantCandidate::order_key);
        atom_candidates.sort_unstable_by_key(CompiledAtomCandidate::order_key);
        metadata_atom_candidates.sort_unstable_by_key(CompiledMetadataAtomCandidate::order_key);
        let candidates = constant_candidates;
        let count = checked_function_entry_count(candidates.len(), "constant pool entries")?;
        if children.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "direct child executables are strictly ordered",
                span: None,
            });
        }
        let string_capacity = candidates.len().checked_add(atom_candidates.len()).ok_or(
            LeafCompilationError::CapacityExceeded {
                domain: "string literal occurrences",
            },
        )?;
        let mut frozen = freeze_constant_candidates(children, candidates, string_capacity)?;
        let (mut atoms, mut atom_interner) = freeze_atom_candidates(
            atom_candidates,
            &mut frozen.string_indices,
            &mut frozen.property_atom_indices,
        )?;
        let metadata_atom_indices = freeze_metadata_atom_candidates(
            metadata_atom_candidates,
            &mut atoms,
            &mut atom_interner,
        )?;
        validate_frozen_constant_candidates(children, count, &mut frozen)?;
        Ok(Self {
            atoms: atoms.into(),
            entries: frozen.entries.into(),
            function_indices: frozen.function_indices.into_boxed_slice(),
            number_indices: frozen.number_indices.into_boxed_slice(),
            string_indices: frozen.string_indices.into_boxed_slice(),
            property_atom_indices: frozen.property_atom_indices.into_boxed_slice(),
            metadata_atom_indices: metadata_atom_indices.into_boxed_slice(),
        })
    }

    pub(in crate::lowering) fn atoms(&self) -> &Arc<[CompilerAtom]> {
        &self.atoms
    }

    pub(in crate::lowering) fn entries(&self) -> &Arc<[CompiledConstant]> {
        &self.entries
    }

    pub(in crate::lowering) fn metadata_atom_index(
        &self,
        key: CompiledMetadataAtomKey,
    ) -> Result<AtomPoolIndex, LeafCompilationError> {
        let position = self
            .metadata_atom_indices
            .binary_search_by_key(&key, |(candidate, _)| *candidate)
            .map_err(|_| LeafCompilationError::SemanticInvariant {
                invariant: "compiled metadata field has a function-local atom",
                span: None,
            })?;
        Ok(AtomPoolIndex::new(self.metadata_atom_indices[position].1))
    }

    pub(in crate::lowering) fn plan_number(
        &self,
        value: f64,
        span: Span,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        let position = self
            .number_indices
            .binary_search_by_key(&(span.start, span.end), |(candidate, _)| {
                (candidate.start, candidate.end)
            })
            .map_err(|_| LeafCompilationError::SemanticInvariant {
                invariant: "non-integer numeric literal has one constant-pool entry",
                span: Some(span),
            })?;
        let index = self.number_indices[position].1;
        let Some(CompiledConstant::Value(CompilerConstantValue::Number(actual))) =
            usize::try_from(index)
                .ok()
                .and_then(|index| self.entries.get(index))
        else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "numeric constant index resolves to its binary64 payload",
                span: Some(span),
            });
        };
        if *actual != Binary64Constant::from_f64(value) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "numeric constant retains its parsed binary64 payload",
                span: Some(span),
            });
        }
        let (opcode, operands) = match u8::try_from(index) {
            Ok(index) => (FinalOpcode::PushConst8, Operands::Const8(index)),
            Err(_) => (FinalOpcode::PushConst, Operands::Const(index)),
        };
        Ok(PlannedInstruction::new(opcode, operands, span))
    }

    pub(in crate::lowering) fn plan_string(
        &self,
        span: Span,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        let position = self
            .string_indices
            .binary_search_by_key(&(span.start, span.end), |(candidate, _)| {
                (candidate.start, candidate.end)
            })
            .map_err(|_| LeafCompilationError::SemanticInvariant {
                invariant: "nonempty runtime string has one pool location",
                span: Some(span),
            })?;
        let instruction = match self.string_indices[position].1 {
            CompiledStringLocation::Constant(index) => {
                let Some(CompiledConstant::Value(CompilerConstantValue::String(value))) =
                    usize::try_from(index)
                        .ok()
                        .and_then(|index| self.entries.get(index))
                else {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "string constant index resolves to an exact string payload",
                        span: Some(span),
                    });
                };
                if !value.is_tagged_integer_atom() {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "string constant retains its tagged-integer spelling",
                        span: Some(span),
                    });
                }
                match u8::try_from(index) {
                    Ok(index) => (FinalOpcode::PushConst8, Operands::Const8(index)),
                    Err(_) => (FinalOpcode::PushConst, Operands::Const(index)),
                }
            }
            CompiledStringLocation::Atom(index) => {
                let Some(atom) = usize::try_from(index)
                    .ok()
                    .and_then(|index| self.atoms.get(index))
                else {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "string atom index resolves to an exact atom payload",
                        span: Some(span),
                    });
                };
                if atom.string().is_empty() || atom.string().is_tagged_integer_atom() {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "string atom retains its non-tagged spelling",
                        span: Some(span),
                    });
                }
                (
                    FinalOpcode::PushAtomValue,
                    Operands::Atom(AtomPoolIndex::new(index)),
                )
            }
        };
        Ok(PlannedInstruction::new(instruction.0, instruction.1, span))
    }

    pub(in crate::lowering) fn property_atom_index(
        &self,
        span: Span,
    ) -> Result<AtomPoolIndex, LeafCompilationError> {
        self.property_atom_index_for(CompiledPropertyAtomKey::Source(span), span)
    }

    pub(in crate::lowering) fn array_index_atom_index(
        &self,
        array: Span,
        index: u32,
        span: Span,
    ) -> Result<AtomPoolIndex, LeafCompilationError> {
        self.property_atom_index_for(CompiledPropertyAtomKey::ArrayIndex { array, index }, span)
    }

    pub(in crate::lowering) fn array_length_atom_index(
        &self,
        array: Span,
        span: Span,
    ) -> Result<AtomPoolIndex, LeafCompilationError> {
        self.property_atom_index_for(CompiledPropertyAtomKey::ArrayLength { array }, span)
    }

    fn property_atom_index_for(
        &self,
        key: CompiledPropertyAtomKey,
        span: Span,
    ) -> Result<AtomPoolIndex, LeafCompilationError> {
        let position = self
            .property_atom_indices
            .binary_search_by_key(&key.order_key(), |(candidate, _)| candidate.order_key())
            .map_err(|_| LeafCompilationError::SemanticInvariant {
                invariant: "static property has one function-local atom",
                span: Some(span),
            })?;
        Ok(AtomPoolIndex::new(self.property_atom_indices[position].1))
    }

    pub(in crate::lowering) fn function_index(
        &self,
        executable: ExecutableId,
    ) -> Result<u32, LeafCompilationError> {
        self.function_indices
            .binary_search_by_key(&executable, |(candidate, _)| *candidate)
            .ok()
            .map(|position| self.function_indices[position].1)
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "direct child executable has a constant-pool index",
                span: None,
            })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use quickjs_bytecode::{Binary64Constant, CompilerConstantValue, CompilerString};
    use quickjs_frontend::{
        CompilationGoal, FrontendOptions, GlobalScriptGoal, Span, with_parsed_program,
    };

    use crate::lowering::atoms::{CompiledAtomCandidate, CompiledAtomPurpose};
    use crate::lowering::{CompilationContext, CompiledConstant, LeafCompilationError};

    use super::{CompiledConstantCandidate, CompiledConstantPool, CompiledConstantPoolInput};

    fn string(code_units: &[u16]) -> CompilerString {
        CompilerString::try_from_code_units(Arc::from(code_units)).expect("compiler string")
    }

    #[test]
    fn constructor_freezes_source_order_exact_number_bits_and_utf16_atoms() {
        let tagged = string(&[b'1'.into(), b'2'.into(), b'3'.into()]);
        let wide = string(&[0xd800, b'a'.into()]);
        let pool = CompiledConstantPool::new(CompiledConstantPoolInput {
            children: &[],
            constant_candidates: vec![
                CompiledConstantCandidate::Number {
                    value: Binary64Constant::from_bits(0x8000_0000_0000_0000),
                    span: Span::new(20, 22),
                },
                CompiledConstantCandidate::String {
                    value: tagged.clone(),
                    span: Span::new(2, 5),
                },
            ],
            atom_candidates: vec![CompiledAtomCandidate {
                value: wide.clone(),
                span: Span::new(10, 14),
                purpose: CompiledAtomPurpose::RuntimeString,
                property_key: None,
            }],
            metadata_atom_candidates: Vec::new(),
        })
        .expect("frozen pool");

        assert_eq!(
            pool.entries().as_ref(),
            [
                CompiledConstant::Value(CompilerConstantValue::String(tagged)),
                CompiledConstant::Value(CompilerConstantValue::Number(
                    Binary64Constant::from_bits(0x8000_0000_0000_0000),
                )),
            ]
        );
        assert_eq!(
            pool.atoms()[0].string().code_units().collect::<Vec<_>>(),
            [0xd800, u16::from(b'a')]
        );
    }

    #[test]
    fn constructor_rejects_a_function_candidate_outside_the_child_domain() {
        with_parsed_program(
            "function child(){}",
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage plan");
                let child = context
                    .storage_plan()
                    .executables()
                    .iter()
                    .find(|executable| executable.name() == Some("child"))
                    .expect("child executable")
                    .id();
                let result = CompiledConstantPool::new(CompiledConstantPoolInput {
                    children: &[],
                    constant_candidates: vec![CompiledConstantCandidate::Function {
                        executable: child,
                        span: Span::new(4, 8),
                    }],
                    atom_candidates: Vec::new(),
                    metadata_atom_candidates: Vec::new(),
                });
                let Err(error) = result else {
                    panic!("foreign function candidate must be rejected");
                };
                assert!(matches!(
                    error,
                    LeafCompilationError::SemanticInvariant {
                        invariant: "constant-pool function is a direct child",
                        span: Some(span),
                    } if span == Span::new(4, 8)
                ));
            },
        )
        .expect("front-end acceptance");
    }
}
