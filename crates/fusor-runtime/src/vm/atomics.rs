//! `%Atomics%` operations over integer typed arrays and shared waiter lists.
//!
//! Observable coercions use the ordinary resumable `ToPrimitive` machinery.
//! Shared bytes and waiters then enter one data-block critical section; Tokio
//! supplies timeout signals only, while this runtime owns FIFO selection and
//! Promise settlement order.

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
    let replacement = matches!(
        method,
        AtomicsMethod::CompareExchange | AtomicsMethod::Wait | AtomicsMethod::WaitAsync
    )
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
        return atomics_notify(runtime, &state, value);
    }
    if matches!(state.method, AtomicsMethod::Wait | AtomicsMethod::WaitAsync) {
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
    runtime: &mut Runtime,
    state: &AtomicsContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let timeout = operator_to_number(value, state.realm, &state.origin)?;
    atomics_do_wait(runtime, state, timeout)
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
    let result = if element.is_bigint() {
        let input = to_bigint_from_primitive(&value, state.realm, &state.origin)?;
        let replacement = replacement
            .map(|replacement| to_bigint_from_primitive(&replacement, state.realm, &state.origin))
            .transpose()?;
        runtime.with_typed_array_element_mut(state.object, index, |data, byte_index, element| {
            atomics_apply_bigint_locked(data, byte_index, element, state, input, replacement)
        })?
    } else {
        let input = atomics_to_integer_number(value, state.realm, &state.origin)?;
        let replacement = replacement
            .map(|replacement| atomics_to_integer_number(replacement, state.realm, &state.origin))
            .transpose()?;
        runtime.with_typed_array_element_mut(state.object, index, |data, byte_index, element| {
            atomics_apply_number_locked(data, byte_index, element, state, input, replacement)
        })?
    }
    .ok_or(EngineFault::RuntimeInvariant {
        message: "validated Atomics operation lost its indexed element",
    })??;
    Ok(NativeDispatch::Immediate(result))
}

fn atomics_apply_number_locked(
    data: &mut [u8],
    byte_index: usize,
    element: TypedArrayElementType,
    state: &AtomicsContinuation,
    input: JsNumber,
    replacement: Option<JsNumber>,
) -> Result<StoredValue, NativeFailure> {
    let StoredValue::Number(old) = typed_array_read_element(data, byte_index, element)? else {
        return Err(EngineFault::RuntimeInvariant {
            message: "number Atomics operation read a BigInt element",
        }
        .into());
    };
    let (result, stored) = match state.method {
        AtomicsMethod::Store => (StoredValue::Number(input), Some(input)),
        AtomicsMethod::Exchange => (StoredValue::Number(old), Some(input)),
        AtomicsMethod::CompareExchange => {
            let replacement = replacement.ok_or(EngineFault::RuntimeInvariant {
                message: "Atomics.compareExchange missing replacement value",
            })?;
            let stored = if atomics_normalize_number(element, old)
                .strict_equals(atomics_normalize_number(element, input))
            {
                Some(replacement)
            } else {
                None
            };
            (StoredValue::Number(old), stored)
        }
        AtomicsMethod::Add
        | AtomicsMethod::And
        | AtomicsMethod::Or
        | AtomicsMethod::Sub
        | AtomicsMethod::Xor => {
            let updated = atomics_number_update(state.method, element, old, input);
            (StoredValue::Number(old), Some(updated))
        }
        AtomicsMethod::IsLockFree
        | AtomicsMethod::Load
        | AtomicsMethod::Notify
        | AtomicsMethod::Wait
        | AtomicsMethod::WaitAsync
        | AtomicsMethod::Pause => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-mutating Atomics method reached value application",
            }
            .into());
        }
    };
    if let Some(stored) = stored {
        atomics_write_locked(
            data,
            byte_index,
            &typed_array_write_element(element, TypedArrayElementValue::Number(stored)),
        )?;
    }
    Ok(result)
}

