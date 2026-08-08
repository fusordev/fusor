//! Synchronous `%Atomics%` operations over shared integer typed arrays.
//!
//! The runtime is deliberately thread-affine, so each read-modify-write is a
//! single interpreter operation. Observable index/value coercions still use
//! the ordinary resumable `ToPrimitive` machinery before that operation.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) struct AtomicsContinuation {
    method: AtomicsMethod,
    object: ObjectId,
    access_length: usize,
    index: Option<usize>,
    value: Option<StoredValue>,
    replacement: Option<StoredValue>,
    realm: RealmId,
    origin: JsStackFrame,
}

impl AtomicsContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64
            .saturating_add(u64::from(self.value.is_some()))
            .saturating_add(u64::from(self.replacement.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.object)));
        if let Some(value) = &self.value {
            trace_stored_value_root(value, mark);
        }
        if let Some(value) = &self.replacement {
            trace_stored_value_root(value, mark);
        }
    }
}

pub(super) fn begin_atomics_method(
    runtime: &mut Runtime,
    method: AtomicsMethod,
    realm: RealmId,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if method == AtomicsMethod::Pause {
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    }
    if method == AtomicsMethod::IsLockFree {
        return begin_operator_primitive_conversion(
            runtime,
            arguments.take_first_or_undefined(),
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::AtomicsIsLockFree,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    let typed_array = arguments.take_first_or_undefined();
    let object = atomics_typed_array(runtime, method, &typed_array, realm, &origin)?;
    let access_length = atomics_access_length(runtime, object)?;
    let index = arguments.take_first_or_undefined();
    let value = method
        .requires_value()
        .then(|| arguments.take_first_or_undefined());
    let replacement = matches!(method, AtomicsMethod::CompareExchange | AtomicsMethod::Wait)
        .then(|| arguments.take_first_or_undefined());
    let state = AtomicsContinuation {
        method,
        object,
        access_length,
        index: None,
        value,
        replacement,
        realm,
        origin: origin.clone(),
    };
    begin_operator_primitive_conversion(
        runtime,
        index,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::AtomicsIndex(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_atomics_is_lock_free(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let size = number_to_integer_or_infinity(operator_to_number(value, realm, origin)?);
    Ok(NativeDispatch::Immediate(StoredValue::Boolean(matches!(
        size,
        1.0 | 2.0 | 4.0 | 8.0
    ))))
}

pub(super) fn finish_atomics_index(
    runtime: &mut Runtime,
    mut state: AtomicsContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let index = atomics_index(value, state.realm, &state.origin)?;
    atomics_validate_access(state.access_length, index, state.realm, &state.origin)?;
    state.index = Some(index);
    if state.method == AtomicsMethod::Load {
        return atomics_load(runtime, state.object, index, state.realm, &state.origin);
    }
    let value = state.value.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Atomics operation lost its value argument",
    })?;
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::AtomicsValue(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_atomics_value(
    runtime: &mut Runtime,
    mut state: AtomicsContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.method == AtomicsMethod::Notify {
        return atomics_notify(&state, value);
    }
    if state.method == AtomicsMethod::Wait {
        return atomics_wait_expected(runtime, state, value, return_to, execution_budget);
    }
    if state.method == AtomicsMethod::CompareExchange {
        state.value = Some(value);
        let replacement = state
            .replacement
            .take()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Atomics.compareExchange lost its replacement argument",
            })?;
        let realm = state.realm;
        let origin = state.origin.clone();
        return begin_operator_primitive_conversion(
            runtime,
            replacement,
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::AtomicsReplacement(Box::new(state)),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    atomics_apply(runtime, &state, value, None)
}

pub(super) fn finish_atomics_replacement(
    runtime: &mut Runtime,
    mut state: AtomicsContinuation,
    replacement: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let expected = state.value.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Atomics.compareExchange lost its expected value",
    })?;
    atomics_apply(runtime, &state, expected, Some(replacement))
}

pub(super) fn finish_atomics_timeout(
    state: &AtomicsContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let timeout = operator_to_number(value, state.realm, &state.origin)?;
    atomics_wait_timeout(timeout)
}

fn atomics_typed_array(
    runtime: &Runtime,
    method: AtomicsMethod,
    value: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<ObjectId, NativeFailure> {
    let StoredValue::Object(object) = value else {
        return atomics_type_error(realm, origin, "not an integer typed array");
    };
    let Some(state) = runtime.typed_array_state(*object)? else {
        return atomics_type_error(realm, origin, "not an integer typed array");
    };
    if !atomics_element_type(state.element()) {
        return atomics_type_error(realm, origin, "typed array element type is not atomic");
    }
    if method.requires_waitable_element() && !atomics_waitable_element_type(state.element()) {
        return atomics_type_error(realm, origin, "typed array element type is not waitable");
    }
    let Some(buffer) = runtime.array_buffer_state(state.buffer())? else {
        return Err(EngineFault::RuntimeInvariant {
            message: "Atomics typed array lost its backing buffer slots",
        }
        .into());
    };
    if method.requires_shared_buffer() && !buffer.is_shared() {
        return atomics_type_error(
            realm,
            origin,
            "typed array is not backed by SharedArrayBuffer",
        );
    }
    if method.requires_writable_buffer() && buffer.is_immutable() {
        return atomics_type_error(realm, origin, "typed array backing buffer is immutable");
    }
    Ok(*object)
}

fn atomics_waitable_element_type(element: TypedArrayElementType) -> bool {
    matches!(
        element,
        TypedArrayElementType::Int32 | TypedArrayElementType::BigInt64
    )
}

fn atomics_access_length(runtime: &Runtime, object: ObjectId) -> Result<usize, NativeFailure> {
    match runtime.typed_array_view(object)? {
        TypedArrayView::InBounds { length, .. } => Ok(length),
        TypedArrayView::Detached | TypedArrayView::OutOfBounds => Ok(0),
    }
}

fn atomics_element_type(element: TypedArrayElementType) -> bool {
    matches!(
        element,
        TypedArrayElementType::Int8
            | TypedArrayElementType::Uint8
            | TypedArrayElementType::Int16
            | TypedArrayElementType::Uint16
            | TypedArrayElementType::Int32
            | TypedArrayElementType::Uint32
            | TypedArrayElementType::BigInt64
            | TypedArrayElementType::BigUint64
    )
}

fn atomics_index(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<usize, NativeFailure> {
    let number = operator_to_number(value, realm, origin)?;
    let Some(index) = number_to_index(number) else {
        return atomics_range_error(realm, origin, "invalid atomic index");
    };
    usize::try_from(index).map_err(|_| {
        NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("invalid atomic index")
                    .expect("static atomics range error is valid UTF-8"),
            },
            origin: origin.clone(),
        })
    })
}

fn atomics_validate_access(
    length: usize,
    index: usize,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<(), NativeFailure> {
    if index >= length {
        return atomics_range_error(realm, origin, "atomic index is outside typed array bounds");
    }
    Ok(())
}

fn atomics_load(
    runtime: &Runtime,
    object: ObjectId,
    index: usize,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let value =
        runtime
            .typed_array_read_index(object, index)?
            .ok_or(EngineFault::RuntimeInvariant {
                message: "validated Atomics load lost its indexed element",
            })?;
    let _ = (realm, origin);
    Ok(NativeDispatch::Immediate(value))
}

fn atomics_apply(
    runtime: &mut Runtime,
    state: &AtomicsContinuation,
    value: StoredValue,
    replacement: Option<StoredValue>,
) -> Result<NativeDispatch, NativeFailure> {
    let index = state.index.ok_or(EngineFault::RuntimeInvariant {
        message: "Atomics operation lost its validated index",
    })?;
    atomics_validate_access(state.access_length, index, state.realm, &state.origin)?;
    let element = runtime
        .typed_array_state(state.object)?
        .copied()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Atomics operation lost its typed-array state",
        })?
        .element();
    let old = runtime.typed_array_read_index(state.object, index)?.ok_or(
        EngineFault::RuntimeInvariant {
            message: "validated Atomics operation lost its indexed element",
        },
    )?;
    if element.is_bigint() {
        atomics_apply_bigint(runtime, state, index, old, &value, replacement, element)
    } else {
        atomics_apply_number(runtime, state, index, &old, value, replacement, element)
    }
}

fn atomics_apply_number(
    runtime: &mut Runtime,
    state: &AtomicsContinuation,
    index: usize,
    old: &StoredValue,
    value: StoredValue,
    replacement: Option<StoredValue>,
    element: TypedArrayElementType,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Number(old) = old else {
        return Err(EngineFault::RuntimeInvariant {
            message: "number Atomics operation read a BigInt element",
        }
        .into());
    };
    let input = atomics_to_integer_number(value, state.realm, &state.origin)?;
    let result = match state.method {
        AtomicsMethod::Store => {
            atomics_store_number(runtime, state.object, index, input)?;
            StoredValue::Number(input)
        }
        AtomicsMethod::Exchange => {
            atomics_store_number(runtime, state.object, index, input)?;
            StoredValue::Number(*old)
        }
        AtomicsMethod::CompareExchange => {
            let replacement = replacement.ok_or(EngineFault::RuntimeInvariant {
                message: "Atomics.compareExchange missing replacement value",
            })?;
            let replacement = atomics_to_integer_number(replacement, state.realm, &state.origin)?;
            if atomics_normalize_number(element, *old)
                .strict_equals(atomics_normalize_number(element, input))
            {
                atomics_store_number(runtime, state.object, index, replacement)?;
            }
            StoredValue::Number(*old)
        }
        AtomicsMethod::Add
        | AtomicsMethod::And
        | AtomicsMethod::Or
        | AtomicsMethod::Sub
        | AtomicsMethod::Xor => {
            let updated = atomics_number_update(state.method, element, *old, input);
            atomics_store_number(runtime, state.object, index, updated)?;
            StoredValue::Number(*old)
        }
        AtomicsMethod::IsLockFree
        | AtomicsMethod::Load
        | AtomicsMethod::Notify
        | AtomicsMethod::Wait
        | AtomicsMethod::Pause => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-mutating Atomics method reached value application",
            }
            .into());
        }
    };
    Ok(NativeDispatch::Immediate(result))
}

