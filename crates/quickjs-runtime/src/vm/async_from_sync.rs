//! `%AsyncFromSyncIteratorPrototype%` methods and value unwrapping.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

fn allocate_capability(
    runtime: &mut Runtime,
    realm: RealmId,
) -> Result<crate::object::PromiseCapability, NativeFailure> {
    let prototype = runtime.realm_promise_prototype(realm)?;
    let promise = runtime.allocate_promise_with_prototype(HeapReference::Object(prototype))?;
    let (resolve, reject) = runtime.allocate_promise_resolving_functions(promise, realm)?;
    Ok(crate::object::PromiseCapability {
        promise: StoredValue::Object(promise),
        resolve,
        reject,
    })
}

fn capability_promise(
    capability: &crate::object::PromiseCapability,
) -> Result<ObjectId, NativeFailure> {
    let StoredValue::Object(promise) = capability.promise else {
        return Err(EngineFault::RuntimeInvariant {
            message: "AsyncFromSyncIterator capability has no Promise object",
        }
        .into());
    };
    Ok(promise)
}

fn reject_pending(
    runtime: &mut Runtime,
    capability: &crate::object::PromiseCapability,
    pending: PendingException,
) -> Result<NativeDispatch, NativeFailure> {
    let promise = capability_promise(capability)?;
    let reason = pending_exception_value(runtime, pending)?;
    reject_promise(runtime, promise, reason)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(promise)))
}

fn reject_type_error(
    runtime: &mut Runtime,
    capability: &crate::object::PromiseCapability,
    realm: RealmId,
    origin: JsStackFrame,
    message: &'static str,
) -> Result<NativeDispatch, NativeFailure> {
    reject_pending(
        runtime,
        capability,
        PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8(message)?,
            },
            origin,
        },
    )
}

fn one_continuation(
    continuation: NativeContinuation,
) -> Result<Vec<NativeContinuation>, NativeFailure> {
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(continuation);
    Ok(continuations)
}

fn call_sync_method(
    function: FunctionId,
    receiver: StoredValue,
    arguments: CallArguments,
    state: AsyncFromSyncContinuation,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments,
        return_to,
        origin,
        continuations: one_continuation(NativeContinuation::AsyncFromSync(state))?,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

fn supplied_arguments(input: Option<StoredValue>) -> CallArguments {
    input.map_or_else(CallArguments::empty, |value| {
        CallArguments::from_values(vec![value])
    })
}

#[allow(
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    reason = "native method dispatch owns its receiver and carries explicit realm, return, origin, and budget authority"
)]
pub(super) fn begin_async_from_sync_iterator_method(
    runtime: &mut Runtime,
    receiver: StoredValue,
    mut arguments: CallArguments,
    mode: AsyncFromSyncMode,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let capability = allocate_capability(runtime, realm)?;
    let input = arguments.take_first();
    let StoredValue::Object(wrapper) = receiver else {
        return reject_type_error(
            runtime,
            &capability,
            realm,
            origin,
            "not an Async-from-Sync Iterator",
        );
    };
    let Some((iterator, next)) = runtime.async_from_sync_iterator_record(realm, wrapper)? else {
        return reject_type_error(
            runtime,
            &capability,
            realm,
            origin,
            "not an Async-from-Sync Iterator",
        );
    };
    let state = AsyncFromSyncContinuation {
        wrapper,
        iterator,
        next,
        input,
        result: None,
        capability,
        realm,
        mode,
        stage: if mode == AsyncFromSyncMode::Next {
            AsyncFromSyncStage::Call
        } else {
            AsyncFromSyncStage::Method
        },
        done: false,
        origin,
    };
    if mode == AsyncFromSyncMode::Next {
        let StoredValue::Function(next) = state.next else {
            return reject_type_error(
                runtime,
                &state.capability,
                realm,
                state.origin,
                "not a function",
            );
        };
        execution_budget.charge_instructions(1)?;
        let receiver = state.iterator.duplicate();
        let arguments = supplied_arguments(state.input.as_ref().map(StoredValue::duplicate));
        return call_sync_method(next, receiver, arguments, state, return_to);
    }
    read_sync_method(runtime, state, return_to, execution_budget)
}

