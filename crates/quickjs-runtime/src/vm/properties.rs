/*
 * JavaScript bytecode execution and closure semantics derived from QuickJS.
 *
 * Copyright (c) 2017-2018 Fabrice Bellard
 * Copyright (c) 2017-2018 Charlie Gordon
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! Property operands and ordinary object read, write, and definition semantics.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) trait AtomDescription {
    fn description(&self) -> Option<&JsString>;
}

impl AtomDescription for crate::Atom {
    fn description(&self) -> Option<&JsString> {
        crate::Atom::description(self)
    }
}

pub(super) struct StaticPropertyOperand {
    pub(super) key: PropertyKey,
    pub(super) name: JsString,
}

#[derive(Clone, Copy)]
pub(super) enum DefineMethodKind {
    Method,
    Getter,
    Setter,
}

pub(super) struct DefineMethodOperand {
    pub(super) property: StaticPropertyOperand,
    pub(super) kind: DefineMethodKind,
    pub(super) enumerable: bool,
}

pub(super) struct DefineMethodComputedOperand {
    pub(super) kind: DefineMethodKind,
    pub(super) enumerable: bool,
}

pub(super) struct GlobalReferenceOperand {
    binding: RealmGlobalBindingId,
    realm: RealmId,
    object: ObjectId,
    pub(super) key: PropertyKey,
    pub(super) name: JsString,
}

pub(super) enum PropertyReadOutcome {
    Value(StoredValue),
    Getter {
        function: FunctionId,
        receiver: StoredValue,
    },
    Failed(PropertyFailure),
}

pub(super) enum PropertyWriteOutcome {
    Complete,
    Setter {
        function: FunctionId,
        receiver: StoredValue,
        value: StoredValue,
    },
    Failed(PropertyFailure),
}

pub(super) enum PropertyDefinitionOutcome {
    Complete,
    Failed(PropertyFailure),
}

pub(super) enum RealmGlobalReadOutcome {
    Value(StoredValue),
    Missing,
}

pub(super) enum RealmGlobalWriteOutcome {
    Complete,
    Missing,
    Property(PropertyFailure),
}

#[derive(Clone, Copy)]
pub(super) enum PropertyFailure {
    ReadNull,
    ReadUndefined,
    WriteNull,
    WriteUndefined,
    NotObject,
    ReadOnly,
    NoSetter,
    NotConfigurable,
    NonExtensible,
    /// `delete` reached `ToObject(null)`.
    DeleteNull,
    /// `delete` reached `ToObject(undefined)`.
    DeleteUndefined,
    /// `delete` refused a non-configurable property in strict code.
    NotDeletable,
}

pub(super) fn static_property_operand(
    runtime: &Runtime,
    frame: &Frame,
    operands: Operands,
) -> Result<StaticPropertyOperand, EngineFault> {
    let Operands::Atom(index) = operands else {
        return Err(EngineFault::MissingPoolEntry {
            pool: "property atom",
            index: u32::MAX,
        });
    };
    static_property_at(runtime, frame, index)
}

fn static_property_at(
    runtime: &Runtime,
    frame: &Frame,
    index: quickjs_bytecode::AtomPoolIndex,
) -> Result<StaticPropertyOperand, EngineFault> {
    let atom = installed_template(runtime, frame.code, frame.template)?
        .atoms
        .get(index.get() as usize)
        .cloned()
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "property atom",
            index: index.get(),
        })?;
    let name = atom
        .description()
        .cloned()
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "property atom description",
            index: index.get(),
        })?;
    Ok(StaticPropertyOperand {
        key: ArrayIndex::parse_property_key(&name).map_or_else(
            || PropertyKey::from_validated_atom(atom),
            PropertyKey::from_index,
        ),
        name,
    })
}

pub(super) fn define_method_operand(
    runtime: &Runtime,
    frame: &Frame,
    operands: Operands,
) -> Result<DefineMethodOperand, EngineFault> {
    let Operands::AtomU8 { atom, value } = operands else {
        return Err(EngineFault::MissingPoolEntry {
            pool: "method property atom",
            index: u32::MAX,
        });
    };
    if value & !0b111 != 0 || value & 0b11 == 0b11 {
        return Err(EngineFault::RuntimeInvariant {
            message: "verified define_method flags are invalid",
        });
    }
    let kind = match value & 0b11 {
        0 => DefineMethodKind::Method,
        1 => DefineMethodKind::Getter,
        2 => DefineMethodKind::Setter,
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "verified define_method kind is invalid",
            });
        }
    };
    Ok(DefineMethodOperand {
        property: static_property_at(runtime, frame, atom)?,
        kind,
        enumerable: value & 0b100 != 0,
    })
}

pub(super) fn define_method_computed_operand(
    operands: Operands,
) -> Result<DefineMethodComputedOperand, EngineFault> {
    let Operands::U8(value) = operands else {
        return Err(EngineFault::RuntimeInvariant {
            message: "verified define_method_computed operand is not u8",
        });
    };
    let kind = match value {
        4 => DefineMethodKind::Method,
        5 => DefineMethodKind::Getter,
        6 => DefineMethodKind::Setter,
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "verified define_method_computed flags are invalid",
            });
        }
    };
    Ok(DefineMethodComputedOperand {
        kind,
        enumerable: true,
    })
}

pub(super) fn global_reference_operand(
    runtime: &Runtime,
    frame: &Frame,
    index: u32,
) -> Result<GlobalReferenceOperand, EngineFault> {
    let binding = *frame
        .environment
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "realm global environment",
            index,
        })?;
    let EnvironmentBinding::RealmGlobal(global) = binding else {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        });
    };
    let record = runtime
        .global_bindings
        .get(global)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "realm global binding",
            index: global.index(),
            generation: global.generation(),
        })?;
    let realm = code(runtime, frame.code)?.realm;
    if record.realm != realm {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        });
    }
    let name = record
        .name
        .description()
        .cloned()
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "realm global atom description",
            index,
        })?;
    Ok(GlobalReferenceOperand {
        binding: global,
        realm,
        object: runtime.realm_global_object(realm)?,
        key: PropertyKey::from_validated_atom(record.name.clone()),
        name,
    })
}

pub(super) fn read_realm_global(
    runtime: &Runtime,
    global: &GlobalReferenceOperand,
) -> Result<RealmGlobalReadOutcome, ExecutionError> {
    let binding =
        runtime
            .global_bindings
            .get(global.binding)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "realm global binding",
                index: global.binding.index(),
                generation: global.binding.generation(),
            })?;
    match binding.state {
        RealmGlobalBindingState::Unresolved | RealmGlobalBindingState::Object => {
            read_heap_property_if_present(
                runtime,
                HeapReference::Object(global.object),
                &global.key,
            )
            .map(|value| {
                value.map_or(
                    RealmGlobalReadOutcome::Missing,
                    RealmGlobalReadOutcome::Value,
                )
            })
        }
    }
}

pub(super) fn write_realm_global(
    runtime: &mut Runtime,
    global: GlobalReferenceOperand,
    value: StoredValue,
    strict: bool,
    execution_budget: &mut ExecutionBudget,
) -> Result<RealmGlobalWriteOutcome, ExecutionError> {
    let state = runtime
        .global_bindings
        .get(global.binding)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "realm global binding",
            index: global.binding.index(),
            generation: global.binding.generation(),
        })?
        .state;
    match state {
        RealmGlobalBindingState::Unresolved => {
            let present = read_heap_property_if_present(
                runtime,
                HeapReference::Object(global.object),
                &global.key,
            )?
            .is_some();
            if !present && strict {
                return Ok(RealmGlobalWriteOutcome::Missing);
            }
            let base = StoredValue::Object(global.object);
            Ok(
                match write_static_property(
                    runtime,
                    global.realm,
                    &base,
                    global.key,
                    value,
                    strict,
                    execution_budget,
                )? {
                    PropertyWriteOutcome::Complete => RealmGlobalWriteOutcome::Complete,
                    PropertyWriteOutcome::Setter { .. } => {
                        return Err(EngineFault::UnsupportedAccessorWrite {
                            operation: "realm-global property write",
                        }
                        .into());
                    }
                    PropertyWriteOutcome::Failed(failure) => {
                        RealmGlobalWriteOutcome::Property(failure)
                    }
                },
            )
        }
        RealmGlobalBindingState::Object => {
            let base = StoredValue::Object(global.object);
            Ok(
                match write_static_property(
                    runtime,
                    global.realm,
                    &base,
                    global.key,
                    value,
                    strict,
                    execution_budget,
                )? {
                    PropertyWriteOutcome::Complete => RealmGlobalWriteOutcome::Complete,
                    PropertyWriteOutcome::Setter { .. } => {
                        return Err(EngineFault::UnsupportedAccessorWrite {
                            operation: "realm-global property write",
                        }
                        .into());
                    }
                    PropertyWriteOutcome::Failed(failure) => {
                        RealmGlobalWriteOutcome::Property(failure)
                    }
                },
            )
        }
    }
}

pub(super) fn read_static_property(
    runtime: &Runtime,
    realm: RealmId,
    base: &StoredValue,
    key: &PropertyKey,
) -> Result<PropertyReadOutcome, ExecutionError> {
    Ok(match base {
        StoredValue::Undefined => PropertyReadOutcome::Failed(PropertyFailure::ReadUndefined),
        StoredValue::Null => PropertyReadOutcome::Failed(PropertyFailure::ReadNull),
        StoredValue::Boolean(_) => read_heap_property_for_receiver(
            runtime,
            HeapReference::Object(runtime.realm_boolean_prototype(realm)?),
            base.duplicate(),
            key,
        )?,
        StoredValue::Number(_) => read_heap_property_for_receiver(
            runtime,
            HeapReference::Object(runtime.realm_number_prototype(realm)?),
            base.duplicate(),
            key,
        )?,
        StoredValue::BigInt(_) => read_heap_property_for_receiver(
            runtime,
            HeapReference::Object(runtime.realm_bigint_prototype(realm)?),
            base.duplicate(),
            key,
        )?,
        StoredValue::Symbol(_) => read_heap_property_for_receiver(
            runtime,
            HeapReference::Object(runtime.realm_symbol_prototype(realm)?),
            base.duplicate(),
            key,
        )?,
        StoredValue::String(value) => {
            if let Some(index) = key.as_index()
                && index.get() < value.len()
            {
                PropertyReadOutcome::Value(StoredValue::String(
                    value.slice(index.get()..index.get().saturating_add(1))?,
                ))
            } else if key.as_atom().and_then(crate::Atom::predefined_atom)
                == Some(PredefinedAtom::Length)
            {
                PropertyReadOutcome::Value(StoredValue::Number(JsNumber::from_f64(f64::from(
                    value.len(),
                ))))
            } else {
                read_heap_property_for_receiver(
                    runtime,
                    HeapReference::Object(runtime.realm_string_prototype(realm)?),
                    base.duplicate(),
                    key,
                )?
            }
        }
        StoredValue::Function(function) => read_heap_property_for_receiver(
            runtime,
            HeapReference::Function(*function),
            base.duplicate(),
            key,
        )?,
        StoredValue::Object(object) => read_heap_property_for_receiver(
            runtime,
            HeapReference::Object(*object),
            base.duplicate(),
            key,
        )?,
    })
}

pub(super) fn read_heap_property_for_receiver(
    runtime: &Runtime,
    reference: HeapReference,
    receiver: StoredValue,
    key: &PropertyKey,
) -> Result<PropertyReadOutcome, ExecutionError> {
    Ok(match lookup_heap_property(runtime, Some(reference), key)? {
        None => PropertyReadOutcome::Value(StoredValue::Undefined),
        Some(OwnProperty::Data { value, .. }) => PropertyReadOutcome::Value(value),
        Some(OwnProperty::Accessor {
            getter: Some(function),
            ..
        }) => PropertyReadOutcome::Getter { function, receiver },
        Some(OwnProperty::Accessor { getter: None, .. }) => {
            PropertyReadOutcome::Value(StoredValue::Undefined)
        }
    })
}

pub(super) fn read_heap_property(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<StoredValue, ExecutionError> {
    Ok(read_heap_property_if_present(runtime, reference, key)?.unwrap_or(StoredValue::Undefined))
}

fn read_heap_property_if_present(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<Option<StoredValue>, ExecutionError> {
    match lookup_heap_property(runtime, Some(reference), key)? {
        None => Ok(None),
        Some(OwnProperty::Data { value, .. }) => Ok(Some(value)),
        Some(OwnProperty::Accessor { .. }) => Err(EngineFault::UnsupportedAccessorRead {
            operation: "synchronous property read",
        }
        .into()),
    }
}

/// Applies ECMAScript `HasProperty`, walking the prototype chain.
///
/// This is what lets `Array.prototype.indexOf` skip a hole: a missing index is
/// not merely `undefined`, so `[1,,3].indexOf(undefined)` is `-1` while
/// `includes`, which reads instead of testing, answers `true`.
pub(super) fn has_property(
    runtime: &Runtime,
    realm: RealmId,
    base: &StoredValue,
    key: &PropertyKey,
) -> Result<bool, ExecutionError> {
    let reference = match base {
        // A primitive receiver tests against its wrapper prototype, matching the
        // read path; `undefined` and `null` never reach here because the caller
        // rejects them first.
        StoredValue::Undefined | StoredValue::Null => return Ok(false),
        StoredValue::Boolean(_) => HeapReference::Object(runtime.realm_boolean_prototype(realm)?),
        StoredValue::Number(_) => HeapReference::Object(runtime.realm_number_prototype(realm)?),
        StoredValue::BigInt(_) => HeapReference::Object(runtime.realm_bigint_prototype(realm)?),
        StoredValue::Symbol(_) => HeapReference::Object(runtime.realm_symbol_prototype(realm)?),
        StoredValue::String(text) => {
            // A String's own index and `length` properties are exotic rather
            // than stored, so they are answered before the prototype walk.
            if let Some(index) = key.as_index() {
                return Ok(u64::from(index.get()) < u64::from(text.len()));
            }
            if key.as_atom().and_then(crate::Atom::predefined_atom) == Some(PredefinedAtom::Length)
            {
                return Ok(true);
            }
            HeapReference::Object(runtime.realm_string_prototype(realm)?)
        }
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
    };
    Ok(lookup_heap_property(runtime, Some(reference), key)?.is_some())
}

pub(super) fn lookup_heap_property(
    runtime: &Runtime,
    mut current: Option<HeapReference>,
    key: &PropertyKey,
) -> Result<Option<OwnProperty>, ExecutionError> {
    let mut remaining = runtime
        .functions
        .len()
        .saturating_add(runtime.objects.len())
        .saturating_add(1);
    while let Some(reference) = current {
        if remaining == 0 {
            return Err(EngineFault::RuntimeInvariant {
                message: "ordinary prototype chain contains a cycle",
            }
            .into());
        }
        remaining -= 1;
        if let Some(property) = heap_own_property(runtime, reference, key)? {
            return Ok(Some(property));
        }
        current = runtime.object_record(reference)?.prototype();
    }
    Ok(None)
}

pub(super) fn heap_own_property(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<Option<OwnProperty>, ExecutionError> {
    if let Some(property) = string_exotic_index_property(runtime, reference, key)? {
        return Ok(Some(property));
    }
    let Some(mut property) = runtime.object_record(reference)?.own_property(key) else {
        return Ok(None);
    };
    if let HeapReference::Object(object) = reference
        && let Some(value) = runtime.mapped_arguments_value(object, key)?
    {
        let OwnProperty::Data {
            layout: _,
            value: stored,
        } = &mut property
        else {
            return Err(EngineFault::RuntimeInvariant {
                message: "mapped arguments property remains a data property",
            }
            .into());
        };
        *stored = value;
    }
    Ok(Some(property))
}

pub(super) fn string_exotic_index_property(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<Option<OwnProperty>, ExecutionError> {
    let HeapReference::Object(object) = reference else {
        return Ok(None);
    };
    let Some(index) = key.as_index() else {
        return Ok(None);
    };
    let Some(unit) = runtime.boxed_string_code_unit_at(object, index.get())? else {
        return Ok(None);
    };
    Ok(Some(OwnProperty::Data {
        layout: PropertyLayout::data(false, true, false),
        value: StoredValue::String(JsString::from_code_units([unit])?),
    }))
}

fn string_exotic_index_is_present(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<bool, ExecutionError> {
    let HeapReference::Object(object) = reference else {
        return Ok(false);
    };
    let Some(index) = key.as_index() else {
        return Ok(false);
    };
    Ok(runtime
        .boxed_string_code_unit_at(object, index.get())?
        .is_some())
}

fn inherited_property(
    runtime: &Runtime,
    current: Option<HeapReference>,
    key: &PropertyKey,
) -> Result<Option<OwnProperty>, ExecutionError> {
    lookup_heap_property(runtime, current, key)
}

/// The observable result of the `delete` operator.
pub(super) enum PropertyDeleteOutcome {
    /// The property is gone, or never existed.
    Deleted,
    /// The property exists and refused removal. Sloppy code evaluates to
    /// `false`; strict code throws.
    Refused,
    /// The base could not be converted to an object.
    Failed(PropertyFailure),
}

/// Applies the `delete` operator to one already-converted property key.
///
/// This is `JS_DeleteProperty` (`quickjs.c:10920`): the base is coerced with
/// `ToObject`, so `null` and `undefined` throw while other primitives delete
/// from a throwaway wrapper and report success. A `String` wrapper's own index
/// properties are non-configurable, so deleting one in range refuses while an
/// out-of-range index succeeds. An array's cached `length` is deliberately
/// left alone: `[[Delete]]` creates a hole rather than shortening the array.
pub(super) fn delete_static_property(
    runtime: &mut Runtime,
    base: &StoredValue,
    key: &PropertyKey,
) -> Result<PropertyDeleteOutcome, ExecutionError> {
    let reference = match base {
        StoredValue::Null => {
            return Ok(PropertyDeleteOutcome::Failed(PropertyFailure::DeleteNull));
        }
        StoredValue::Undefined => {
            return Ok(PropertyDeleteOutcome::Failed(
                PropertyFailure::DeleteUndefined,
            ));
        }
        // A primitive base is boxed by `ToObject`, and the wrapper is
        // discarded immediately. Only a `String` wrapper has own properties,
        // and its indices are non-configurable.
        StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::Symbol(_) => {
            return Ok(PropertyDeleteOutcome::Deleted);
        }
        StoredValue::String(value) => {
            let refused = key
                .as_index()
                .is_some_and(|index| index.get() < value.len());
            return Ok(if refused {
                PropertyDeleteOutcome::Refused
            } else {
                PropertyDeleteOutcome::Deleted
            });
        }
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
    };
    if string_exotic_index_is_present(runtime, reference, key)? {
        return Ok(PropertyDeleteOutcome::Refused);
    }
    Ok(match runtime.delete_own_property(reference, key)? {
        PropertyDeletion::Missing | PropertyDeletion::Deleted => PropertyDeleteOutcome::Deleted,
        PropertyDeletion::NotConfigurable => PropertyDeleteOutcome::Refused,
    })
}

pub(super) fn is_array_length_target(
    runtime: &Runtime,
    base: &StoredValue,
    key: &PropertyKey,
) -> Result<bool, ExecutionError> {
    if key.as_atom().and_then(crate::Atom::predefined_atom) != Some(PredefinedAtom::Length) {
        return Ok(false);
    }
    let StoredValue::Object(object) = base else {
        return Ok(false);
    };
    Ok(runtime.array_length(*object)?.is_some())
}

pub(super) fn array_length_write_target(
    base: StoredValue,
    name: JsString,
    strict: bool,
    reflect: bool,
    value: &StoredValue,
) -> OperatorPrimitiveTarget {
    let original = (!matches!(
        value,
        StoredValue::Null | StoredValue::Boolean(_) | StoredValue::Number(_)
    ))
    .then(|| value.duplicate());
    OperatorPrimitiveTarget::ArrayLengthWrite(ArrayLengthWriteState {
        base,
        name,
        strict,
        reflect,
        definition: None,
        original,
        first_length: None,
    })
}

pub(super) fn array_length_define_target(
    base: StoredValue,
    name: JsString,
    value: &StoredValue,
    definition: ArrayLengthDefinition,
) -> OperatorPrimitiveTarget {
    let original = (!matches!(
        value,
        StoredValue::Null | StoredValue::Boolean(_) | StoredValue::Number(_)
    ))
    .then(|| value.duplicate());
    OperatorPrimitiveTarget::ArrayLengthWrite(ArrayLengthWriteState {
        base,
        name,
        strict: false,
        reflect: false,
        definition: Some(definition),
        original,
        first_length: None,
    })
}

fn array_define_write_outcome(outcome: ArrayDefineOutcome, strict: bool) -> PropertyWriteOutcome {
    match outcome {
        ArrayDefineOutcome::Complete => PropertyWriteOutcome::Complete,
        ArrayDefineOutcome::ReadOnlyLength if strict => {
            PropertyWriteOutcome::Failed(PropertyFailure::ReadOnly)
        }
        ArrayDefineOutcome::NonExtensible if strict => {
            PropertyWriteOutcome::Failed(PropertyFailure::NonExtensible)
        }
        ArrayDefineOutcome::ReadOnlyLength | ArrayDefineOutcome::NonExtensible => {
            PropertyWriteOutcome::Complete
        }
    }
}

fn write_primitive_property(
    runtime: &Runtime,
    prototype: HeapReference,
    receiver: &StoredValue,
    key: &PropertyKey,
    value: StoredValue,
    strict: bool,
) -> Result<PropertyWriteOutcome, ExecutionError> {
    if let Some(inherited) = inherited_property(runtime, Some(prototype), key)? {
        match inherited {
            OwnProperty::Accessor { setter, .. } => {
                return Ok(match setter {
                    Some(function) => PropertyWriteOutcome::Setter {
                        function,
                        receiver: receiver.duplicate(),
                        value,
                    },
                    None if strict => PropertyWriteOutcome::Failed(PropertyFailure::NoSetter),
                    None => PropertyWriteOutcome::Complete,
                });
            }
            OwnProperty::Data { layout, .. } if layout.writable() != Some(true) => {
                return Ok(if strict {
                    PropertyWriteOutcome::Failed(PropertyFailure::ReadOnly)
                } else {
                    PropertyWriteOutcome::Complete
                });
            }
            OwnProperty::Data { .. } => {}
        }
    }
    Ok(if strict {
        PropertyWriteOutcome::Failed(PropertyFailure::NotObject)
    } else {
        PropertyWriteOutcome::Complete
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "ordinary write semantics audit every primitive, own, inherited, accessor, and extensibility branch"
)]
pub(super) fn write_static_property(
    runtime: &mut Runtime,
    realm: RealmId,
    base: &StoredValue,
    key: PropertyKey,
    value: StoredValue,
    strict: bool,
    execution_budget: &mut ExecutionBudget,
) -> Result<PropertyWriteOutcome, ExecutionError> {
    let reference = match base {
        StoredValue::Undefined => {
            return Ok(PropertyWriteOutcome::Failed(
                PropertyFailure::WriteUndefined,
            ));
        }
        StoredValue::Null => {
            return Ok(PropertyWriteOutcome::Failed(PropertyFailure::WriteNull));
        }
        StoredValue::Boolean(_) => {
            let prototype = runtime.realm_boolean_prototype(realm)?;
            return write_primitive_property(
                runtime,
                HeapReference::Object(prototype),
                base,
                &key,
                value,
                strict,
            );
        }
        StoredValue::BigInt(_) => {
            let prototype = runtime.realm_bigint_prototype(realm)?;
            return write_primitive_property(
                runtime,
                HeapReference::Object(prototype),
                base,
                &key,
                value,
                strict,
            );
        }
        StoredValue::Number(_) => {
            let prototype = runtime.realm_number_prototype(realm)?;
            return write_primitive_property(
                runtime,
                HeapReference::Object(prototype),
                base,
                &key,
                value,
                strict,
            );
        }
        StoredValue::String(_) => {
            let prototype = runtime.realm_string_prototype(realm)?;
            return write_primitive_property(
                runtime,
                HeapReference::Object(prototype),
                base,
                &key,
                value,
                strict,
            );
        }
        StoredValue::Symbol(_) => {
            return Ok(if strict {
                PropertyWriteOutcome::Failed(PropertyFailure::NotObject)
            } else {
                PropertyWriteOutcome::Complete
            });
        }
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
    };
    let mapped_cell = match reference {
        HeapReference::Object(object) => runtime.mapped_arguments_cell(object, &key)?,
        HeapReference::Function(_) => None,
    };
    let array = match reference {
        HeapReference::Object(object) if runtime.is_array_object(object)? => Some(object),
        HeapReference::Function(_) | HeapReference::Object(_) => None,
    };
    if array.is_some()
        && key.as_atom().and_then(crate::Atom::predefined_atom) == Some(PredefinedAtom::Length)
    {
        return Err(EngineFault::RuntimeInvariant {
            message: "array length write bypassed resumable numeric conversion",
        }
        .into());
    }

    if string_exotic_index_is_present(runtime, reference, &key)? {
        return Ok(if strict {
            PropertyWriteOutcome::Failed(PropertyFailure::ReadOnly)
        } else {
            PropertyWriteOutcome::Complete
        });
    }

    let (prototype, extensible) = {
        let record = runtime.object_record(reference)?;
        (record.prototype(), record.is_extensible())
    };
    let own = match array {
        Some(array) => runtime.array_own_property(array, &key)?,
        None => heap_own_property(runtime, reference, &key)?,
    };
    if let Some(own) = own {
        match own {
            OwnProperty::Data { layout, .. } => {
                if layout.writable() == Some(true) {
                    if let Some(array) = array {
                        let work = runtime.preview_array_define_data_property_work(array)?;
                        execution_budget.charge_instructions(work)?;
                        let outcome =
                            runtime.define_array_data_property(array, key, layout, value)?;
                        return Ok(array_define_write_outcome(outcome, strict));
                    }
                    if let Some(cell) = mapped_cell {
                        runtime.replace_mapped_arguments_cell_value(cell, value.duplicate())?;
                    }
                    let replaced = runtime
                        .object_record_mut(reference)?
                        .replace_existing_data(&key, value);
                    if !replaced {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "located own data property disappeared before its update",
                        }
                        .into());
                    }
                    runtime.collection_pending = true;
                    return Ok(PropertyWriteOutcome::Complete);
                }
                return Ok(if strict {
                    PropertyWriteOutcome::Failed(PropertyFailure::ReadOnly)
                } else {
                    PropertyWriteOutcome::Complete
                });
            }
            OwnProperty::Accessor { setter, .. } => {
                return Ok(match setter {
                    Some(function) => PropertyWriteOutcome::Setter {
                        function,
                        receiver: base.duplicate(),
                        value,
                    },
                    None if strict => PropertyWriteOutcome::Failed(PropertyFailure::NoSetter),
                    None => PropertyWriteOutcome::Complete,
                });
            }
        }
    }
    if let Some(inherited) = inherited_property(runtime, prototype, &key)? {
        match inherited {
            OwnProperty::Data { layout, .. } if layout.writable() != Some(true) => {
                return Ok(if strict {
                    PropertyWriteOutcome::Failed(PropertyFailure::ReadOnly)
                } else {
                    PropertyWriteOutcome::Complete
                });
            }
            OwnProperty::Data { .. } => {}
            OwnProperty::Accessor { setter, .. } => {
                return Ok(match setter {
                    Some(function) => PropertyWriteOutcome::Setter {
                        function,
                        receiver: base.duplicate(),
                        value,
                    },
                    None if strict => PropertyWriteOutcome::Failed(PropertyFailure::NoSetter),
                    None => PropertyWriteOutcome::Complete,
                });
            }
        }
    }
    if !extensible {
        return Ok(if strict {
            PropertyWriteOutcome::Failed(PropertyFailure::NonExtensible)
        } else {
            PropertyWriteOutcome::Complete
        });
    }
    if let Some(array) = array {
        let work = runtime.preview_array_define_data_property_work(array)?;
        execution_budget.charge_instructions(work)?;
        let outcome = runtime.define_array_data_property(
            array,
            key,
            PropertyLayout::data(true, true, true),
            value,
        )?;
        return Ok(array_define_write_outcome(outcome, strict));
    }
    runtime.append_data_property(
        reference,
        key,
        PropertyLayout::data(true, true, true),
        value,
    )?;
    Ok(PropertyWriteOutcome::Complete)
}

pub(super) fn define_static_property(
    runtime: &mut Runtime,
    base: &StoredValue,
    key: PropertyKey,
    value: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<PropertyWriteOutcome, ExecutionError> {
    let reference = match base {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Ok(PropertyWriteOutcome::Failed(PropertyFailure::NotObject));
        }
    };
    let mapped_cell = match reference {
        HeapReference::Object(object) => runtime.mapped_arguments_cell(object, &key)?,
        HeapReference::Function(_) => None,
    };
    if let HeapReference::Object(object) = reference
        && runtime.is_array_object(object)?
    {
        if key.as_atom().and_then(crate::Atom::predefined_atom) == Some(PredefinedAtom::Length) {
            return Ok(PropertyWriteOutcome::Failed(
                PropertyFailure::NotConfigurable,
            ));
        }
        let work = runtime.preview_array_define_data_property_work(object)?;
        execution_budget.charge_instructions(work)?;
        return Ok(
            match runtime.define_array_data_property(
                object,
                key,
                PropertyLayout::data(true, true, true),
                value,
            )? {
                ArrayDefineOutcome::Complete => PropertyWriteOutcome::Complete,
                ArrayDefineOutcome::ReadOnlyLength => {
                    PropertyWriteOutcome::Failed(PropertyFailure::ReadOnly)
                }
                ArrayDefineOutcome::NonExtensible => {
                    PropertyWriteOutcome::Failed(PropertyFailure::NonExtensible)
                }
            },
        );
    }
    let (exists, extensible) = {
        (
            heap_own_property(runtime, reference, &key)?,
            runtime.object_record(reference)?.is_extensible(),
        )
    };
    // An object-literal or class-field definition is
    // `CreateDataPropertyOrThrow`, whose descriptor is a fully writable,
    // enumerable, configurable data property. Routing it through
    // `ValidateAndApplyPropertyDescriptor` keeps one authority for the
    // compatibility rules, so redefining a non-configurable property is
    // rejected here exactly as it is for an explicit `defineProperty`.
    let definition = PropertyDefinition::data(Requested::Present(value), Requested::Present(true))
        .with_enumerable(Requested::Present(true))
        .with_configurable(Requested::Present(true));
    let decision = match &exists {
        Some(existing) => validate_and_apply_existing(&definition, existing),
        None => validate_and_apply_new(&definition, extensible),
    };
    match decision {
        DefinitionDecision::Unchanged => {
            if let Some(cell) = mapped_cell
                && let Some(value) = definition.present_data_value()
            {
                runtime.replace_mapped_arguments_cell_value(cell, value.duplicate())?;
            }
            Ok(PropertyWriteOutcome::Complete)
        }
        DefinitionDecision::Rejected if exists.is_some() => Ok(PropertyWriteOutcome::Failed(
            PropertyFailure::NotConfigurable,
        )),
        DefinitionDecision::Rejected => {
            Ok(PropertyWriteOutcome::Failed(PropertyFailure::NonExtensible))
        }
        DefinitionDecision::Replace(property) => {
            if runtime
                .object_record_mut(reference)?
                .restore_existing_property(&key, property)
                .is_none()
            {
                return Err(EngineFault::RuntimeInvariant {
                    message: "located own property disappeared before its data definition",
                }
                .into());
            }
            runtime.collection_pending = true;
            if let Some(cell) = mapped_cell
                && let Some(value) = definition.present_data_value()
            {
                runtime.replace_mapped_arguments_cell_value(cell, value.duplicate())?;
            }
            Ok(PropertyWriteOutcome::Complete)
        }
        DefinitionDecision::Create(property) => {
            runtime.append_own_property(reference, key, property)?;
            Ok(PropertyWriteOutcome::Complete)
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "method naming, descriptor merging, publication, and rollback form one failure-atomic transaction"
)]
pub(super) fn define_static_method(
    runtime: &mut Runtime,
    base: &StoredValue,
    key: PropertyKey,
    name: &JsString,
    function: FunctionId,
    kind: DefineMethodKind,
    enumerable: bool,
) -> Result<PropertyDefinitionOutcome, ExecutionError> {
    let reference = match base {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Ok(PropertyDefinitionOutcome::Failed(
                PropertyFailure::NotObject,
            ));
        }
    };
    if reference == HeapReference::Function(function) {
        return Err(EngineFault::RuntimeInvariant {
            message: "define_method cannot publish a function onto itself",
        }
        .into());
    }
    if bytecode_function_is_constructor(runtime, function)? {
        return Err(EngineFault::RuntimeInvariant {
            message: "define_method received a constructable function",
        }
        .into());
    }

    let function_name = method_function_name(name, kind)?;
    let previous_name = preflight_method_function_name(runtime, function)?;
    let (existing, extensible) = {
        let record = runtime.object_record(reference)?;
        (record.own_property(&key), record.is_extensible())
    };
    if existing
        .as_ref()
        .is_some_and(|property| !property.layout().is_configurable())
    {
        return Ok(PropertyDefinitionOutcome::Failed(
            PropertyFailure::NotConfigurable,
        ));
    }
    if existing.is_none() && !extensible {
        return Ok(PropertyDefinitionOutcome::Failed(
            PropertyFailure::NonExtensible,
        ));
    }
    if existing.is_none() {
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            runtime.limits.max_object_properties,
            runtime.object_properties.saturating_add(1),
        )?;
        runtime
            .object_record_mut(reference)?
            .try_reserve_data(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
    }

    let layout = match kind {
        DefineMethodKind::Method => PropertyLayout::data(true, enumerable, true),
        DefineMethodKind::Getter | DefineMethodKind::Setter => {
            PropertyLayout::accessor(enumerable, true)
        }
    };
    set_preflighted_method_function_name(runtime, function, function_name)?;
    let definition = (|| -> Result<(), ExecutionError> {
        if let Some(existing) = existing {
            let replacement = match kind {
                DefineMethodKind::Method => runtime
                    .object_record_mut(reference)?
                    .replace_existing_with_data(&key, layout, StoredValue::Function(function)),
                DefineMethodKind::Getter => {
                    let setter = match existing {
                        OwnProperty::Accessor { setter, .. } => setter,
                        OwnProperty::Data { .. } => None,
                    };
                    runtime
                        .object_record_mut(reference)?
                        .replace_existing_with_accessor(&key, layout, Some(function), setter)
                }
                DefineMethodKind::Setter => {
                    let getter = match existing {
                        OwnProperty::Accessor { getter, .. } => getter,
                        OwnProperty::Data { .. } => None,
                    };
                    runtime
                        .object_record_mut(reference)?
                        .replace_existing_with_accessor(&key, layout, getter, Some(function))
                }
            };
            if replacement.is_none() {
                return Err(EngineFault::RuntimeInvariant {
                    message: "located own property disappeared during define_method",
                }
                .into());
            }
        } else {
            match kind {
                DefineMethodKind::Method => runtime.append_data_property(
                    reference,
                    key,
                    layout,
                    StoredValue::Function(function),
                )?,
                DefineMethodKind::Getter => runtime.append_accessor_property(
                    reference,
                    key,
                    layout,
                    Some(function),
                    None,
                )?,
                DefineMethodKind::Setter => runtime.append_accessor_property(
                    reference,
                    key,
                    layout,
                    None,
                    Some(function),
                )?,
            }
        }
        Ok(())
    })();
    if let Err(error) = definition {
        restore_preflighted_method_function_name(runtime, function, previous_name)?;
        return Err(error);
    }
    // The function name is initialized before the target slot becomes
    // observable. Every fallible target-append resource is preflighted above;
    // the rollback remains as a defensive transaction boundary.
    if runtime
        .object_record(HeapReference::Function(function))?
        .own_property(&runtime.predefined_property_key(PredefinedAtom::Name))
        .is_none()
    {
        return Err(EngineFault::RuntimeInvariant {
            message: "defined method lost its initialized name property",
        }
        .into());
    }
    runtime.collection_pending = true;
    Ok(PropertyDefinitionOutcome::Complete)
}

fn method_function_name(
    name: &JsString,
    kind: DefineMethodKind,
) -> Result<JsString, JsStringError> {
    match kind {
        DefineMethodKind::Method => Ok(name.clone()),
        DefineMethodKind::Getter => JsString::from_utf8("get ")?.concat(name),
        DefineMethodKind::Setter => JsString::from_utf8("set ")?.concat(name),
    }
}

fn preflight_method_function_name(
    runtime: &Runtime,
    function: FunctionId,
) -> Result<OwnProperty, ExecutionError> {
    let key = runtime.predefined_property_key(PredefinedAtom::Name);
    let property = runtime
        .object_record(HeapReference::Function(function))?
        .own_property(&key)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "define_method function has no own name property",
        })?;
    match property {
        OwnProperty::Data { layout, .. } if layout == PropertyLayout::data(false, false, true) => {
            Ok(property)
        }
        OwnProperty::Data { .. } | OwnProperty::Accessor { .. } => {
            Err(EngineFault::RuntimeInvariant {
                message: "define_method function has an invalid name descriptor",
            }
            .into())
        }
    }
}

fn set_preflighted_method_function_name(
    runtime: &mut Runtime,
    function: FunctionId,
    name: JsString,
) -> Result<(), ExecutionError> {
    let key = runtime.predefined_property_key(PredefinedAtom::Name);
    let replaced = runtime
        .object_record_mut(HeapReference::Function(function))?
        .replace_existing_with_data(
            &key,
            PropertyLayout::data(false, false, true),
            StoredValue::String(name),
        );
    if replaced.is_none() {
        return Err(EngineFault::RuntimeInvariant {
            message: "preflighted define_method name property disappeared",
        }
        .into());
    }
    Ok(())
}

fn restore_preflighted_method_function_name(
    runtime: &mut Runtime,
    function: FunctionId,
    previous: OwnProperty,
) -> Result<(), ExecutionError> {
    let key = runtime.predefined_property_key(PredefinedAtom::Name);
    let restored = runtime
        .object_record_mut(HeapReference::Function(function))?
        .restore_existing_property(&key, previous);
    if restored.is_none() {
        return Err(EngineFault::RuntimeInvariant {
            message: "preflighted define_method name property disappeared during rollback",
        }
        .into());
    }
    Ok(())
}

/// Starts the pinned `copy_data_properties` abstract operation over the
/// source's own enumerable string-keyed properties: each key not present on
/// the excluded object is read (resumably, so accessors run) and defined on
/// the target with the ordinary writable/enumerable/configurable layout.
/// The operand stack is untouched; the caller consumes the three operands.
#[allow(
    clippy::too_many_arguments,
    reason = "copy-data-properties carries the same runtime, operand, realm, resume, origin, and budget authority as every other resumable native operation"
)]
pub(super) fn begin_copy_data_properties(
    runtime: &mut Runtime,
    target: StoredValue,
    source: StoredValue,
    excluded: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let reference = match &source {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "copy-data-properties source is not an object",
            }
            .into());
        }
    };
    // CopyDataProperties snapshots all own keys, including Symbols. Whether a
    // snapshotted key is still present and enumerable is rechecked when that
    // key is reached after any earlier getter re-entry.
    let (snapshot, snapshot_work) = runtime.try_own_key_snapshot(reference, 0, KeyPhases::ALL)?;
    execution_budget.charge_instructions(snapshot_work)?;
    let excluded = if matches!(excluded, StoredValue::Undefined) {
        None
    } else {
        Some(excluded)
    };
    let state = CopyDataPropertiesContinuation {
        target,
        source,
        excluded,
        snapshot,
        next: 0,
        current_key: None,
        realm,
        stage: CopyDataPropertiesStage::Next,
        origin,
    };
    advance_copy_data_properties(
        runtime,
        state,
        &StoredValue::Undefined,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "the resumable next/read-value state machine mirrors the pinned copy_data_properties stages in one traced continuation"
)]
pub(super) fn advance_copy_data_properties(
    runtime: &mut Runtime,
    mut state: CopyDataPropertiesContinuation,
    completion: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        match state.stage {
            CopyDataPropertiesStage::Next => {
                let Some(candidate) = state.snapshot.get(state.next).cloned() else {
                    return Ok(NativeDispatch::CopyDataPropertiesDone);
                };
                state.next = state.next.saturating_add(1);
                execution_budget.charge_instructions(1)?;
                if let Some(excluded) = &state.excluded {
                    let reference = match excluded {
                        StoredValue::Function(function) => HeapReference::Function(*function),
                        StoredValue::Object(object) => HeapReference::Object(*object),
                        StoredValue::Undefined
                        | StoredValue::Null
                        | StoredValue::Boolean(_)
                        | StoredValue::Number(_)
                        | StoredValue::BigInt(_)
                        | StoredValue::String(_)
                        | StoredValue::Symbol(_) => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "copy-data-properties excluded operand is not an object",
                            }
                            .into());
                        }
                    };
                    if runtime
                        .object_record(reference)?
                        .own_property(candidate.key())
                        .is_some()
                    {
                        continue;
                    }
                }
                let source_reference =
                    state
                        .source
                        .heap_reference()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "copy-data-properties lost its object source",
                        })?;
                charge_heap_property_lookup(runtime, &state.source, execution_budget)?;
                let Some(own) = own_property_of(runtime, source_reference, candidate.key())? else {
                    continue;
                };
                if !own.layout().is_enumerable() {
                    continue;
                }
                charge_heap_property_lookup(runtime, &state.source, execution_budget)?;
                match read_static_property(runtime, state.realm, &state.source, candidate.key())? {
                    PropertyReadOutcome::Value(value) => {
                        match define_static_property(
                            runtime,
                            &state.target,
                            candidate.key().clone(),
                            value,
                            execution_budget,
                        )? {
                            PropertyWriteOutcome::Complete => {}
                            PropertyWriteOutcome::Failed(failure) => {
                                return Err(NativeFailure::Abrupt(property_exception_at(
                                    state.realm,
                                    state.origin,
                                    None,
                                    failure,
                                )?));
                            }
                            PropertyWriteOutcome::Setter { .. } => {
                                return Err(EngineFault::RuntimeInvariant {
                                    message: "copy-data-properties define returned a setter",
                                }
                                .into());
                            }
                        }
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.stage = CopyDataPropertiesStage::ReadValue;
                        state.current_key = Some(candidate.key().clone());
                        let origin = state.origin.clone();
                        return iterator_getter_call(
                            function,
                            receiver,
                            NativeContinuation::CopyDataProperties(state),
                            return_to,
                            origin,
                            None,
                        );
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(NativeFailure::Abrupt(property_exception_at(
                            state.realm,
                            state.origin,
                            None,
                            failure,
                        )?));
                    }
                }
            }
            CopyDataPropertiesStage::ReadValue => {
                let key = state
                    .current_key
                    .take()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "copy-data-properties getter resumed without a pending key",
                    })?;
                match define_static_property(
                    runtime,
                    &state.target,
                    key,
                    completion.duplicate(),
                    execution_budget,
                )? {
                    PropertyWriteOutcome::Complete => {
                        state.stage = CopyDataPropertiesStage::Next;
                    }
                    PropertyWriteOutcome::Failed(failure) => {
                        return Err(NativeFailure::Abrupt(property_exception_at(
                            state.realm,
                            state.origin,
                            None,
                            failure,
                        )?));
                    }
                    PropertyWriteOutcome::Setter { .. } => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "copy-data-properties define returned a setter",
                        }
                        .into());
                    }
                }
            }
        }
    }
}

/// Applies ECMAScript `[[DefineOwnProperty]]` from a validated descriptor.
///
/// This is the script-reachable entry to the descriptor authority: the decision
/// is made by `ValidateAndApplyPropertyDescriptor`, and only the mutation
/// happens here. An array index routes through the exotic array define so the
/// `length` invariant is maintained.
#[allow(
    clippy::too_many_lines,
    reason = "ordinary descriptor authority and mapped-arguments post-commit semantics stay auditable together"
)]
pub(super) fn define_own_property(
    runtime: &mut Runtime,
    base: &StoredValue,
    key: PropertyKey,
    definition: &PropertyDefinition,
    execution_budget: &mut ExecutionBudget,
) -> Result<PropertyDefinitionOutcome, ExecutionError> {
    let reference = match base {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Ok(PropertyDefinitionOutcome::Failed(
                PropertyFailure::NotObject,
            ));
        }
    };
    let mapped_object = match reference {
        HeapReference::Object(object) => runtime
            .mapped_arguments_cell(object, &key)?
            .map(|cell| (object, cell)),
        HeapReference::Function(_) => None,
    };

    // A present Array `length` value must first pass the resumable two-conversion
    // ArraySetLength validation. Attribute-only and accessor definitions can be
    // decided here without observing user code; the latter is rejected by the
    // ordinary non-configurable data-property rules below.
    if let HeapReference::Object(object) = reference
        && runtime.is_array_object(object)?
        && key.as_atom().and_then(crate::Atom::predefined_atom) == Some(PredefinedAtom::Length)
        && definition.has_present_data_value()
    {
        return Ok(PropertyDefinitionOutcome::Failed(
            PropertyFailure::NotConfigurable,
        ));
    }

    if let Some((object, _)) = mapped_object
        && definition.requested_writable() == Some(false)
        && definition.present_data_value().is_none()
    {
        runtime.synchronize_mapped_arguments_property(object, &key)?;
    }
    let (existing, extensible) = (
        heap_own_property(runtime, reference, &key)?,
        runtime.object_record(reference)?.is_extensible(),
    );
    let decision = match &existing {
        Some(existing) => validate_and_apply_existing(definition, existing),
        None => validate_and_apply_new(definition, extensible),
    };
    match decision {
        DefinitionDecision::Unchanged => {
            apply_mapped_arguments_definition(runtime, mapped_object, &key, definition)?;
            Ok(PropertyDefinitionOutcome::Complete)
        }
        DefinitionDecision::Rejected if existing.is_some() => Ok(
            PropertyDefinitionOutcome::Failed(PropertyFailure::NotConfigurable),
        ),
        DefinitionDecision::Rejected => Ok(PropertyDefinitionOutcome::Failed(
            PropertyFailure::NonExtensible,
        )),
        DefinitionDecision::Replace(property) => {
            if runtime
                .object_record_mut(reference)?
                .restore_existing_property(&key, property)
                .is_none()
            {
                return Err(EngineFault::RuntimeInvariant {
                    message: "located own property disappeared before its definition",
                }
                .into());
            }
            runtime.collection_pending = true;
            apply_mapped_arguments_definition(runtime, mapped_object, &key, definition)?;
            Ok(PropertyDefinitionOutcome::Complete)
        }
        DefinitionDecision::Create(property) => {
            // An array index must extend the cached length, which the exotic
            // define owns.
            if let HeapReference::Object(object) = reference
                && runtime.is_array_object(object)?
                && let Some(index) = key.as_index()
            {
                let work = runtime.preview_array_define_data_property_work(object)?;
                execution_budget.charge_instructions(work)?;
                let _ = index;
                return Ok(match property {
                    OwnProperty::Data { layout, value } => {
                        match runtime.define_array_data_property(object, key, layout, value)? {
                            ArrayDefineOutcome::Complete => PropertyDefinitionOutcome::Complete,
                            ArrayDefineOutcome::ReadOnlyLength => {
                                PropertyDefinitionOutcome::Failed(PropertyFailure::ReadOnly)
                            }
                            ArrayDefineOutcome::NonExtensible => {
                                PropertyDefinitionOutcome::Failed(PropertyFailure::NonExtensible)
                            }
                        }
                    }
                    // An accessor at an array index leaves the dense range, so
                    // it is stored as an ordinary property while the length is
                    // still extended by the data path above.
                    property @ OwnProperty::Accessor { .. } => {
                        runtime.append_own_property(reference, key, property)?;
                        PropertyDefinitionOutcome::Complete
                    }
                });
            }
            runtime.append_own_property(reference, key.clone(), property)?;
            apply_mapped_arguments_definition(runtime, mapped_object, &key, definition)?;
            Ok(PropertyDefinitionOutcome::Complete)
        }
    }
}

fn apply_mapped_arguments_definition(
    runtime: &mut Runtime,
    mapped: Option<(ObjectId, BindingCellId)>,
    key: &PropertyKey,
    definition: &PropertyDefinition,
) -> Result<(), ExecutionError> {
    let Some((object, cell)) = mapped else {
        return Ok(());
    };
    if definition.is_accessor_descriptor() {
        let detached = runtime.detach_mapped_arguments_property(object, key)?;
        debug_assert_eq!(detached, Some(cell));
        return Ok(());
    }
    if let Some(value) = definition.present_data_value() {
        runtime.replace_mapped_arguments_cell_value(cell, value.duplicate())?;
    }
    if definition.requested_writable() == Some(false) {
        let detached = runtime.detach_mapped_arguments_property(object, key)?;
        debug_assert_eq!(detached, Some(cell));
    }
    Ok(())
}