fn atomics_apply_bigint(
    runtime: &mut Runtime,
    state: &AtomicsContinuation,
    index: usize,
    old: StoredValue,
    value: &StoredValue,
    replacement: Option<StoredValue>,
    element: TypedArrayElementType,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::BigInt(old) = old else {
        return Err(EngineFault::RuntimeInvariant {
            message: "BigInt Atomics operation read a Number element",
        }
        .into());
    };
    let value = to_bigint_from_primitive(value, state.realm, &state.origin)?;
    let result = match state.method {
        AtomicsMethod::Store => {
            atomics_store_bigint(runtime, state.object, index, value.as_ref())?;
            StoredValue::BigInt(value)
        }
        AtomicsMethod::Exchange => {
            atomics_store_bigint(runtime, state.object, index, value.as_ref())?;
            StoredValue::BigInt(old)
        }
        AtomicsMethod::CompareExchange => {
            let replacement = replacement.ok_or(EngineFault::RuntimeInvariant {
                message: "Atomics.compareExchange missing replacement value",
            })?;
            let replacement = to_bigint_from_primitive(&replacement, state.realm, &state.origin)?;
            let expected = atomics_normalize_bigint(element, value.as_ref(), state)?;
            if old.as_ref() == &expected {
                atomics_store_bigint(runtime, state.object, index, replacement.as_ref())?;
            }
            StoredValue::BigInt(old)
        }
        AtomicsMethod::Add
        | AtomicsMethod::And
        | AtomicsMethod::Or
        | AtomicsMethod::Sub
        | AtomicsMethod::Xor => {
            let updated = atomics_bigint_update(state.method, old.as_ref(), value.as_ref(), state)?;
            atomics_store_bigint(runtime, state.object, index, &updated)?;
            StoredValue::BigInt(old)
        }
        AtomicsMethod::IsLockFree
        | AtomicsMethod::Load
        | AtomicsMethod::Notify
        | AtomicsMethod::Wait
        | AtomicsMethod::Pause => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-mutating Atomics method reached value application",
            }
            .into());
        }
    };
    Ok(NativeDispatch::Immediate(result))
}