fn read_sync_method(
    runtime: &mut Runtime,
    state: AsyncFromSyncContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let key = runtime.predefined_property_key(match state.mode {
        AsyncFromSyncMode::Return => PredefinedAtom::Return,
        AsyncFromSyncMode::Throw => PredefinedAtom::Throw,
        AsyncFromSyncMode::Next => {
            return Err(EngineFault::RuntimeInvariant {
                message: "AsyncFromSyncIterator next attempted a dynamic method lookup",
            }
            .into());
        }
    });
    let iterator = state.iterator.duplicate();
    begin_async_from_sync_get(runtime, state, &iterator, key, return_to, execution_budget)
}

fn read_result_property(
    runtime: &mut Runtime,
    state: AsyncFromSyncContinuation,
    key: PredefinedAtom,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let result = state
        .result
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "AsyncFromSyncIterator result lookup has no result object",
        })?
        .duplicate();
    let key = runtime.predefined_property_key(key);
    begin_async_from_sync_get(runtime, state, &result, key, return_to, execution_budget)
}

fn async_from_sync_continuation(state: AsyncFromSyncContinuation) -> NativeContinuation {
    NativeContinuation::AsyncFromSync(state)
}

fn begin_async_from_sync_get(
    runtime: &mut Runtime,
    state: AsyncFromSyncContinuation,
    base: &StoredValue,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_iterator_property_lookup(runtime, base, execution_budget)?;
    let dispatch = match begin_value_get(
        runtime,
        base,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending)) => {
            return resume_async_from_sync_abrupt(
                runtime,
                state,
                pending,
                return_to,
                execution_budget,
            );
        }
        Err(failure) => return Err(failure),
    };
    continue_get_after(
        dispatch,
        state,
        async_from_sync_continuation,
        |state, value| advance_async_from_sync(runtime, state, value, return_to, execution_budget),
        "AsyncFromSyncIterator Get produced a structured result",
    )
}

fn continue_promise_resolve(
    runtime: &mut Runtime,
    state: AsyncFromSyncContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let constructor = runtime.realm_promise_constructor(state.realm)?;
    let dispatch = begin_promise_resolve_with_constructor(
        runtime,
        state.realm,
        constructor,
        value,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_async_from_sync(runtime, state, value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                one_continuation(NativeContinuation::AsyncFromSync(state))?,
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                one_continuation(NativeContinuation::AsyncFromSync(state))?,
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "AsyncFromSyncIterator PromiseResolve produced an invalid dispatch",
        }
        .into()),
    }
}

fn fulfill_missing_return(
    runtime: &mut Runtime,
    state: AsyncFromSyncContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    let promise = capability_promise(&state.capability)?;
    let value = state.input.unwrap_or(StoredValue::Undefined);
    let prepared = runtime.prepare_iterator_result_allocation(state.realm, None)?;
    let result = runtime.commit_prepared_iterator_result(prepared, value, true)?;
    fulfill_promise(runtime, promise, StoredValue::Object(result))?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(promise)))
}

