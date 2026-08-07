//! Resumable integer-indexed typed-array element writes.
//!
//! `TypedArraySetElement` converts its value before it performs the final
//! `IsValidIntegerIndex` witness. Keeping that sequence in an explicit
//! `OperatorPrimitiveTarget` lets a user-defined conversion resize a backing
//! `ArrayBuffer` without publishing an old bounds decision.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) struct TypedArrayElementSetState {
    object: ObjectId,
    index: Option<usize>,
    completion: TypedArraySetCompletion,
    realm: RealmId,
    origin: JsStackFrame,
}

#[derive(Clone, Copy)]
pub(super) enum TypedArraySetCompletion {
    LanguageWrite,
    ReflectSet,
    Define(DefinePropertyResult),
}

/// The non-observable prefix of typed-array `[[DefineOwnProperty]]`.
///
/// A `Store` has already passed its first `IsValidIntegerIndex` and all
/// descriptor shape restrictions. Its eventual store still takes a new
/// buffer witness after value coercion, as required by `TypedArraySetElement`.
pub(super) enum TypedArrayDefineAction {
    Ordinary,
    Rejected,
    Complete,
    Store(usize),
}

impl TypedArrayElementSetState {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.object)));
    }
}

/// Recognizes a canonical numeric key on a direct typed-array receiver. The
/// returned key deliberately preserves `Invalid`: it must still run the value
/// conversion for a self-receiver write before the final bounds decision.
pub(super) fn typed_array_indexed_key(
    runtime: &Runtime,
    base: &StoredValue,
    key: &PropertyKey,
) -> Result<Option<(ObjectId, TypedArrayPropertyKey)>, ExecutionError> {
    let StoredValue::Object(object) = base else {
        return Ok(None);
    };
    let Some(key) = runtime.typed_array_property_key(*object, key)? else {
        return Ok(None);
    };
    Ok((key != TypedArrayPropertyKey::Ordinary).then_some((*object, key)))
}

pub(super) fn typed_array_define_own_property_action(
    runtime: &Runtime,
    object: ObjectId,
    key: &PropertyKey,
    definition: &PropertyDefinition,
) -> Result<Option<TypedArrayDefineAction>, ExecutionError> {
    let Some(key) = runtime.typed_array_property_key(object, key)? else {
        return Ok(None);
    };
    let TypedArrayPropertyKey::Index(index) = key else {
        return Ok(Some(match key {
            TypedArrayPropertyKey::Ordinary => TypedArrayDefineAction::Ordinary,
            TypedArrayPropertyKey::Invalid => TypedArrayDefineAction::Rejected,
            TypedArrayPropertyKey::Index(_) => unreachable!("matched above"),
        }));
    };
    if runtime.typed_array_read_index(object, index)?.is_none()
        || definition.requested_configurable() == Some(false)
        || definition.requested_enumerable() == Some(false)
        || definition.is_accessor_descriptor()
        || definition.requested_writable() == Some(false)
    {
        return Ok(Some(TypedArrayDefineAction::Rejected));
    }
    Ok(Some(if definition.has_present_data_value() {
        TypedArrayDefineAction::Store(index)
    } else {
        TypedArrayDefineAction::Complete
    }))
}

/// Starts `TypedArraySetElement` for a canonical numeric key. `Invalid` is
/// still converted: the conversion precedes `IsValidIntegerIndex` in the
/// normative abstract operation, while a non-canonical key never reaches this
/// path.
#[allow(
    clippy::too_many_arguments,
    reason = "the receiver-independent typed-array write carries its key classification, completion shape, and VM resume authority explicitly"
)]
pub(super) fn begin_typed_array_element_set(
    runtime: &mut Runtime,
    object: ObjectId,
    key: TypedArrayPropertyKey,
    value: StoredValue,
    completion: TypedArraySetCompletion,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let index = match key {
        TypedArrayPropertyKey::Index(index) => Some(index),
        TypedArrayPropertyKey::Invalid => None,
        TypedArrayPropertyKey::Ordinary => {
            return Err(EngineFault::RuntimeInvariant {
                message: "typed-array element set received an ordinary property key",
            }
            .into());
        }
    };
    if runtime.typed_array_state(object)?.is_none() {
        return Err(EngineFault::RuntimeInvariant {
            message: "typed-array element set receiver lost its internal slots",
        }
        .into());
    }
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayElementSet(Box::new(TypedArrayElementSetState {
            object,
            index,
            completion,
            realm,
            origin: origin.clone(),
        })),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_typed_array_element_set(
    runtime: &mut Runtime,
    state: TypedArrayElementSetState,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let element = runtime
        .typed_array_state(state.object)?
        .copied()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "typed-array element set receiver lost its internal slots",
        })?
        .element();
    let stored = if element.is_bigint() {
        let value = to_bigint_from_primitive(&value, state.realm, &state.origin)?;
        state
            .index
            .map_or(Ok(TypedArrayStoreOutcome::Missing), |index| {
                runtime.typed_array_store_index(
                    state.object,
                    index,
                    TypedArrayElementValue::BigInt(value.as_ref()),
                )
            })?
    } else {
        let value = operator_to_number(value, state.realm, &state.origin)?;
        state
            .index
            .map_or(Ok(TypedArrayStoreOutcome::Missing), |index| {
                runtime.typed_array_store_index(
                    state.object,
                    index,
                    TypedArrayElementValue::Number(value),
                )
            })?
    };
    if stored == TypedArrayStoreOutcome::ContentTypeMismatch {
        return Err(EngineFault::RuntimeInvariant {
            message: "typed-array element content type changed during conversion",
        }
        .into());
    }
    Ok(NativeDispatch::Immediate(match state.completion {
        TypedArraySetCompletion::LanguageWrite => StoredValue::Undefined,
        TypedArraySetCompletion::ReflectSet => StoredValue::Boolean(true),
        TypedArraySetCompletion::Define(DefinePropertyResult::Target) => {
            StoredValue::Object(state.object)
        }
        TypedArraySetCompletion::Define(DefinePropertyResult::Boolean) => {
            StoredValue::Boolean(true)
        }
    }))
}
