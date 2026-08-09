//! Resumable object-environment lookup for sloppy `with` statements.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[derive(Clone, Copy)]
enum WithGetStage {
    HasBinding,
    GetUnscopables,
    GetBlocked,
    HasValue,
    GetValue,
    DeleteValue,
}

#[derive(Clone, Copy)]
enum WithBindingOperation {
    Get,
    GetReference,
    Delete,
}

pub(super) struct WithGetContinuation {
    object: StoredValue,
    unscopables: Option<StoredValue>,
    key: PropertyKey,
    name: JsString,
    is_with: bool,
    strict: bool,
    operation: WithBindingOperation,
    realm: RealmId,
    origin: JsStackFrame,
    stage: WithGetStage,
}

impl WithGetContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(u64::from(self.unscopables.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.object, mark);
        if let Some(unscopables) = &self.unscopables {
            trace_stored_value_root(unscopables, mark);
        }
    }
}

fn with_reference(state: &WithGetContinuation) -> Result<HeapReference, NativeFailure> {
    state
        .object
        .heap_reference()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "with-object environment retained a non-object binding",
        })
        .map_err(Into::into)
}

fn with_boolean(completion: &StoredValue) -> Result<bool, NativeFailure> {
    let StoredValue::Boolean(value) = completion else {
        return Err(EngineFault::RuntimeInvariant {
            message: "with-object [[HasProperty]] returned a non-Boolean completion",
        }
        .into());
    };
    Ok(*value)
}

fn with_not_found(operation: WithBindingOperation) -> NativeDispatch {
    let status = match operation {
        WithBindingOperation::Get | WithBindingOperation::Delete => StoredValue::Boolean(false),
        WithBindingOperation::GetReference => StoredValue::Undefined,
    };
    NativeDispatch::Pair(status, StoredValue::Undefined)
}

fn with_resolved(state: &WithGetContinuation, value: StoredValue) -> NativeDispatch {
    let status = match state.operation {
        WithBindingOperation::Get | WithBindingOperation::Delete => StoredValue::Boolean(true),
        WithBindingOperation::GetReference => state.object.duplicate(),
    };
    NativeDispatch::Pair(status, value)
}

fn continue_with_get_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: WithGetContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_with_get(runtime, state, value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::WithGet(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::WithGet(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "with-object internal method returned a structured completion",
        }
        .into()),
    }
}

fn begin_binding_operation(
    runtime: &mut Runtime,
    mut state: WithGetContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.unscopables = None;
    let dispatch = match state.operation {
        WithBindingOperation::Get | WithBindingOperation::GetReference => {
            state.stage = WithGetStage::HasValue;
            begin_internal_has(
                runtime,
                with_reference(&state)?,
                state.key.clone(),
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            )?
        }
        WithBindingOperation::Delete => {
            state.stage = WithGetStage::DeleteValue;
            begin_internal_delete(
                runtime,
                with_reference(&state)?,
                state.key.clone(),
                false,
                true,
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            )?
        }
    };
    continue_with_get_after(runtime, dispatch, state, return_to, execution_budget)
}

#[allow(
    clippy::too_many_arguments,
    reason = "with lookup carries explicit object-environment, source, continuation, and budget authority"
)]
fn begin_with_binding_operation(
    runtime: &mut Runtime,
    object: StoredValue,
    key: PropertyKey,
    name: JsString,
    is_with: bool,
    strict: bool,
    operation: WithBindingOperation,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let reference = object
        .heap_reference()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "with_get_var operand is not an object",
        })?;
    let state = WithGetContinuation {
        object,
        unscopables: None,
        key: key.clone(),
        name,
        is_with,
        strict,
        operation,
        realm,
        origin: origin.clone(),
        stage: WithGetStage::HasBinding,
    };
    let dispatch = begin_internal_has(
        runtime,
        reference,
        key,
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    continue_with_get_after(runtime, dispatch, state, return_to, execution_budget)
}

#[allow(
    clippy::too_many_arguments,
    reason = "with lookup carries explicit object-environment, source, continuation, and budget authority"
)]
pub(super) fn begin_with_get(
    runtime: &mut Runtime,
    object: StoredValue,
    key: PropertyKey,
    name: JsString,
    is_with: bool,
    strict: bool,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_with_binding_operation(
        runtime,
        object,
        key,
        name,
        is_with,
        strict,
        WithBindingOperation::Get,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "with lookup carries explicit object-environment, source, continuation, and budget authority"
)]
pub(super) fn begin_with_get_reference(
    runtime: &mut Runtime,
    object: StoredValue,
    key: PropertyKey,
    name: JsString,
    is_with: bool,
    strict: bool,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_with_binding_operation(
        runtime,
        object,
        key,
        name,
        is_with,
        strict,
        WithBindingOperation::GetReference,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "with lookup carries explicit object-environment, source, continuation, and budget authority"
)]
pub(super) fn begin_with_delete(
    runtime: &mut Runtime,
    object: StoredValue,
    key: PropertyKey,
    name: JsString,
    is_with: bool,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_with_binding_operation(
        runtime,
        object,
        key,
        name,
        is_with,
        false,
        WithBindingOperation::Delete,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn advance_with_get(
    runtime: &mut Runtime,
    mut state: WithGetContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        WithGetStage::HasBinding => {
            if !with_boolean(&completion)? {
                return Ok(with_not_found(state.operation));
            }
            if !state.is_with {
                return begin_binding_operation(runtime, state, return_to, execution_budget);
            }
            state.stage = WithGetStage::GetUnscopables;
            let reference = with_reference(&state)?;
            let dispatch = begin_internal_get(
                runtime,
                reference,
                state.object.duplicate(),
                runtime.predefined_symbol_property_key(PredefinedAtom::SymbolUnscopables),
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            )?;
            continue_with_get_after(runtime, dispatch, state, return_to, execution_budget)
        }
        WithGetStage::GetUnscopables => {
            let Some(reference) = completion.heap_reference() else {
                return begin_binding_operation(runtime, state, return_to, execution_budget);
            };
            state.stage = WithGetStage::GetBlocked;
            state.unscopables = Some(completion);
            let receiver = state
                .unscopables
                .as_ref()
                .expect("stored immediately above")
                .duplicate();
            let dispatch = begin_internal_get(
                runtime,
                reference,
                receiver,
                state.key.clone(),
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            )?;
            continue_with_get_after(runtime, dispatch, state, return_to, execution_budget)
        }
        WithGetStage::GetBlocked => {
            if completion.is_truthy() {
                return Ok(with_not_found(state.operation));
            }
            begin_binding_operation(runtime, state, return_to, execution_budget)
        }
        WithGetStage::HasValue => {
            if !with_boolean(&completion)? {
                if state.strict {
                    return Err(NativeFailure::Abrupt(PendingException {
                        realm: state.realm,
                        payload: PendingExceptionPayload::EngineError {
                            kind: ExceptionKind::ReferenceError,
                            message: named_property_message("'", &state.name, "' is not defined")?,
                        },
                        origin: state.origin,
                    }));
                }
                return Ok(with_resolved(&state, StoredValue::Undefined));
            }
            state.stage = WithGetStage::GetValue;
            let reference = with_reference(&state)?;
            let dispatch = begin_internal_get(
                runtime,
                reference,
                state.object.duplicate(),
                state.key.clone(),
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            )?;
            continue_with_get_after(runtime, dispatch, state, return_to, execution_budget)
        }
        WithGetStage::GetValue | WithGetStage::DeleteValue => Ok(with_resolved(&state, completion)),
    }
}