fn finish_missing_throw(
    runtime: &mut Runtime,
    state: &AsyncFromSyncContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    reject_type_error(
        runtime,
        &state.capability,
        state.realm,
        state.origin.clone(),
        "iterator does not have a throw method",
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "the typed AsyncFromSyncIterator state machine keeps specification steps and abrupt boundaries in one exhaustive transition"
)]
pub(super) fn advance_async_from_sync(
    runtime: &mut Runtime,
    mut state: AsyncFromSyncContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        AsyncFromSyncStage::Method => match completion {
            StoredValue::Undefined | StoredValue::Null
                if state.mode == AsyncFromSyncMode::Return =>
            {
                fulfill_missing_return(runtime, state)
            }
            StoredValue::Undefined | StoredValue::Null => {
                state.stage = AsyncFromSyncStage::MissingThrowReturnMethod;
                let key = runtime.predefined_property_key(PredefinedAtom::Return);
                let iterator = state.iterator.duplicate();
                begin_async_from_sync_get(
                    runtime,
                    state,
                    &iterator,
                    key,
                    return_to,
                    execution_budget,
                )
            }
            StoredValue::Function(function) => {
                state.stage = AsyncFromSyncStage::Call;
                let receiver = state.iterator.duplicate();
                let arguments =
                    supplied_arguments(state.input.as_ref().map(StoredValue::duplicate));
                call_sync_method(function, receiver, arguments, state, return_to)
            }
            _ => reject_type_error(
                runtime,
                &state.capability,
                state.realm,
                state.origin,
                "not a function",
            ),
        },
        AsyncFromSyncStage::Call => {
            if !matches!(
                completion,
                StoredValue::Function(_) | StoredValue::Object(_)
            ) {
                return reject_type_error(
                    runtime,
                    &state.capability,
                    state.realm,
                    state.origin,
                    "iterator must return an object",
                );
            }
            state.result = Some(completion);
            state.stage = AsyncFromSyncStage::Done;
            read_result_property(
                runtime,
                state,
                PredefinedAtom::Done,
                return_to,
                execution_budget,
            )
        }
        AsyncFromSyncStage::Done => {
            state.done = completion.is_truthy();
            state.stage = AsyncFromSyncStage::Value;
            read_result_property(
                runtime,
                state,
                PredefinedAtom::Value,
                return_to,
                execution_budget,
            )
        }
        AsyncFromSyncStage::Value => {
            state.stage = AsyncFromSyncStage::PromiseResolve;
            continue_promise_resolve(runtime, state, completion, return_to, execution_budget)
        }
        AsyncFromSyncStage::PromiseResolve => {
            let StoredValue::Object(value_promise) = completion else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "AsyncFromSyncIterator PromiseResolve returned a non-Promise",
                }
                .into());
            };
            let output = capability_promise(&state.capability)?;
            let close_on_rejection = !state.done && state.mode != AsyncFromSyncMode::Return;
            let (unwrap, close) = runtime.allocate_async_from_sync_handlers(
                state.realm,
                state.done,
                close_on_rejection.then(|| state.iterator.duplicate()),
            )?;
            perform_promise_then(
                runtime,
                value_promise,
                Some(unwrap),
                close,
                state.capability,
            )?;
            Ok(NativeDispatch::Immediate(StoredValue::Object(output)))
        }
        AsyncFromSyncStage::MissingThrowReturnMethod => match completion {
            StoredValue::Undefined | StoredValue::Null => finish_missing_throw(runtime, &state),
            StoredValue::Function(function) => {
                state.stage = AsyncFromSyncStage::MissingThrowReturnCall;
                let receiver = state.iterator.duplicate();
                call_sync_method(function, receiver, CallArguments::empty(), state, return_to)
            }
            _ => reject_type_error(
                runtime,
                &state.capability,
                state.realm,
                state.origin,
                "not a function",
            ),
        },
        AsyncFromSyncStage::MissingThrowReturnCall => {
            if !matches!(
                completion,
                StoredValue::Function(_) | StoredValue::Object(_)
            ) {
                return reject_type_error(
                    runtime,
                    &state.capability,
                    state.realm,
                    state.origin,
                    "iterator must return an object",
                );
            }
            finish_missing_throw(runtime, &state)
        }
    }
}

pub(super) fn resume_async_from_sync_abrupt(
    runtime: &mut Runtime,
    state: AsyncFromSyncContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.stage == AsyncFromSyncStage::PromiseResolve
        && !state.done
        && state.mode != AsyncFromSyncMode::Return
    {
        let reason = pending_exception_value(runtime, pending)?;
        let close = AsyncFromSyncCloseContinuation {
            iterator: state.iterator,
            reason,
            target: AsyncFromSyncCloseTarget::Capability(state.capability),
            realm: state.realm,
            stage: AsyncFromSyncCloseStage::ReturnMethod,
            origin: state.origin,
        };
        return read_async_from_sync_close_return(runtime, close, return_to, execution_budget);
    }
    reject_pending(runtime, &state.capability, pending)
}

fn native_capture(
    runtime: &Runtime,
    function: FunctionId,
    key: PredefinedAtom,
) -> Result<StoredValue, NativeFailure> {
    let function = runtime
        .functions
        .get(function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "AsyncFromSyncIterator handler",
            index: function.index(),
            generation: function.generation(),
        })?;
    match function
        .object
        .own_property(&runtime.predefined_property_key(key))
    {
        Some(OwnProperty::Data { value, .. }) => Ok(value),
        Some(OwnProperty::Accessor { .. }) | None => Err(EngineFault::RuntimeInvariant {
            message: "AsyncFromSyncIterator handler lost its capture",
        }
        .into()),
    }
}