fn atomics_store_number(
    runtime: &mut Runtime,
    object: ObjectId,
    index: usize,
    value: JsNumber,
) -> Result<(), NativeFailure> {
    let outcome =
        runtime.typed_array_store_index(object, index, TypedArrayElementValue::Number(value))?;
    if outcome != TypedArrayStoreOutcome::Stored {
        return Err(EngineFault::RuntimeInvariant {
            message: "validated Atomics number store did not store",
        }
        .into());
    }
    Ok(())
}

fn atomics_store_bigint(
    runtime: &mut Runtime,
    object: ObjectId,
    index: usize,
    value: &JsBigInt,
) -> Result<(), NativeFailure> {
    let outcome =
        runtime.typed_array_store_index(object, index, TypedArrayElementValue::BigInt(value))?;
    if outcome != TypedArrayStoreOutcome::Stored {
        return Err(EngineFault::RuntimeInvariant {
            message: "validated Atomics BigInt store did not store",
        }
        .into());
    }
    Ok(())
}

fn atomics_normalize_number(element: TypedArrayElementType, value: JsNumber) -> JsNumber {
    match element {
        TypedArrayElementType::Int8 => JsNumber::from_i32(i32::from(number_to_int8(value))),
        TypedArrayElementType::Uint8 => JsNumber::from_i32(i32::from(number_to_uint8(value))),
        TypedArrayElementType::Int16 => JsNumber::from_i32(i32::from(number_to_int16(value))),
        TypedArrayElementType::Uint16 => JsNumber::from_i32(i32::from(number_to_uint16(value))),
        TypedArrayElementType::Int32 => JsNumber::from_i32(number_to_int32(value)),
        TypedArrayElementType::Uint32 => JsNumber::from_u32(number_to_uint32(value)),
        TypedArrayElementType::Uint8Clamped
        | TypedArrayElementType::BigInt64
        | TypedArrayElementType::BigUint64
        | TypedArrayElementType::Float16
        | TypedArrayElementType::Float32
        | TypedArrayElementType::Float64 => unreachable!("Atomics validates integer number views"),
    }
}

