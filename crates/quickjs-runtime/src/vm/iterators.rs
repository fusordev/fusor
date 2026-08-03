/*
 * JavaScript iterator abstract operations derived from QuickJS.
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

//! Resumable synchronous iterator operations and intrinsic iterator methods.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

fn iterator_exception(
    realm: RealmId,
    origin: JsStackFrame,
    kind: ExceptionKind,
    message: &str,
) -> Result<NativeFailure, NativeFailure> {
    Ok(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}

pub(super) fn iterator_getter_call(
    function: FunctionId,
    receiver: StoredValue,
    continuation: NativeContinuation,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    pre_call: Option<NativePreCall>,
) -> Result<NativeDispatch, NativeFailure> {
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(continuation);
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::empty(),
        return_to,
        origin,
        continuations,
        pre_call,
        new_target: None,
        native_caller: None,
    }))
}

fn iterator_method_call(
    function: FunctionId,
    receiver: StoredValue,
    continuation: NativeContinuation,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    iterator_getter_call(function, receiver, continuation, return_to, origin, None)
}

fn iterator_result(
    runtime: &mut Runtime,
    realm: RealmId,
    value: StoredValue,
    done: bool,
) -> Result<NativeDispatch, NativeFailure> {
    Ok(NativeDispatch::Immediate(StoredValue::Object(
        runtime.allocate_iterator_result(realm, value, done)?,
    )))
}

pub(super) fn begin_array_iterator_method(
    runtime: &mut Runtime,
    receiver: StoredValue,
    kind: crate::object::ArrayIteratorKind,
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(receiver, StoredValue::Undefined | StoredValue::Null) {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "cannot convert to object",
        )?);
    }
    let primitive_wrapper = matches!(
        receiver,
        StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
    );
    let additional_objects = 1_usize.saturating_add(usize::from(primitive_wrapper));
    check_execution_limit(
        RuntimeResource::HeapObjects,
        runtime.limits.max_heap_objects,
        usize_to_u64(runtime.objects.len()).saturating_add(usize_to_u64(additional_objects)),
    )?;
    if matches!(receiver, StoredValue::String(_)) {
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            runtime.limits.max_object_properties,
            runtime.object_properties.saturating_add(1),
        )?;
    }
    runtime
        .objects
        .try_reserve(additional_objects)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapObjects,
            additional: additional_objects,
        })?;
    let collection_pending = runtime.collection_pending;
    let mut temporary_wrapper = None;
    let receiver = match receiver {
        StoredValue::Undefined | StoredValue::Null => unreachable!("nullish receiver was rejected"),
        StoredValue::Boolean(value) => {
            let wrapper = runtime.allocate_boxed_boolean(realm, value)?;
            temporary_wrapper = Some(wrapper);
            StoredValue::Object(wrapper)
        }
        StoredValue::Number(value) => {
            let wrapper = runtime.allocate_boxed_number(realm, value)?;
            temporary_wrapper = Some(wrapper);
            StoredValue::Object(wrapper)
        }
        StoredValue::BigInt(value) => {
            let wrapper = runtime.allocate_boxed_bigint(realm, value)?;
            temporary_wrapper = Some(wrapper);
            StoredValue::Object(wrapper)
        }
        StoredValue::String(value) => {
            let wrapper = runtime.allocate_boxed_string(realm, value)?;
            temporary_wrapper = Some(wrapper);
            StoredValue::Object(wrapper)
        }
        StoredValue::Symbol(value) => {
            let wrapper = runtime.allocate_boxed_symbol(realm, value)?;
            temporary_wrapper = Some(wrapper);
            StoredValue::Object(wrapper)
        }
        value @ (StoredValue::Function(_) | StoredValue::Object(_)) => value,
    };
    match runtime.allocate_array_iterator(realm, receiver, kind) {
        Ok(iterator) => Ok(NativeDispatch::Immediate(StoredValue::Object(iterator))),
        Err(error) => {
            if let Some(wrapper) = temporary_wrapper
                && let Some(object) = runtime.objects.remove(wrapper)
            {
                runtime.object_properties = runtime
                    .object_properties
                    .saturating_sub(usize_to_u64(object.record.property_count()));
            }
            runtime.collection_pending = collection_pending;
            Err(error.into())
        }
    }
}

pub(super) fn begin_string_iterator_method(
    runtime: &mut Runtime,
    receiver: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match receiver {
        StoredValue::Undefined | StoredValue::Null => Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "cannot convert to object",
        )?),
        StoredValue::String(string) => Ok(NativeDispatch::Immediate(StoredValue::Object(
            runtime.allocate_string_iterator(realm, string)?,
        ))),
        value => begin_operator_primitive_conversion(
            runtime,
            value,
            OperatorPrimitiveHint::String,
            OperatorPrimitiveTarget::StringIteratorIntrinsic,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the call boundary transfers ownership of the receiver into receiver validation"
)]
pub(super) fn begin_array_iterator_next(
    runtime: &mut Runtime,
    receiver: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(iterator) = receiver else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Array Iterator object expected",
        )?);
    };
    if runtime
        .objects
        .get(iterator)
        .and_then(crate::object::HeapObject::array_iterator_state)
        .is_none()
    {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Array Iterator object expected",
        )?);
    }
    let snapshot = runtime.array_iterator_snapshot(iterator)?;
    let Some(iterated) = snapshot.iterated else {
        return iterator_result(runtime, realm, StoredValue::Undefined, true);
    };
    let state = ArrayIteratorNextContinuation {
        iterator,
        iterated,
        kind: snapshot.kind,
        index: snapshot.next,
        realm,
        stage: ArrayIteratorNextStage::AwaitLength,
        prepared_result: None,
        origin,
    };
    let key = runtime.predefined_property_key(PredefinedAtom::Length);
    charge_iterator_property_lookup(runtime, &state.iterated, execution_budget)?;
    match read_static_property(runtime, realm, &state.iterated, &key)? {
        PropertyReadOutcome::Value(value) => begin_array_iterator_length_conversion(
            runtime,
            state,
            value,
            return_to,
            execution_budget,
        ),
        PropertyReadOutcome::Getter { function, receiver } => iterator_getter_call(
            function,
            receiver,
            NativeContinuation::ArrayIteratorNext(state),
            return_to,
            native_function_host_origin(),
            None,
        ),
        PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            realm,
            state.origin,
            None,
            failure,
        )?)),
    }
}

pub(super) fn advance_array_iterator_next(
    runtime: &mut Runtime,
    state: ArrayIteratorNextContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ArrayIteratorNextStage::AwaitLength => begin_array_iterator_length_conversion(
            runtime,
            state,
            completion,
            return_to,
            execution_budget,
        ),
        ArrayIteratorNextStage::AwaitValue => {
            finish_array_iterator_value(runtime, state, completion)
        }
    }
}

fn begin_array_iterator_length_conversion(
    runtime: &mut Runtime,
    state: ArrayIteratorNextContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::ArrayIteratorLength(state),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_array_iterator_length(
    runtime: &mut Runtime,
    mut state: ArrayIteratorNextContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let number = operator_to_number(value, state.realm, &state.origin)?;
    let length = number_to_uint32(number);
    let live = runtime.array_iterator_snapshot(state.iterator)?;
    state.iterated = live.iterated.unwrap_or(StoredValue::Undefined);
    state.kind = live.kind;
    state.index = live.next;
    if state.index >= length {
        let prepared = runtime.prepare_iterator_result_allocation(state.realm, None)?;
        let result =
            runtime.commit_prepared_iterator_result(prepared, StoredValue::Undefined, true)?;
        runtime.finish_array_iterator(state.iterator)?;
        return Ok(NativeDispatch::Immediate(StoredValue::Object(result)));
    }

    let index = state.index;
    state.prepared_result = Some(runtime.prepare_iterator_result_allocation(
        state.realm,
        matches!(state.kind, crate::object::ArrayIteratorKind::KeyAndValue).then_some(index),
    )?);
    if matches!(state.kind, crate::object::ArrayIteratorKind::Key) {
        let prepared = state
            .prepared_result
            .take()
            .expect("Array iterator result preparation was just installed");
        let result = runtime.commit_prepared_iterator_result(
            prepared,
            StoredValue::Number(JsNumber::from_u32(index)),
            false,
        )?;
        runtime.advance_array_iterator(state.iterator)?;
        return Ok(NativeDispatch::Immediate(StoredValue::Object(result)));
    }
    let Some(index) = ArrayIndex::new(index) else {
        let prepared = state
            .prepared_result
            .take()
            .expect("Array iterator result preparation was just installed");
        let result =
            runtime.commit_prepared_iterator_result(prepared, StoredValue::Undefined, true)?;
        runtime.finish_array_iterator(state.iterator)?;
        return Ok(NativeDispatch::Immediate(StoredValue::Object(result)));
    };
    let key = PropertyKey::from_index(index);
    charge_iterator_property_lookup(runtime, &state.iterated, execution_budget)?;
    match read_static_property(runtime, state.realm, &state.iterated, &key)? {
        PropertyReadOutcome::Value(value) => {
            let iterator = state.iterator;
            let result = finish_array_iterator_value(runtime, state, value)?;
            runtime.advance_array_iterator(iterator)?;
            Ok(result)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            state.stage = ArrayIteratorNextStage::AwaitValue;
            state
                .prepared_result
                .as_mut()
                .expect("Array iterator result preparation was just installed")
                .mark_callback_boundary();
            let origin = state.origin.clone();
            let iterator = state.iterator;
            iterator_getter_call(
                function,
                receiver,
                NativeContinuation::ArrayIteratorNext(state),
                return_to,
                origin,
                Some(NativePreCall::AdvanceArrayIterator(iterator)),
            )
        }
        PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            state.realm,
            state.origin,
            None,
            failure,
        )?)),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the completed continuation is consumed at this terminal boundary"
)]
fn finish_array_iterator_value(
    runtime: &mut Runtime,
    mut state: ArrayIteratorNextContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(prepared) = state.prepared_result.take() else {
        return Err(EngineFault::RuntimeInvariant {
            message: "Array iterator value completion has no prepared result allocation",
        }
        .into());
    };
    let result = runtime.commit_prepared_iterator_result(prepared, value, false)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the call boundary transfers ownership of the receiver into receiver validation"
)]
pub(super) fn begin_string_iterator_next(
    runtime: &mut Runtime,
    receiver: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(iterator) = receiver else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "String Iterator object expected",
        )?);
    };
    if runtime
        .objects
        .get(iterator)
        .and_then(crate::object::HeapObject::string_iterator_state)
        .is_none()
    {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "String Iterator object expected",
        )?);
    }
    let prepared = runtime.prepare_iterator_result_allocation(realm, None)?;
    let (value, done) = match runtime.string_iterator_next(iterator)? {
        Some(value) => (StoredValue::String(value), false),
        None => (StoredValue::Undefined, true),
    };
    let result = runtime.commit_prepared_iterator_result(prepared, value, done)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
}

pub(super) fn begin_for_of_start(
    runtime: &mut Runtime,
    iterable: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = ForOfStartContinuation {
        iterable,
        iterator: None,
        realm,
        stage: ForOfStartStage::IteratorMethod,
        origin,
    };
    read_for_of_start_property(
        runtime,
        state,
        &runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        return_to,
        execution_budget,
    )
}

pub(super) fn advance_for_of_start(
    runtime: &mut Runtime,
    mut state: ForOfStartContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ForOfStartStage::IteratorMethod => {
            let StoredValue::Function(method) = completion else {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "value is not iterable",
                )?);
            };
            let receiver = state.iterable.duplicate();
            state.stage = ForOfStartStage::Iterator;
            let origin = state.origin.clone();
            iterator_method_call(
                method,
                receiver,
                NativeContinuation::ForOfStart(state),
                return_to,
                origin,
            )
        }
        ForOfStartStage::Iterator => {
            if !matches!(
                completion,
                StoredValue::Function(_) | StoredValue::Object(_)
            ) {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "not an object",
                )?);
            }
            state.iterator = Some(completion);
            state.stage = ForOfStartStage::NextMethod;
            read_for_of_start_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        ForOfStartStage::NextMethod => {
            let iterator = state.iterator.ok_or(EngineFault::RuntimeInvariant {
                message: "for-of next lookup completed without an iterator",
            })?;
            Ok(NativeDispatch::ForOfRecord {
                iterator,
                next: completion,
            })
        }
    }
}

fn read_for_of_start_property(
    runtime: &mut Runtime,
    state: ForOfStartContinuation,
    key: &PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (base, property_name) = match state.stage {
        ForOfStartStage::IteratorMethod => (&state.iterable, "Symbol.iterator"),
        ForOfStartStage::NextMethod => {
            let iterator = state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "for-of next lookup has no iterator",
                })?;
            (iterator, "next")
        }
        ForOfStartStage::Iterator => {
            return Err(EngineFault::RuntimeInvariant {
                message: "for-of iterator call stage attempted a property read",
            }
            .into());
        }
    };
    charge_iterator_property_lookup(runtime, base, execution_budget)?;
    match read_static_property(runtime, state.realm, base, key)? {
        PropertyReadOutcome::Value(value) => {
            advance_for_of_start(runtime, state, value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            let origin = state.origin.clone();
            iterator_getter_call(
                function,
                receiver,
                NativeContinuation::ForOfStart(state),
                return_to,
                origin,
                None,
            )
        }
        PropertyReadOutcome::Failed(failure) => {
            let property_name = JsString::from_utf8(property_name)?;
            Err(NativeFailure::Abrupt(property_exception_at(
                state.realm,
                state.origin,
                Some(&property_name),
                failure,
            )?))
        }
    }
}

pub(super) fn begin_for_of_next(
    iterator: StoredValue,
    next: StoredValue,
    realm: RealmId,
    offset: u8,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let function = match &next {
        StoredValue::Function(function) => *function,
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Object(_) => {
            return Err(iterator_exception(
                realm,
                origin,
                ExceptionKind::TypeError,
                "not a function",
            )?);
        }
    };
    execution_budget.charge_instructions(1)?;
    let receiver = iterator.duplicate();
    let state = ForOfNextContinuation {
        iterator,
        next,
        result: None,
        realm,
        stage: ForOfNextStage::Result,
        offset,
        origin: origin.clone(),
    };
    iterator_method_call(
        function,
        receiver,
        NativeContinuation::ForOfNext(state),
        return_to,
        origin,
    )
}

pub(super) fn advance_for_of_next(
    runtime: &mut Runtime,
    mut state: ForOfNextContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ForOfNextStage::Result => {
            if !matches!(
                completion,
                StoredValue::Function(_) | StoredValue::Object(_)
            ) {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "iterator must return an object",
                )?);
            }
            state.result = Some(completion);
            state.stage = ForOfNextStage::Done;
            read_for_of_next_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        ForOfNextStage::Done => {
            if completion.is_truthy() {
                return Ok(NativeDispatch::ForOfStep {
                    value: StoredValue::Undefined,
                    done: true,
                    offset: state.offset,
                });
            }
            state.stage = ForOfNextStage::Value;
            read_for_of_next_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        ForOfNextStage::Value => Ok(NativeDispatch::ForOfStep {
            value: completion,
            done: false,
            offset: state.offset,
        }),
    }
}

fn read_for_of_next_property(
    runtime: &mut Runtime,
    state: ForOfNextContinuation,
    key: &PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let result = state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "for-of result property lookup has no result object",
    })?;
    charge_iterator_property_lookup(runtime, result, execution_budget)?;
    match read_static_property(runtime, state.realm, result, key)? {
        PropertyReadOutcome::Value(value) => {
            advance_for_of_next(runtime, state, value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            let origin = state.origin.clone();
            iterator_getter_call(
                function,
                receiver,
                NativeContinuation::ForOfNext(state),
                return_to,
                origin,
                None,
            )
        }
        PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            state.realm,
            state.origin,
            None,
            failure,
        )?)),
    }
}

pub(super) fn begin_for_of_close(
    runtime: &mut Runtime,
    iterator: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(iterator, StoredValue::Undefined) {
        return Ok(NativeDispatch::ForOfClosed);
    }
    let state = ForOfCloseContinuation {
        iterator,
        realm,
        stage: ForOfCloseStage::AwaitReturnProperty,
        origin,
    };
    read_for_of_return(runtime, state, return_to, execution_budget)
}

fn read_for_of_return(
    runtime: &mut Runtime,
    state: ForOfCloseContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let key = runtime.predefined_property_key(PredefinedAtom::Return);
    charge_iterator_property_lookup(runtime, &state.iterator, execution_budget)?;
    match read_static_property(runtime, state.realm, &state.iterator, &key)? {
        PropertyReadOutcome::Value(value) => advance_for_of_close(state, &value, return_to),
        PropertyReadOutcome::Getter { function, receiver } => {
            let origin = state.origin.clone();
            iterator_getter_call(
                function,
                receiver,
                NativeContinuation::ForOfClose(state),
                return_to,
                origin,
                None,
            )
        }
        PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            state.realm,
            state.origin,
            None,
            failure,
        )?)),
    }
}

pub(super) fn advance_for_of_close(
    mut state: ForOfCloseContinuation,
    completion: &StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ForOfCloseStage::AwaitReturnProperty => match completion {
            StoredValue::Undefined | StoredValue::Null => Ok(NativeDispatch::ForOfClosed),
            StoredValue::Function(function) => {
                let receiver = state.iterator.duplicate();
                state.stage = ForOfCloseStage::AwaitReturnCall;
                let origin = state.origin.clone();
                iterator_method_call(
                    *function,
                    receiver,
                    NativeContinuation::ForOfClose(state),
                    return_to,
                    origin,
                )
            }
            StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => Err(iterator_exception(
                state.realm,
                state.origin,
                ExceptionKind::TypeError,
                "not a function",
            )?),
        },
        ForOfCloseStage::AwaitReturnCall => {
            if matches!(
                completion,
                StoredValue::Function(_) | StoredValue::Object(_)
            ) {
                Ok(NativeDispatch::ForOfClosed)
            } else {
                Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "not an object",
                )?)
            }
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Append starts with explicit destination, cursor, realm, provenance, and execution authority"
)]
pub(super) fn begin_iterator_append(
    runtime: &mut Runtime,
    array: ObjectId,
    next_index: u32,
    iterable: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_iterator_drain(
        runtime,
        IteratorDrain::AppendToArray { array, next_index },
        iterable,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

/// Starts `Object.fromEntries(iterable)`.
///
/// The result object is created before the iterable is touched, so an iterable
/// that throws still allocated it — which no script can observe, since the
/// object is unreachable. Each drained value must be an object, and its `0` and
/// `1` become one own property through `CreateDataPropertyOrThrow`, so the
/// property is fully mutable and a repeated key overwrites the earlier entry
/// (`quickjs.c:40481-40520`).
pub(super) fn begin_object_from_entries(
    runtime: &mut Runtime,
    realm: RealmId,
    iterable: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = runtime.realm_object_prototype(realm)?;
    let target = runtime.allocate_ordinary_object(prototype)?;
    begin_iterator_drain(
        runtime,
        IteratorDrain::EntriesIntoObject {
            target,
            entry: None,
            key: None,
        },
        iterable,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

/// Starts `Object.groupBy(items, callback)`.
///
/// The result has a *null* prototype, so a group key can never collide with an
/// inherited property, which is what separates it from `Object.fromEntries`
/// (`quickjs.c:40700-40740`). The callback is validated before the iterable is
/// touched, so a non-callable one reports `not a function` without probing
/// `Symbol.iterator`.
pub(super) fn begin_object_group_by(
    runtime: &mut Runtime,
    realm: RealmId,
    items: StoredValue,
    callback: &StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(callback) = *callback else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "not a function",
        )?);
    };
    let target = runtime.allocate_ordinary_object_with_optional_prototype(None)?;
    begin_iterator_drain(
        runtime,
        IteratorDrain::GroupIntoObject {
            target,
            callback,
            next_index: 0,
            item: None,
        },
        items,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

/// Starts draining one iterable into the requested destination.
///
/// Everything before the destination's own step is the iterator protocol:
/// probing `Symbol.iterator`, calling it, acquiring `next`, and then reading
/// `done` and `value` per step. An abrupt exit after the iterator exists closes
/// it with `return`.
#[allow(
    clippy::too_many_arguments,
    reason = "a drain starts with explicit destination, realm, provenance, and execution authority"
)]
pub(super) fn begin_iterator_drain(
    runtime: &mut Runtime,
    drain: IteratorDrain,
    iterable: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = IteratorAppendContinuation {
        drain,
        iterable,
        iterator: None,
        next_acquired: false,
        next_method: None,
        result: None,
        realm,
        stage: IteratorAppendStage::AwaitProbe,
        origin,
    };
    read_append_property(
        runtime,
        state,
        runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "the ordered iterator protocol is one explicit resumable state machine"
)]
pub(super) fn advance_iterator_append(
    runtime: &mut Runtime,
    mut state: IteratorAppendContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IteratorAppendStage::AwaitProbe => {
            state.stage = IteratorAppendStage::AwaitMethod;
            read_append_property(
                runtime,
                state,
                runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
                return_to,
                execution_budget,
            )
        }
        IteratorAppendStage::AwaitMethod => {
            let StoredValue::Function(method) = completion else {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "value is not iterable",
                )?);
            };
            let receiver = state.iterable.duplicate();
            state.stage = IteratorAppendStage::AwaitIterator;
            let origin = state.origin.clone();
            iterator_method_call(
                method,
                receiver,
                NativeContinuation::IteratorAppend(state),
                return_to,
                origin,
            )
        }
        IteratorAppendStage::AwaitIterator => {
            if !matches!(
                completion,
                StoredValue::Function(_) | StoredValue::Object(_)
            ) {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "not an object",
                )?);
            }
            state.iterator = Some(completion);
            state.stage = IteratorAppendStage::AwaitNextMethod;
            read_append_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        IteratorAppendStage::AwaitNextMethod => {
            state.next_acquired = true;
            let StoredValue::Function(next) = completion else {
                let pending = iterator_exception(
                    state.realm,
                    state.origin.clone(),
                    ExceptionKind::TypeError,
                    "not a function",
                )?;
                let NativeFailure::Abrupt(pending) = pending else {
                    unreachable!("iterator_exception always returns an abrupt completion")
                };
                return begin_iterator_close(runtime, state, pending, return_to, execution_budget);
            };
            state.next_method = Some(next);
            call_append_next(state, return_to, execution_budget)
        }
        IteratorAppendStage::AwaitNextResult => {
            if !matches!(
                completion,
                StoredValue::Function(_) | StoredValue::Object(_)
            ) {
                let pending = iterator_exception(
                    state.realm,
                    state.origin.clone(),
                    ExceptionKind::TypeError,
                    "iterator must return an object",
                )?;
                let NativeFailure::Abrupt(pending) = pending else {
                    unreachable!("iterator_exception always returns an abrupt completion")
                };
                return begin_iterator_close(runtime, state, pending, return_to, execution_budget);
            }
            state.result = Some(completion);
            state.stage = IteratorAppendStage::AwaitDone;
            read_append_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        IteratorAppendStage::AwaitDone => {
            if completion.is_truthy() {
                // The iterator finished on its own, so no `return` is called.
                return Ok(match state.drain {
                    IteratorDrain::AppendToArray { array, next_index } => NativeDispatch::Pair(
                        StoredValue::Object(array),
                        StoredValue::Number(JsNumber::from_u32(next_index)),
                    ),
                    IteratorDrain::EntriesIntoObject { target, .. }
                    | IteratorDrain::GroupIntoObject { target, .. } => {
                        NativeDispatch::Immediate(StoredValue::Object(target))
                    }
                });
            }
            state.stage = IteratorAppendStage::AwaitValue;
            read_append_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        IteratorAppendStage::AwaitValue => match state.drain {
            IteratorDrain::AppendToArray { .. } => {
                append_drained_value(runtime, state, completion, return_to, execution_budget)
            }
            // An entry must be an object before either of its two indices is
            // read, and a rejected one closes the iterator.
            IteratorDrain::GroupIntoObject { .. } => {
                call_group_callback(state, completion, return_to)
            }
            IteratorDrain::EntriesIntoObject { .. } => {
                if !matches!(
                    completion,
                    StoredValue::Function(_) | StoredValue::Object(_)
                ) {
                    let pending = iterator_exception(
                        state.realm,
                        state.origin.clone(),
                        ExceptionKind::TypeError,
                        "not an object",
                    )?;
                    let NativeFailure::Abrupt(pending) = pending else {
                        unreachable!("iterator_exception always returns an abrupt completion")
                    };
                    return begin_iterator_close(
                        runtime,
                        state,
                        pending,
                        return_to,
                        execution_budget,
                    );
                }
                if let IteratorDrain::EntriesIntoObject { entry, key, .. } = &mut state.drain {
                    *entry = Some(completion);
                    *key = None;
                }
                state.stage = IteratorAppendStage::AwaitEntryKey;
                read_append_property(
                    runtime,
                    state,
                    PropertyKey::from_index(ArrayIndex::new(0).ok_or(
                        EngineFault::RuntimeInvariant {
                            message: "zero is not a valid array index",
                        },
                    )?),
                    return_to,
                    execution_budget,
                )
            }
        },
        IteratorAppendStage::AwaitEntryKey => {
            if let IteratorDrain::EntriesIntoObject { key, .. } = &mut state.drain {
                *key = Some(completion);
            }
            state.stage = IteratorAppendStage::AwaitEntryValue;
            read_append_property(
                runtime,
                state,
                PropertyKey::from_index(ArrayIndex::new(1).ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "one is not a valid array index",
                    },
                )?),
                return_to,
                execution_budget,
            )
        }
        IteratorAppendStage::AwaitEntryValue => {
            define_drained_entry(runtime, state, completion, return_to, execution_budget)
        }
        IteratorAppendStage::AwaitGroupKey => {
            group_drained_item(runtime, state, completion, return_to, execution_budget)
        }
    }
}

/// Calls `Object.groupBy`'s callback with one drained item and its index.
///
/// The callback receives exactly `(item, index)` and an `undefined` receiver,
/// and its result becomes the group key (`quickjs.c:40700-40740`).
fn call_group_callback(
    mut state: IteratorAppendContinuation,
    item: StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let IteratorDrain::GroupIntoObject {
        callback,
        next_index,
        item: held,
        ..
    } = &mut state.drain
    else {
        return Err(EngineFault::RuntimeInvariant {
            message: "group callback reached a non-grouping drain",
        }
        .into());
    };
    let callback = *callback;
    let index = *next_index;
    *next_index = index.saturating_add(1);
    *held = Some(item.duplicate());
    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(2)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 2,
        })?;
    arguments.push(item);
    arguments.push(StoredValue::Number(JsNumber::from_u32(index)));
    state.stage = IteratorAppendStage::AwaitGroupKey;
    let origin = state.origin.clone();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::IteratorAppend(state));
    Ok(NativeDispatch::Call(NativeCall {
        function: callback,
        receiver: StoredValue::Undefined,
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

/// Converts the callback's result into a group key.
fn group_drained_item(
    runtime: &mut Runtime,
    mut state: IteratorAppendContinuation,
    key: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let IteratorDrain::GroupIntoObject { item, .. } = &mut state.drain else {
        return Err(EngineFault::RuntimeInvariant {
            message: "group key reached a non-grouping drain",
        }
        .into());
    };
    let item = item.take().ok_or(EngineFault::RuntimeInvariant {
        message: "group key has no pending item",
    })?;
    // The key converts with `ToPropertyKey`, which can run a user `toString`.
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_property_key_conversion(
        runtime,
        key,
        PropertyKeyTarget::GroupKey {
            drain: Box::new(state),
            item,
        },
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

/// Appends one item to its group once the key has been converted.
///
/// A group is created on first use as a fresh base Array, and the property is
/// fully mutable, so the result reads like an ordinary object even though its
/// prototype is `null`.
pub(super) fn finish_group_key(
    runtime: &mut Runtime,
    mut state: IteratorAppendContinuation,
    item: StoredValue,
    property: StaticPropertyOperand,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let IteratorDrain::GroupIntoObject { target, .. } = &state.drain else {
        return Err(EngineFault::RuntimeInvariant {
            message: "group append reached a non-grouping drain",
        }
        .into());
    };
    let target = *target;
    let group = match runtime
        .object_record(HeapReference::Object(target))?
        .own_property(&property.key)
    {
        Some(OwnProperty::Data {
            value: StoredValue::Object(group),
            ..
        }) => group,
        // A group's own property is only ever the Array this creates, so any
        // other shape means the result object was tampered with, which is
        // impossible: it is unreachable until the drain completes.
        Some(_) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "group property is not a group array",
            }
            .into());
        }
        None => {
            let group = runtime.allocate_array(state.realm, Vec::new())?;
            runtime.append_data_property(
                HeapReference::Object(target),
                property.key,
                PropertyLayout::data(true, true, true),
                StoredValue::Object(group),
            )?;
            group
        }
    };
    let length = runtime
        .array_length(group)?
        .ok_or(EngineFault::RuntimeInvariant {
            message: "group array lost its array state",
        })?;
    let index = ArrayIndex::new(length).ok_or(EngineFault::RuntimeInvariant {
        message: "group array index exceeds the array-index domain",
    })?;
    let work = runtime.preview_array_define_data_property_work(group)?;
    execution_budget.charge_instructions(work)?;
    match runtime.define_array_data_property(
        group,
        PropertyKey::from_index(index),
        PropertyLayout::data(true, true, true),
        item,
    )? {
        ArrayDefineOutcome::Complete => {}
        ArrayDefineOutcome::ReadOnlyLength | ArrayDefineOutcome::NonExtensible => {
            return Err(EngineFault::RuntimeInvariant {
                message: "fresh group array refused an append",
            }
            .into());
        }
    }
    state.result = None;
    call_append_next(state, return_to, execution_budget)
}

/// Appends one drained value to the destination array.
fn append_drained_value(
    runtime: &mut Runtime,
    mut state: IteratorAppendContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let IteratorDrain::AppendToArray { array, next_index } = state.drain else {
        return Err(EngineFault::RuntimeInvariant {
            message: "array append reached a non-array drain",
        }
        .into());
    };
    let Some(index) = ArrayIndex::new(next_index) else {
        let pending = iterator_exception(
            state.realm,
            state.origin.clone(),
            ExceptionKind::RangeError,
            "invalid array length",
        )?;
        let NativeFailure::Abrupt(pending) = pending else {
            unreachable!("iterator_exception always returns an abrupt completion")
        };
        return begin_iterator_close(runtime, state, pending, return_to, execution_budget);
    };
    let work = runtime.preview_array_define_data_property_work(array)?;
    execution_budget.charge_instructions(work)?;
    match runtime.define_array_data_property(
        array,
        PropertyKey::from_index(index),
        PropertyLayout::data(true, true, true),
        value,
    )? {
        ArrayDefineOutcome::Complete => {}
        ArrayDefineOutcome::ReadOnlyLength | ArrayDefineOutcome::NonExtensible => {
            let pending = iterator_exception(
                state.realm,
                state.origin.clone(),
                ExceptionKind::TypeError,
                "cannot append iterator value",
            )?;
            let NativeFailure::Abrupt(pending) = pending else {
                unreachable!("iterator_exception always returns an abrupt completion")
            };
            return begin_iterator_close(runtime, state, pending, return_to, execution_budget);
        }
    }
    state.drain = IteratorDrain::AppendToArray {
        array,
        next_index: next_index.saturating_add(1),
    };
    state.result = None;
    call_append_next(state, return_to, execution_budget)
}

/// Defines one drained entry as an own property of the destination object.
///
/// The key converts with `ToPropertyKey`, which can run a `toString`, so the
/// conversion happens here rather than during the entry's reads. The definition
/// is `CreateDataPropertyOrThrow`, so the property is fully mutable and a later
/// entry with the same key simply overwrites the earlier one.
fn define_drained_entry(
    runtime: &mut Runtime,
    mut state: IteratorAppendContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let IteratorDrain::EntriesIntoObject { target, key, .. } = &mut state.drain else {
        return Err(EngineFault::RuntimeInvariant {
            message: "entry definition reached a non-entries drain",
        }
        .into());
    };
    let _ = target;
    let requested = key.take().ok_or(EngineFault::RuntimeInvariant {
        message: "entry definition has no key",
    })?;
    // The key converts with `ToPropertyKey`, which can run a user `toString`,
    // so the conversion is resumable and the drain rides along inside it.
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_property_key_conversion(
        runtime,
        requested,
        PropertyKeyTarget::EntryKey {
            drain: Box::new(state),
            value,
        },
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

/// Defines one entry's property once its key has been converted.
pub(super) fn finish_entry_key(
    runtime: &mut Runtime,
    mut state: IteratorAppendContinuation,
    value: StoredValue,
    property: StaticPropertyOperand,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let IteratorDrain::EntriesIntoObject { target, .. } = &state.drain else {
        return Err(EngineFault::RuntimeInvariant {
            message: "entry definition reached a non-entries drain",
        }
        .into());
    };
    let target = *target;
    let definition = PropertyDefinition::data(Requested::Present(value), Requested::Present(true))
        .with_enumerable(Requested::Present(true))
        .with_configurable(Requested::Present(true));
    match define_own_property(
        runtime,
        &StoredValue::Object(target),
        property.key,
        &definition,
        execution_budget,
    )? {
        PropertyDefinitionOutcome::Complete => {}
        PropertyDefinitionOutcome::Failed(_) => {
            let pending = iterator_exception(
                state.realm,
                state.origin.clone(),
                ExceptionKind::TypeError,
                "cannot define entry property",
            )?;
            let NativeFailure::Abrupt(pending) = pending else {
                unreachable!("iterator_exception always returns an abrupt completion")
            };
            return begin_iterator_close(runtime, state, pending, return_to, execution_budget);
        }
    }
    if let IteratorDrain::EntriesIntoObject { entry, key, .. } = &mut state.drain {
        *entry = None;
        *key = None;
    }
    state.result = None;
    call_append_next(state, return_to, execution_budget)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "property-key ownership remains local to one resumable Get boundary"
)]
fn read_append_property(
    runtime: &mut Runtime,
    state: IteratorAppendContinuation,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = match state.stage {
        IteratorAppendStage::AwaitProbe | IteratorAppendStage::AwaitMethod => &state.iterable,
        IteratorAppendStage::AwaitNextMethod => {
            state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "iterator next lookup has no iterator",
                })?
        }
        IteratorAppendStage::AwaitDone | IteratorAppendStage::AwaitValue => {
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "iterator result lookup has no result object",
            })?
        }
        // An entry's two indices are read from the entry itself, which the
        // preceding `value` read produced.
        IteratorAppendStage::AwaitEntryKey | IteratorAppendStage::AwaitEntryValue => {
            match &state.drain {
                IteratorDrain::EntriesIntoObject { entry, .. } => {
                    entry.as_ref().ok_or(EngineFault::RuntimeInvariant {
                        message: "entry index lookup has no entry object",
                    })?
                }
                IteratorDrain::AppendToArray { .. } | IteratorDrain::GroupIntoObject { .. } => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "non-entries drain reached an entry index lookup",
                    }
                    .into());
                }
            }
        }
        IteratorAppendStage::AwaitIterator
        | IteratorAppendStage::AwaitNextResult
        | IteratorAppendStage::AwaitGroupKey => {
            return Err(EngineFault::RuntimeInvariant {
                message: "iterator call stage attempted a property read",
            }
            .into());
        }
    };
    charge_iterator_property_lookup(runtime, base, execution_budget)?;
    match read_static_property(runtime, state.realm, base, &key)? {
        PropertyReadOutcome::Value(value) => {
            advance_iterator_append(runtime, state, value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            let origin = state.origin.clone();
            iterator_getter_call(
                function,
                receiver,
                NativeContinuation::IteratorAppend(state),
                return_to,
                origin,
                None,
            )
        }
        PropertyReadOutcome::Failed(failure) => {
            let pending = property_exception_at(state.realm, state.origin.clone(), None, failure)?;
            if state.next_acquired {
                begin_iterator_close(runtime, state, pending, return_to, execution_budget)
            } else {
                Err(NativeFailure::Abrupt(pending))
            }
        }
    }
}

fn call_append_next(
    mut state: IteratorAppendContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget.charge_instructions(1)?;
    let next = state.next_method.ok_or(EngineFault::RuntimeInvariant {
        message: "iterator advance has no retained next method",
    })?;
    let receiver = state
        .iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "iterator advance has no retained iterator",
        })?
        .duplicate();
    state.stage = IteratorAppendStage::AwaitNextResult;
    let origin = state.origin.clone();
    iterator_method_call(
        next,
        receiver,
        NativeContinuation::IteratorAppend(state),
        return_to,
        origin,
    )
}

pub(super) fn resume_iterator_abrupt(
    runtime: &mut Runtime,
    continuation: NativeContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match continuation {
        NativeContinuation::IteratorAppend(state) => {
            if state.next_acquired {
                begin_iterator_close(runtime, state, pending, return_to, execution_budget)
            } else {
                Err(NativeFailure::Abrupt(pending))
            }
        }
        NativeContinuation::IteratorClose(state) => Err(NativeFailure::Abrupt(state.original)),
        _ => Err(EngineFault::RuntimeInvariant {
            message: "non-abrupt native continuation reached iterator abrupt resumption",
        }
        .into()),
    }
}

fn begin_iterator_close(
    runtime: &mut Runtime,
    state: IteratorAppendContinuation,
    original: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let iterator = state.iterator.ok_or(EngineFault::RuntimeInvariant {
        message: "IteratorClose started before iterator acquisition",
    })?;
    begin_exceptional_iterator_close(runtime, iterator, original, return_to, execution_budget)
}

pub(super) fn begin_exceptional_iterator_close(
    runtime: &mut Runtime,
    iterator: StoredValue,
    original: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let close = IteratorCloseContinuation {
        iterator,
        original,
        stage: IteratorCloseStage::AwaitReturnProperty,
    };
    read_iterator_return(runtime, close, return_to, execution_budget)
}

fn read_iterator_return(
    runtime: &mut Runtime,
    close: IteratorCloseContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let key = runtime.predefined_property_key(PredefinedAtom::Return);
    charge_iterator_property_lookup(runtime, &close.iterator, execution_budget)?;
    match read_static_property(runtime, close.original.realm, &close.iterator, &key)? {
        PropertyReadOutcome::Value(value) => advance_iterator_close(close, value, return_to),
        PropertyReadOutcome::Getter { function, receiver } => {
            let origin = close.original.origin.clone();
            iterator_getter_call(
                function,
                receiver,
                NativeContinuation::IteratorClose(close),
                return_to,
                origin,
                None,
            )
        }
        PropertyReadOutcome::Failed(_) => Err(NativeFailure::Abrupt(close.original)),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the close completion is consumed at the pending-exception boundary"
)]
pub(super) fn advance_iterator_close(
    mut close: IteratorCloseContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    match close.stage {
        IteratorCloseStage::AwaitReturnProperty => {
            let StoredValue::Function(function) = completion else {
                return Err(NativeFailure::Abrupt(close.original));
            };
            let receiver = close.iterator.duplicate();
            close.stage = IteratorCloseStage::AwaitReturnCall;
            let origin = close.original.origin.clone();
            iterator_method_call(
                function,
                receiver,
                NativeContinuation::IteratorClose(close),
                return_to,
                origin,
            )
        }
        IteratorCloseStage::AwaitReturnCall => Err(NativeFailure::Abrupt(close.original)),
    }
}

pub(super) fn charge_iterator_property_lookup(
    runtime: &Runtime,
    base: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    if base.heap_reference().is_some() {
        charge_heap_property_lookup(runtime, base, execution_budget)?;
    }
    Ok(())
}