pub(super) fn dispatch_async_from_sync_unwrap(
    runtime: &mut Runtime,
    function: FunctionId,
    mut arguments: CallArguments,
    realm: RealmId,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Boolean(done) = native_capture(runtime, function, PredefinedAtom::Done)?
    else {
        return Err(EngineFault::RuntimeInvariant {
            message: "AsyncFromSyncIterator unwrap handler has a non-Boolean done capture",
        }
        .into());
    };
    let prepared = runtime.prepare_iterator_result_allocation(realm, None)?;
    let result = runtime.commit_prepared_iterator_result(
        prepared,
        arguments.take_first_or_undefined(),
        done,
    )?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
}

pub(super) fn dispatch_async_from_sync_close(
    runtime: &mut Runtime,
    function: FunctionId,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let iterator = native_capture(runtime, function, PredefinedAtom::Value)?;
    let state = AsyncFromSyncCloseContinuation {
        iterator,
        reason: arguments.take_first_or_undefined(),
        target: AsyncFromSyncCloseTarget::RejectedPromise,
        realm,
        stage: AsyncFromSyncCloseStage::ReturnMethod,
        origin,
    };
    read_async_from_sync_close_return(runtime, state, return_to, execution_budget)
}

fn rejected_close_promise(
    runtime: &mut Runtime,
    realm: RealmId,
    reason: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = runtime.realm_promise_prototype(realm)?;
    let promise = runtime.allocate_promise_with_prototype(HeapReference::Object(prototype))?;
    reject_promise(runtime, promise, reason)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(promise)))
}

fn finish_async_from_sync_close(
    runtime: &mut Runtime,
    state: AsyncFromSyncCloseContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    match state.target {
        AsyncFromSyncCloseTarget::RejectedPromise => {
            rejected_close_promise(runtime, state.realm, state.reason)
        }
        AsyncFromSyncCloseTarget::Capability(capability) => {
            let promise = capability_promise(&capability)?;
            reject_promise(runtime, promise, state.reason)?;
            Ok(NativeDispatch::Immediate(StoredValue::Object(promise)))
        }
    }
}

fn read_async_from_sync_close_return(
    runtime: &mut Runtime,
    state: AsyncFromSyncCloseContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let key = runtime.predefined_property_key(PredefinedAtom::Return);
    charge_iterator_property_lookup(runtime, &state.iterator, execution_budget)?;
    let dispatch = match begin_value_get(
        runtime,
        &state.iterator,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(_)) => return finish_async_from_sync_close(runtime, state),
        Err(failure) => return Err(failure),
    };
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::AsyncFromSyncClose,
        |state, value| advance_async_from_sync_close(runtime, state, value, return_to),
        "AsyncFromSyncIterator close Get produced a structured result",
    )
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the native continuation dispatcher transfers ownership of completion values at every state-machine boundary"
)]
pub(super) fn advance_async_from_sync_close(
    runtime: &mut Runtime,
    mut state: AsyncFromSyncCloseContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        AsyncFromSyncCloseStage::ReturnMethod => match completion {
            StoredValue::Undefined | StoredValue::Null => {
                finish_async_from_sync_close(runtime, state)
            }
            StoredValue::Function(function) => {
                state.stage = AsyncFromSyncCloseStage::ReturnCall;
                let receiver = state.iterator.duplicate();
                let origin = state.origin.clone();
                Ok(NativeDispatch::Call(NativeCall {
                    function,
                    receiver,
                    arguments: CallArguments::empty(),
                    return_to,
                    origin,
                    continuations: one_continuation(NativeContinuation::AsyncFromSyncClose(state))?,
                    pre_call: None,
                    new_target: None,
                    native_caller: None,
                }))
            }
            _ => finish_async_from_sync_close(runtime, state),
        },
        AsyncFromSyncCloseStage::ReturnCall => finish_async_from_sync_close(runtime, state),
    }
}

pub(super) fn resume_async_from_sync_close_abrupt(
    runtime: &mut Runtime,
    state: AsyncFromSyncCloseContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    finish_async_from_sync_close(runtime, state)
}