fn atomics_to_integer_number(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<JsNumber, NativeFailure> {
    let integer = number_to_integer_or_infinity(operator_to_number(value, realm, origin)?);
    Ok(JsNumber::from_f64(if integer == 0.0 {
        0.0
    } else {
        integer
    }))
}

fn atomics_number_update(
    method: AtomicsMethod,
    element: TypedArrayElementType,
    old: JsNumber,
    input: JsNumber,
) -> JsNumber {
    macro_rules! update {
        ($old:expr, $input:expr) => {{
            let old = $old;
            let input = $input;
            let value = match method {
                AtomicsMethod::Add => old.wrapping_add(input),
                AtomicsMethod::And => old & input,
                AtomicsMethod::Or => old | input,
                AtomicsMethod::Sub => old.wrapping_sub(input),
                AtomicsMethod::Xor => old ^ input,
                _ => unreachable!("only read-modify-write Atomics operations reach update"),
            };
            JsNumber::from_f64(f64::from(value))
        }};
    }
    match element {
        TypedArrayElementType::Int8 => update!(number_to_int8(old), number_to_int8(input)),
        TypedArrayElementType::Uint8 => update!(number_to_uint8(old), number_to_uint8(input)),
        TypedArrayElementType::Int16 => update!(number_to_int16(old), number_to_int16(input)),
        TypedArrayElementType::Uint16 => update!(number_to_uint16(old), number_to_uint16(input)),
        TypedArrayElementType::Int32 => update!(number_to_int32(old), number_to_int32(input)),
        TypedArrayElementType::Uint32 => update!(number_to_uint32(old), number_to_uint32(input)),
        TypedArrayElementType::Uint8Clamped
        | TypedArrayElementType::BigInt64
        | TypedArrayElementType::BigUint64
        | TypedArrayElementType::Float16
        | TypedArrayElementType::Float32
        | TypedArrayElementType::Float64 => unreachable!("Atomics validates integer number views"),
    }
}

fn atomics_normalize_bigint(
    element: TypedArrayElementType,
    value: &JsBigInt,
    state: &AtomicsContinuation,
) -> Result<JsBigInt, NativeFailure> {
    let result = match element {
        TypedArrayElementType::BigInt64 => value.as_int_n(64),
        TypedArrayElementType::BigUint64 => value.as_uint_n(64),
        _ => unreachable!("Atomics validates BigInt typed-array element kinds"),
    };
    match result {
        Ok(value) => Ok(value),
        Err(error) => Err(NativeFailure::Abrupt(bigint_exception(
            state.realm,
            &state.origin,
            error,
        )?)),
    }
}

fn atomics_bigint_update(
    method: AtomicsMethod,
    old: &JsBigInt,
    input: &JsBigInt,
    state: &AtomicsContinuation,
) -> Result<JsBigInt, NativeFailure> {
    let result = match method {
        AtomicsMethod::Add => old.add(input),
        AtomicsMethod::And => old.bitand(input),
        AtomicsMethod::Or => old.bitor(input),
        AtomicsMethod::Sub => old.sub(input),
        AtomicsMethod::Xor => old.bitxor(input),
        _ => unreachable!("only read-modify-write Atomics operations reach BigInt update"),
    };
    match result {
        Ok(value) => Ok(value),
        Err(error) => Err(NativeFailure::Abrupt(bigint_exception(
            state.realm,
            &state.origin,
            error,
        )?)),
    }
}

fn atomics_notify(
    state: &AtomicsContinuation,
    count: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    if !matches!(count, StoredValue::Undefined) {
        let _ =
            number_to_integer_or_infinity(operator_to_number(count, state.realm, &state.origin)?);
    }
    Ok(NativeDispatch::Immediate(StoredValue::Number(
        JsNumber::from_i32(0),
    )))
}

fn atomics_wait_expected(
    runtime: &mut Runtime,
    mut state: AtomicsContinuation,
    expected: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let index = state.index.ok_or(EngineFault::RuntimeInvariant {
        message: "Atomics.wait lost its validated index",
    })?;
    let element = runtime
        .typed_array_state(state.object)?
        .copied()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Atomics.wait lost its typed-array state",
        })?
        .element();
    let actual = runtime.typed_array_read_index(state.object, index)?.ok_or(
        EngineFault::RuntimeInvariant {
            message: "Atomics.wait lost its validated indexed element",
        },
    )?;
    if !atomics_wait_matches(element, actual, expected, &state)? {
        return atomics_wait_result("not-equal");
    }
    let timeout = state
        .replacement
        .take()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Atomics.wait lost its timeout argument",
        })?;
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        timeout,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::AtomicsTimeout(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn atomics_wait_matches(
    element: TypedArrayElementType,
    actual: StoredValue,
    expected: StoredValue,
    state: &AtomicsContinuation,
) -> Result<bool, NativeFailure> {
    if element.is_bigint() {
        let StoredValue::BigInt(actual) = actual else {
            return Err(EngineFault::RuntimeInvariant {
                message: "BigInt Atomics.wait read a Number element",
            }
            .into());
        };
        let expected = to_bigint_from_primitive(&expected, state.realm, &state.origin)?;
        return Ok(actual.as_ref() == expected.as_ref());
    }
    let StoredValue::Number(actual) = actual else {
        return Err(EngineFault::RuntimeInvariant {
            message: "number Atomics.wait read a BigInt element",
        }
        .into());
    };
    let expected = JsNumber::from_f64(number_to_integer_or_infinity(operator_to_number(
        expected,
        state.realm,
        &state.origin,
    )?));
    Ok(actual.strict_equals(expected))
}

fn atomics_wait_timeout(timeout: JsNumber) -> Result<NativeDispatch, NativeFailure> {
    let milliseconds = timeout.as_f64();
    if milliseconds.is_finite() && milliseconds > 0.0 {
        let seconds = (milliseconds / 1_000.0).min(std::time::Duration::MAX.as_secs_f64() / 2.0);
        std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
    } else if milliseconds.is_infinite() && milliseconds.is_sign_positive() {
        std::thread::park();
    }
    atomics_wait_result("timed-out")
}

fn atomics_wait_result(value: &str) -> Result<NativeDispatch, NativeFailure> {
    Ok(NativeDispatch::Immediate(StoredValue::String(
        JsString::from_utf8(value)?,
    )))
}

fn atomics_type_error<T>(
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

fn atomics_range_error<T>(
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
