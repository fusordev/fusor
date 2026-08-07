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

/// `%Int8Array%`-style construction after the initial `ToIndex(length)`.
pub(super) struct TypedArrayConstructorLengthState {
    new_target: FunctionId,
    element: TypedArrayElementType,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TypedArrayConstructorLengthState {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
    }
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

pub(super) fn begin_typed_array_constructor(
    runtime: &mut Runtime,
    element: TypedArrayElementType,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = inputs.new_target else {
        return typed_array_type_error(realm, &origin, "TypedArray constructor requires 'new'");
    };
    let mut arguments = inputs.arguments;
    let length = arguments.take_first_or_undefined();
    if matches!(length, StoredValue::Object(_) | StoredValue::Function(_)) {
        return typed_array_type_error(
            realm,
            &origin,
            "TypedArray object, iterable, and ArrayBuffer initializers are not implemented",
        );
    }
    begin_operator_primitive_conversion(
        runtime,
        length,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayConstructorLength(Box::new(
            TypedArrayConstructorLengthState {
                new_target,
                element,
                realm,
                origin: origin.clone(),
            },
        )),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_typed_array_constructor_length(
    runtime: &mut Runtime,
    state: TypedArrayConstructorLengthState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let length = typed_array_to_index(value, state.realm, &state.origin)?;
    let Some(_byte_length) = length.checked_mul(state.element.byte_width()) else {
        return typed_array_range_error(
            state.realm,
            &state.origin,
            "TypedArray length exceeds implementation range",
        );
    };
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    begin_intrinsic_get(
        runtime,
        state.realm,
        HeapReference::Function(state.new_target),
        StoredValue::Function(state.new_target),
        &prototype_key,
        IntrinsicGetContinuation::TypedArrayConstructor {
            new_target: state.new_target,
            element: state.element,
            length,
        },
        return_to,
        Some(state.origin),
        execution_budget,
    )
}

pub(super) fn finish_typed_array_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    element: TypedArrayElementType,
    length: usize,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let realm = runtime.function_realm(new_target)?;
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            HeapReference::Object(runtime.realm_typed_array_prototype(realm, element)?)
        }
    };
    let byte_length =
        length
            .checked_mul(element.byte_width())
            .ok_or(EngineFault::RuntimeInvariant {
                message: "validated typed-array length overflowed its byte length",
            })?;
    let buffer = runtime
        .allocate_array_buffer(
            HeapReference::Object(runtime.realm_array_buffer_prototype(realm)?),
            byte_length,
            None,
        )
        .map_err(NativeFailure::Execution)?;
    let object = runtime
        .allocate_typed_array(
            prototype,
            TypedArrayState::new(buffer, 0, TypedArrayLength::Fixed(length), element),
        )
        .map_err(NativeFailure::Execution)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(super) fn dispatch_typed_array_prototype(
    runtime: &Runtime,
    method: TypedArrayPrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(object) = receiver else {
        return typed_array_type_error(realm, &origin, "not a TypedArray");
    };
    let state =
        runtime
            .typed_array_state(*object)?
            .copied()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "typed-array prototype receiver lost its internal slots",
            })?;
    let view = runtime.typed_array_view(*object)?;
    let (byte_length, byte_offset, length) = match view {
        TypedArrayView::InBounds {
            byte_offset,
            length,
            ..
        } => (
            length.saturating_mul(state.element().byte_width()),
            byte_offset,
            length,
        ),
        TypedArrayView::Detached | TypedArrayView::OutOfBounds => (0, 0, 0),
    };
    let number = |value: usize| {
        #[expect(
            clippy::cast_precision_loss,
            reason = "typed-array byte lengths and indices are bounded by ToIndex"
        )]
        let value = value as f64;
        StoredValue::Number(JsNumber::from_f64(value))
    };
    Ok(NativeDispatch::Immediate(match method {
        TypedArrayPrototypeMethod::Buffer => StoredValue::Object(state.buffer()),
        TypedArrayPrototypeMethod::ByteLength => number(byte_length),
        TypedArrayPrototypeMethod::ByteOffset => number(byte_offset),
        TypedArrayPrototypeMethod::Length => number(length),
        TypedArrayPrototypeMethod::ToStringTag => {
            StoredValue::String(JsString::from_utf8(typed_array_name(state.element()))?)
        }
    }))
}

fn typed_array_to_index(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<usize, NativeFailure> {
    let number = operator_to_number(value, realm, origin)?;
    let Some(index) = number_to_index(number) else {
        return typed_array_range_error(realm, origin, "invalid TypedArray length");
    };
    usize::try_from(index).map_err(|_| {
        NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("TypedArray length exceeds implementation range")
                    .expect("static TypedArray range message is valid UTF-8"),
            },
            origin: origin.clone(),
        })
    })
}

fn typed_array_name(element: TypedArrayElementType) -> &'static str {
    match element {
        TypedArrayElementType::Int8 => "Int8Array",
        TypedArrayElementType::Uint8 => "Uint8Array",
        TypedArrayElementType::Uint8Clamped => "Uint8ClampedArray",
        TypedArrayElementType::Int16 => "Int16Array",
        TypedArrayElementType::Uint16 => "Uint16Array",
        TypedArrayElementType::Int32 => "Int32Array",
        TypedArrayElementType::Uint32 => "Uint32Array",
        TypedArrayElementType::BigInt64 => "BigInt64Array",
        TypedArrayElementType::BigUint64 => "BigUint64Array",
        TypedArrayElementType::Float16 => "Float16Array",
        TypedArrayElementType::Float32 => "Float32Array",
        TypedArrayElementType::Float64 => "Float64Array",
    }
}

fn typed_array_type_error<T>(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<T, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    }))
}

fn typed_array_range_error<T>(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<T, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::RangeError,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
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