fn atomics_apply_bigint_locked(
    data: &mut [u8],
    byte_index: usize,
    element: TypedArrayElementType,
    state: &AtomicsContinuation,
    input: Arc<JsBigInt>,
    replacement: Option<Arc<JsBigInt>>,
) -> Result<StoredValue, NativeFailure> {
    let StoredValue::BigInt(old) = typed_array_read_element(data, byte_index, element)? else {
        return Err(EngineFault::RuntimeInvariant {
            message: "BigInt Atomics operation read a Number element",
        }
        .into());
    };
    let (result, stored) = match state.method {
        AtomicsMethod::Store => (StoredValue::BigInt(Arc::clone(&input)), Some(input)),
        AtomicsMethod::Exchange => (StoredValue::BigInt(old), Some(input)),
        AtomicsMethod::CompareExchange => {
            let replacement = replacement.ok_or(EngineFault::RuntimeInvariant {
                message: "Atomics.compareExchange missing replacement value",
            })?;
            let expected = atomics_normalize_bigint(element, input.as_ref(), state)?;
            let stored = (old.as_ref() == &expected).then_some(replacement);
            (StoredValue::BigInt(old), stored)
        }
        AtomicsMethod::Add
        | AtomicsMethod::And
        | AtomicsMethod::Or
        | AtomicsMethod::Sub
        | AtomicsMethod::Xor => {
            let updated = atomics_bigint_update(state.method, old.as_ref(), input.as_ref(), state)?;
            (StoredValue::BigInt(old), Some(Arc::new(updated)))
        }
        AtomicsMethod::IsLockFree
        | AtomicsMethod::Load
        | AtomicsMethod::Notify
        | AtomicsMethod::Wait
        | AtomicsMethod::WaitAsync
        | AtomicsMethod::Pause => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-mutating Atomics method reached value application",
            }
            .into());
        }
    };
    if let Some(stored) = stored {
        atomics_write_locked(
            data,
            byte_index,
            &typed_array_write_element(element, TypedArrayElementValue::BigInt(stored.as_ref())),
        )?;
    }
    Ok(result)
}

fn atomics_write_locked(
    data: &mut [u8],
    byte_index: usize,
    bytes: &[u8],
) -> Result<(), NativeFailure> {
    let end = byte_index
        .checked_add(bytes.len())
        .ok_or(EngineFault::RuntimeInvariant {
            message: "atomic element write range overflowed",
        })?;
    let target = data
        .get_mut(byte_index..end)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "atomic element write escaped its validated backing store",
        })?;
    target.copy_from_slice(bytes);
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
    runtime: &mut Runtime,
    state: &AtomicsContinuation,
    count: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let count = atomics_notify_count(count, state)?;
    let index = state.index.ok_or(EngineFault::RuntimeInvariant {
        message: "Atomics.notify lost its validated index",
    })?;
    let typed_array =
        runtime
            .typed_array_state(state.object)?
            .copied()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Atomics.notify lost its typed-array state",
            })?;
    let buffer =
        runtime
            .array_buffer_state(typed_array.buffer())?
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Atomics.notify lost its backing buffer",
            })?;
    let Some(block) = buffer.shared_data_block().map(Arc::clone) else {
        return Ok(NativeDispatch::Immediate(StoredValue::Number(
            JsNumber::from_i32(0),
        )));
    };
    let byte_index =
        typed_array_element_byte_index(typed_array.byte_offset(), index, typed_array.element())?;
    let direct_token = next_atomics_wake_token();
    let notified = block.notify(byte_index, count, runtime.atomics_agent_id, direct_token);
    // The current agent resolves its own async waiters during Notify rather
    // than deferring settlement until the host checkpoint. Promise reactions
    // remain queued normally, preserving their relative FIFO position.
    runtime.settle_notified_atomics_waiters(direct_token)?;
    Ok(NativeDispatch::Immediate(StoredValue::Number(
        atomics_waiter_count_number(notified),
    )))
}

#[expect(
    clippy::cast_precision_loss,
    reason = "ECMAScript reports a mathematical waiter count as the nearest binary64 Number"
)]
fn atomics_waiter_count_number(count: usize) -> JsNumber {
    JsNumber::from_f64(count as f64)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "ToIntegerOrInfinity is clamped to the host waiter-count domain"
)]
fn atomics_notify_count(
    count: StoredValue,
    state: &AtomicsContinuation,
) -> Result<usize, NativeFailure> {
    if matches!(count, StoredValue::Undefined) {
        return Ok(usize::MAX);
    }
    let count =
        number_to_integer_or_infinity(operator_to_number(count, state.realm, &state.origin)?);
    if count <= 0.0 {
        Ok(0)
    } else if count.is_infinite() {
        Ok(usize::MAX)
    } else {
        Ok(count as usize)
    }
}

fn atomics_wait_expected(
    runtime: &mut Runtime,
    mut state: AtomicsContinuation,
    expected: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let element = runtime
        .typed_array_state(state.object)?
        .copied()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Atomics.wait lost its typed-array state",
        })?
        .element();
    state.value = Some(atomics_normalize_wait_expected(element, expected, &state)?);
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

fn atomics_normalize_wait_expected(
    element: TypedArrayElementType,
    expected: StoredValue,
    state: &AtomicsContinuation,
) -> Result<StoredValue, NativeFailure> {
    if element.is_bigint() {
        let expected = to_bigint_from_primitive(&expected, state.realm, &state.origin)?;
        let expected = atomics_normalize_bigint(element, expected.as_ref(), state)?;
        return Ok(StoredValue::BigInt(Arc::new(expected)));
    }
    let expected = number_to_int32(operator_to_number(expected, state.realm, &state.origin)?);
    Ok(StoredValue::Number(JsNumber::from_i32(expected)))
}

fn atomics_do_wait(
    runtime: &mut Runtime,
    state: &AtomicsContinuation,
    timeout: JsNumber,
) -> Result<NativeDispatch, NativeFailure> {
    let (block, byte_index, element) = atomics_wait_location(runtime, state)?;
    let expected = state.value.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "Atomics wait lost its converted expected value",
    })?;
    let expected_bytes = atomics_wait_expected_bytes(element, expected)?;
    let timeout = AtomicsTimeout::from_number(timeout);
    if timeout.is_zero() {
        let equal = block.with_bytes(|bytes| {
            byte_index
                .checked_add(expected_bytes.len())
                .and_then(|end| bytes.get(byte_index..end))
                == Some(expected_bytes.as_slice())
        });
        return if equal {
            atomics_wait_immediate(runtime, state, "timed-out")
        } else {
            atomics_wait_immediate(runtime, state, "not-equal")
        };
    }
    match state.method {
        AtomicsMethod::Wait => {
            atomics_wait_blocking(block.as_ref(), byte_index, &expected_bytes, timeout)
        }
        AtomicsMethod::WaitAsync => {
            atomics_wait_async(runtime, state, &block, byte_index, &expected_bytes, timeout)
        }
        _ => Err(EngineFault::RuntimeInvariant {
            message: "non-wait Atomics method reached DoWait",
        }
        .into()),
    }
}

fn atomics_wait_location(
    runtime: &Runtime,
    state: &AtomicsContinuation,
) -> Result<(Arc<SharedDataBlock>, usize, TypedArrayElementType), NativeFailure> {
    let index = state.index.ok_or(EngineFault::RuntimeInvariant {
        message: "Atomics wait lost its validated index",
    })?;
    let typed_array =
        runtime
            .typed_array_state(state.object)?
            .copied()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Atomics wait lost its typed-array state",
            })?;
    let buffer =
        runtime
            .array_buffer_state(typed_array.buffer())?
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Atomics wait lost its backing buffer",
            })?;
    let block = buffer
        .shared_data_block()
        .cloned()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Atomics wait lost its Shared Data Block",
        })?;
    let byte_index =
        typed_array_element_byte_index(typed_array.byte_offset(), index, typed_array.element())?;
    Ok((block, byte_index, typed_array.element()))
}

fn atomics_wait_expected_bytes(
    element: TypedArrayElementType,
    expected: &StoredValue,
) -> Result<Vec<u8>, NativeFailure> {
    match expected {
        StoredValue::Number(value) => Ok(typed_array_write_element(
            element,
            TypedArrayElementValue::Number(*value),
        )),
        StoredValue::BigInt(value) => Ok(typed_array_write_element(
            element,
            TypedArrayElementValue::BigInt(value.as_ref()),
        )),
        _ => Err(EngineFault::RuntimeInvariant {
            message: "Atomics wait expected value was not normalized",
        }
        .into()),
    }
}

fn atomics_wait_blocking(
    block: &SharedDataBlock,
    byte_index: usize,
    expected: &[u8],
    timeout: AtomicsTimeout,
) -> Result<NativeDispatch, NativeFailure> {
    let waiter_id = next_atomics_waiter_id();
    let waiter_state = Arc::new(AtomicsWaiterState::pending());
    let blocking = Arc::new(BlockingWaiter::new(Arc::clone(&waiter_state)));
    let registered = block.register_waiter_if_equal(
        byte_index,
        expected,
        SharedWaiter {
            id: waiter_id,
            byte_index,
            state: waiter_state,
            wake: SharedWaiterWake::Blocking(Arc::clone(&blocking)),
        },
    )?;
    if !registered {
        return atomics_wait_result("not-equal");
    }
    let outcome = blocking.wait(timeout.duration());
    block.remove_waiter(byte_index, waiter_id);
    atomics_wait_result(match outcome {
        AtomicsWakeResult::Ok => "ok",
        AtomicsWakeResult::TimedOut => "timed-out",
    })
}

fn atomics_wait_async(
    runtime: &mut Runtime,
    state: &AtomicsContinuation,
    block: &Arc<SharedDataBlock>,
    byte_index: usize,
    expected: &[u8],
    timeout: AtomicsTimeout,
) -> Result<NativeDispatch, NativeFailure> {
    let registration = runtime.register_async_atomics_waiter(
        block,
        byte_index,
        expected,
        state.realm,
        timeout.duration(),
    )?;
    let Some((waiter_id, promise)) = registration else {
        return atomics_wait_async_result(
            runtime,
            state.realm,
            false,
            StoredValue::String(JsString::from_utf8("not-equal")?),
        );
    };
    match atomics_wait_async_result(runtime, state.realm, true, StoredValue::Object(promise)) {
        Ok(dispatch) => Ok(dispatch),
        Err(error) => {
            runtime.cancel_atomics_waiter(waiter_id);
            Err(error)
        }
    }
}

fn atomics_wait_immediate(
    runtime: &mut Runtime,
    state: &AtomicsContinuation,
    result: &str,
) -> Result<NativeDispatch, NativeFailure> {
    if state.method == AtomicsMethod::WaitAsync {
        atomics_wait_async_result(
            runtime,
            state.realm,
            false,
            StoredValue::String(JsString::from_utf8(result)?),
        )
    } else {
        atomics_wait_result(result)
    }
}

fn atomics_wait_async_result(
    runtime: &mut Runtime,
    realm: RealmId,
    asynchronous: bool,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let result = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    let async_key = runtime.property_key_from_string(&JsString::from_utf8("async")?)?;
    runtime.append_data_property(
        HeapReference::Object(result),
        async_key,
        PropertyLayout::data(true, true, true),
        StoredValue::Boolean(asynchronous),
    )?;
    runtime.append_data_property(
        HeapReference::Object(result),
        runtime.predefined_property_key(PredefinedAtom::Value),
        PropertyLayout::data(true, true, true),
        value,
    )?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
}

#[derive(Clone, Copy)]
enum AtomicsTimeout {
    Zero,
    Finite(std::time::Duration),
    Infinite,
}

impl AtomicsTimeout {
    fn from_number(timeout: JsNumber) -> Self {
        let milliseconds = timeout.as_f64();
        if milliseconds.is_nan() || (milliseconds.is_infinite() && milliseconds.is_sign_positive())
        {
            return Self::Infinite;
        }
        if milliseconds <= 0.0 {
            return Self::Zero;
        }
        let seconds = (milliseconds / 1_000.0).min(std::time::Duration::MAX.as_secs_f64() / 2.0);
        Self::Finite(std::time::Duration::from_secs_f64(seconds))
    }

    const fn is_zero(self) -> bool {
        matches!(self, Self::Zero)
    }

    const fn duration(self) -> Option<std::time::Duration> {
        match self {
            Self::Finite(duration) => Some(duration),
            Self::Zero => Some(std::time::Duration::ZERO),
            Self::Infinite => None,
        }
    }
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
