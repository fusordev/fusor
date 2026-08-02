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

//! Native intrinsic dispatch and resumable Function.prototype.apply execution.

use super::instanceof::{advance_instance_of, begin_function_has_instance};

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[allow(
    clippy::large_enum_variant,
    reason = "boxing a Frame would introduce an unaccounted infallible allocation in the interpreter path"
)]
pub(super) enum NativeDispatch {
    Immediate(StoredValue),
    Pair(StoredValue, StoredValue),
    ForOfRecord {
        iterator: StoredValue,
        next: StoredValue,
    },
    ForOfStep {
        value: StoredValue,
        done: bool,
        offset: u8,
    },
    ForOfClosed,
    CopyDataPropertiesDone,
    Frame(Frame),
    Call(NativeCall),
}

pub(super) enum NativeFailure {
    Abrupt(PendingException),
    AbruptAfterTransient(PendingException),
    Execution(ExecutionError),
}

impl From<ExecutionError> for NativeFailure {
    fn from(error: ExecutionError) -> Self {
        Self::Execution(error)
    }
}

impl From<JsStringError> for NativeFailure {
    fn from(error: JsStringError) -> Self {
        Self::Execution(error.into())
    }
}

impl From<crate::AtomError> for NativeFailure {
    fn from(error: crate::AtomError) -> Self {
        Self::Execution(error.into())
    }
}

impl From<EngineFault> for NativeFailure {
    fn from(error: EngineFault) -> Self {
        Self::Execution(error.into())
    }
}

pub(super) fn native_continuation_values(continuations: &[NativeContinuation]) -> u64 {
    continuations.iter().fold(0_u64, |total, continuation| {
        total.saturating_add(continuation.retained_values())
    })
}

pub(super) fn active_execution_frames(frames: &[Frame]) -> usize {
    frames.iter().fold(frames.len(), |total, frame| {
        total.saturating_add(frame.native_returns.len())
    })
}

pub(super) fn attach_native_continuations(
    frame: &mut Frame,
    mut outer: Vec<NativeContinuation>,
) -> Result<(), NativeFailure> {
    if outer.is_empty() {
        return Ok(());
    }
    // Every frame returned by an unresolved native dispatch is freshly
    // created. Continuations are attached exactly once at the resolver
    // boundary, so this reservation is always for zero additional elements
    // and cannot fail after a dynamic root commits its environment.
    debug_assert!(frame.native_returns.is_empty());
    let retained_values = native_continuation_values(&outer);
    outer.try_reserve(frame.native_returns.len()).map_err(|_| {
        ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: frame.native_returns.len(),
        }
    })?;
    outer.append(&mut frame.native_returns);
    frame.native_returns = outer;
    frame.reserved_values = frame.reserved_values.saturating_add(retained_values);
    Ok(())
}

pub(super) fn prepend_native_continuations(
    call: &mut NativeCall,
    mut outer: Vec<NativeContinuation>,
) -> Result<(), NativeFailure> {
    if outer.is_empty() {
        return Ok(());
    }
    outer
        .try_reserve(call.continuations.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: call.continuations.len(),
        })?;
    outer.append(&mut call.continuations);
    call.continuations = outer;
    Ok(())
}

pub(super) fn take_iterator_abrupt_handler(
    continuations: &mut Vec<NativeContinuation>,
) -> Option<NativeContinuation> {
    let index = continuations.iter().rposition(|continuation| {
        matches!(
            continuation,
            NativeContinuation::AggregateError(_)
                | NativeContinuation::FromEntries(_)
                | NativeContinuation::GroupBy(_)
                | NativeContinuation::IteratorAppend(_)
                | NativeContinuation::IteratorClose(_)
        )
    })?;
    let handler = continuations.remove(index);
    continuations.truncate(index);
    Some(handler)
}

pub(super) fn resume_iterator_abrupt_continuations(
    runtime: &mut Runtime,
    mut continuations: Vec<NativeContinuation>,
    mut pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        let Some(handler) = take_iterator_abrupt_handler(&mut continuations) else {
            return Err(NativeFailure::Abrupt(pending));
        };
        let resumed = match handler {
            NativeContinuation::AggregateError(state) => {
                resume_aggregate_error_abrupt(runtime, state, pending, return_to, execution_budget)
            }
            NativeContinuation::FromEntries(state) => {
                resume_from_entries_abrupt(runtime, *state, pending, return_to, execution_budget)
            }
            NativeContinuation::GroupBy(state) => {
                resume_group_by_abrupt(runtime, *state, pending, return_to, execution_budget)
            }
            handler => {
                resume_iterator_abrupt(runtime, handler, pending, return_to, execution_budget)
            }
        };
        match resumed {
            Ok(mut dispatch) => {
                match &mut dispatch {
                    NativeDispatch::Frame(frame) => {
                        attach_native_continuations(frame, continuations)?;
                    }
                    NativeDispatch::Call(call) => {
                        prepend_native_continuations(call, continuations)?;
                    }
                    NativeDispatch::Immediate(_)
                    | NativeDispatch::Pair(_, _)
                    | NativeDispatch::ForOfRecord { .. }
                    | NativeDispatch::ForOfStep { .. }
                    | NativeDispatch::ForOfClosed
                    | NativeDispatch::CopyDataPropertiesDone
                        if !continuations.is_empty() =>
                    {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "iterator abrupt completion skipped an outer continuation",
                        }
                        .into());
                    }
                    NativeDispatch::Immediate(_)
                    | NativeDispatch::Pair(_, _)
                    | NativeDispatch::ForOfRecord { .. }
                    | NativeDispatch::ForOfStep { .. }
                    | NativeDispatch::ForOfClosed
                    | NativeDispatch::CopyDataPropertiesDone => {}
                }
                return Ok(dispatch);
            }
            Err(NativeFailure::Abrupt(next) | NativeFailure::AbruptAfterTransient(next)) => {
                pending = next;
            }
            Err(NativeFailure::Execution(error)) => {
                return Err(NativeFailure::Execution(error));
            }
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "resuming a native abstract operation needs the same explicit execution authority and budgets as its originating call"
)]
pub(super) fn resume_native_continuations(
    runtime: &mut Runtime,
    mut continuations: Vec<NativeContinuation>,
    mut value: StoredValue,
    return_to: Option<CallReturn>,
    active_root_frames: &[Frame],
    active_frames: usize,
    active_frame_values: u64,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    while let Some(continuation) = continuations.pop() {
        let suspended_frames = active_frames.saturating_add(continuations.len());
        let suspended_values =
            active_frame_values.saturating_add(native_continuation_values(&continuations));
        let dispatch = match continuation {
            NativeContinuation::FunctionSource(state) => {
                let Some(compiler) = compiler else {
                    return Err(NativeFailure::Execution(
                        DynamicFunctionCompileFailure::Engine {
                            source: Arc::new(DynamicFunctionServiceUnavailable),
                        }
                        .into(),
                    ));
                };
                advance_function_source_conversion(
                    runtime,
                    state,
                    Some(value),
                    return_to,
                    suspended_frames,
                    suspended_values,
                    compiler,
                    execution_budget,
                )?
            }
            NativeContinuation::FunctionApply(state) => {
                advance_function_apply(runtime, state, Some(value), return_to, execution_budget)?
            }
            NativeContinuation::FunctionBind(state) => {
                advance_function_bind(runtime, state, Some(value), return_to, execution_budget)?
            }
            NativeContinuation::PropertyKey(state) => advance_property_key_conversion(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )?,
            NativeContinuation::OperatorPrimitive(state) => advance_operator_primitive_conversion(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )?,
            NativeContinuation::IntrinsicGet(IntrinsicGetContinuation::ArrayConstructor {
                realm,
                new_target,
                arguments,
                origin,
            }) => finish_array_constructor_after_prototype_get(
                runtime,
                realm,
                new_target,
                arguments,
                origin,
                &value,
                execution_budget,
            )?,
            NativeContinuation::IntrinsicGet(state) => {
                finish_intrinsic_get(runtime, state, value, active_root_frames, &continuations)?
            }
            NativeContinuation::AggregateError(state) => advance_aggregate_error_collection(
                runtime,
                state,
                value,
                return_to,
                execution_budget,
            )?,
            NativeContinuation::FromEntries(state) => {
                advance_from_entries(runtime, *state, value, return_to, execution_budget)?
            }
            NativeContinuation::GroupBy(state) => {
                advance_group_by(runtime, *state, value, return_to, execution_budget)?
            }
            NativeContinuation::JsonParse(state) => {
                advance_json_parse(runtime, *state, value, return_to, execution_budget)?
            }
            NativeContinuation::ErrorConstructor(state) => {
                advance_error_constructor(runtime, state, value, return_to, execution_budget)?
            }
            NativeContinuation::ErrorToString(state) => {
                advance_error_to_string(runtime, state, value, return_to, execution_budget)?
            }
            NativeContinuation::ArrayIteratorNext(state) => {
                advance_array_iterator_next(runtime, state, value, return_to, execution_budget)?
            }
            NativeContinuation::ForOfStart(state) => {
                advance_for_of_start(runtime, state, value, return_to, execution_budget)?
            }
            NativeContinuation::ForOfNext(state) => {
                advance_for_of_next(runtime, state, value, return_to, execution_budget)?
            }
            NativeContinuation::ForOfClose(state) => {
                advance_for_of_close(state, &value, return_to)?
            }
            NativeContinuation::IteratorAppend(state) => {
                advance_iterator_append(runtime, state, value, return_to, execution_budget)?
            }
            NativeContinuation::IteratorClose(state) => {
                advance_iterator_close(state, value, return_to)?
            }
            NativeContinuation::CopyDataProperties(state) => {
                advance_copy_data_properties(runtime, state, &value, return_to, execution_budget)?
            }
            NativeContinuation::EnumerableOwnProperties(state) => {
                advance_enumerable_own_properties(
                    runtime,
                    *state,
                    Some(value.duplicate()),
                    return_to,
                    execution_budget,
                )?
            }
            NativeContinuation::ObjectAssign(state) => advance_object_assign(
                runtime,
                *state,
                Some(value.duplicate()),
                return_to,
                execution_budget,
            )?,
            NativeContinuation::DefineProperty(state) => advance_define_property(
                runtime,
                *state,
                Some(value.duplicate()),
                return_to,
                execution_budget,
            )?,
            NativeContinuation::DefineProperties(state) => advance_define_properties(
                runtime,
                *state,
                Some(value.duplicate()),
                return_to,
                execution_budget,
            )?,
            NativeContinuation::ArrayJoin(state) => advance_array_join(
                runtime,
                *state,
                Some(value.duplicate()),
                return_to,
                execution_budget,
            )?,
            NativeContinuation::ArraySearch(state) => advance_array_search(
                runtime,
                *state,
                Some(value.duplicate()),
                return_to,
                execution_budget,
            )?,
            NativeContinuation::ArrayMutator(state) => advance_array_mutator(
                runtime,
                *state,
                Some(value.duplicate()),
                return_to,
                execution_budget,
            )?,
            NativeContinuation::ArrayCopier(state) => advance_array_copier(
                runtime,
                *state,
                Some(value.duplicate()),
                return_to,
                execution_budget,
            )?,
            NativeContinuation::ArrayCallback(state) => advance_array_callback(
                runtime,
                *state,
                Some(value.duplicate()),
                return_to,
                execution_budget,
            )?,
            NativeContinuation::ArrayReduction(state) => advance_array_reduction(
                runtime,
                *state,
                Some(value.duplicate()),
                return_to,
                execution_budget,
            )?,
            NativeContinuation::ArraySplice(state) => advance_array_splice(
                runtime,
                *state,
                Some(value.duplicate()),
                return_to,
                execution_budget,
            )?,
            NativeContinuation::InstanceOf(state) => {
                advance_instance_of(runtime, state, &value, return_to, execution_budget)?
            }
            NativeContinuation::ReflectSet => NativeDispatch::Immediate(StoredValue::Boolean(true)),
            NativeContinuation::FunctionCall => NativeDispatch::Immediate(value),
        };
        match dispatch {
            NativeDispatch::Immediate(next) => value = next,
            dispatch @ (NativeDispatch::Pair(_, _)
            | NativeDispatch::ForOfRecord { .. }
            | NativeDispatch::ForOfStep { .. }
            | NativeDispatch::ForOfClosed
            | NativeDispatch::CopyDataPropertiesDone) => {
                if continuations.is_empty() {
                    return Ok(dispatch);
                }
                return Err(EngineFault::RuntimeInvariant {
                    message: "structured native result escaped into an outer continuation",
                }
                .into());
            }
            NativeDispatch::Frame(mut frame) => {
                attach_native_continuations(&mut frame, continuations)?;
                return Ok(NativeDispatch::Frame(frame));
            }
            NativeDispatch::Call(mut call) => {
                prepend_native_continuations(&mut call, continuations)?;
                return Ok(NativeDispatch::Call(call));
            }
        }
    }
    Ok(NativeDispatch::Immediate(value))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the iterative native-to-bytecode transition carries explicit frame and dynamic-compilation budgets"
)]
pub(super) fn resolve_native_dispatch(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    active_root_frames: &[Frame],
    active_frames: usize,
    active_frame_values: u64,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut saw_temporary_receiver = false;
    let result = resolve_native_dispatch_inner(
        runtime,
        dispatch,
        active_root_frames,
        active_frames,
        active_frame_values,
        compiler,
        execution_budget,
        &mut saw_temporary_receiver,
    );
    match result {
        Err(NativeFailure::Execution(error)) => {
            if saw_temporary_receiver && runtime.collection_pending {
                let _ = collect_cycles_with_execution_roots(runtime, active_root_frames, &[], &[]);
            }
            Err(NativeFailure::Execution(error))
        }
        Err(NativeFailure::Abrupt(pending)) if saw_temporary_receiver => {
            Err(NativeFailure::AbruptAfterTransient(pending))
        }
        result => result,
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the iterative native-to-bytecode transition carries explicit frame and dynamic-compilation budgets"
)]
fn resolve_native_dispatch_inner(
    runtime: &mut Runtime,
    mut dispatch: NativeDispatch,
    active_root_frames: &[Frame],
    active_frames: usize,
    active_frame_values: u64,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
    saw_temporary_receiver: &mut bool,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        let NativeDispatch::Call(mut call) = dispatch else {
            return Ok(dispatch);
        };
        *saw_temporary_receiver |=
            native_continuations_have_temporary_receiver(&call.continuations);
        let suspended_frames = active_frames.saturating_add(call.continuations.len());
        // Call inputs are a synchronous transfer buffer, not values reserved
        // by an active frame. Persistent native state lives in continuations
        // and is charged here; a bytecode callee is charged by `plan_frame`.
        let suspended_values =
            active_frame_values.saturating_add(native_continuation_values(&call.continuations));
        check_execution_limit(
            RuntimeResource::Frames,
            u64::from(runtime.limits.max_active_frames),
            usize_to_u64(suspended_frames),
        )?;
        check_execution_limit(
            RuntimeResource::FrameValues,
            runtime.limits.max_active_frame_values,
            suspended_values,
        )?;
        let node = runtime
            .functions
            .get(call.function)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "function",
                index: call.function.index(),
                generation: call.function.generation(),
            })?;
        let native = node.native().copied();
        if native.is_none()
            && let Some(bound) = node.bound()
        {
            let target = bound.target;
            let mut arguments = Vec::new();
            arguments
                .try_reserve_exact(
                    bound
                        .bound_arguments
                        .len()
                        .saturating_add(call.arguments.values.len()),
                )
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: bound
                        .bound_arguments
                        .len()
                        .saturating_add(call.arguments.values.len()),
                })?;
            for argument in &bound.bound_arguments {
                arguments.push(argument.duplicate());
            }
            arguments.extend(call.arguments.into_remaining_values());
            let new_target = match call.new_target {
                Some(current) if current == call.function => Some(target),
                other => other,
            };
            let receiver = if new_target.is_some() {
                call.receiver
            } else {
                bound.bound_this.duplicate()
            };
            call.function = target;
            call.receiver = receiver;
            call.arguments = CallArguments::from_values(arguments);
            call.new_target = new_target;
            dispatch = NativeDispatch::Call(call);
            continue;
        }
        if let Some(native) = native {
            apply_native_pre_call(runtime, call.pre_call.as_ref())?;
            let outcome = dispatch_native_call_with_frames(
                runtime,
                call.function,
                native,
                CallInputs {
                    receiver: call.receiver,
                    arguments: call.arguments,
                    new_target: call.new_target,
                },
                call.return_to,
                Some(call.origin),
                active_root_frames,
                suspended_frames,
                suspended_values,
                compiler,
                execution_budget,
            );
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(
                    NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending),
                ) => {
                    dispatch = resume_iterator_abrupt_continuations(
                        runtime,
                        call.continuations,
                        pending,
                        call.return_to,
                        execution_budget,
                    )?;
                    continue;
                }
                Err(NativeFailure::Execution(error)) => {
                    return Err(NativeFailure::Execution(error));
                }
            };
            dispatch = match outcome {
                NativeDispatch::Immediate(value) => resume_native_continuations(
                    runtime,
                    call.continuations,
                    value,
                    call.return_to,
                    active_root_frames,
                    active_frames,
                    active_frame_values,
                    compiler,
                    execution_budget,
                )?,
                NativeDispatch::Pair(_, _)
                | NativeDispatch::ForOfRecord { .. }
                | NativeDispatch::ForOfStep { .. }
                | NativeDispatch::ForOfClosed
                | NativeDispatch::CopyDataPropertiesDone => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "native function produced a structured continuation result",
                    }
                    .into());
                }
                NativeDispatch::Frame(mut frame) => {
                    attach_native_continuations(&mut frame, call.continuations)?;
                    NativeDispatch::Frame(frame)
                }
                NativeDispatch::Call(mut inner) => {
                    prepend_native_continuations(&mut inner, call.continuations)?;
                    NativeDispatch::Call(inner)
                }
            };
            continue;
        }

        let plan = plan_frame(runtime, call.function, suspended_frames, suspended_values)
            .map_err(NativeFailure::Execution)?;
        let construction = call.new_target;
        let mut frame = create_frame(
            runtime,
            plan,
            if construction.is_some() {
                StoredValue::Undefined
            } else {
                call.receiver
            },
            FrameArguments::Owned(call.arguments),
            call.return_to,
            None,
        )
        .map_err(NativeFailure::Execution)?;
        if let Some(new_target) = construction {
            frame.receiver =
                StoredValue::Object(create_ordinary_constructor_receiver(runtime, new_target)?);
            frame.ordinary_constructor = true;
        }
        frame.native_caller = call.native_caller;
        attach_native_continuations(&mut frame, call.continuations)?;
        apply_native_pre_call(runtime, call.pre_call.as_ref())?;
        return Ok(NativeDispatch::Frame(frame));
    }
}

fn apply_native_pre_call(
    runtime: &mut Runtime,
    pre_call: Option<&NativePreCall>,
) -> Result<(), NativeFailure> {
    match pre_call {
        Some(NativePreCall::AdvanceArrayIterator(iterator)) => {
            runtime.advance_array_iterator(*iterator)?;
        }
        None => {}
    }
    Ok(())
}

#[derive(Debug)]
struct DynamicFunctionServiceUnavailable;

impl fmt::Display for DynamicFunctionServiceUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("no ordinary dynamic-Function compiler was supplied for this execution")
    }
}

impl Error for DynamicFunctionServiceUnavailable {}

pub(super) fn execute_native_entry(
    runtime: &mut Runtime,
    function: FunctionId,
    native: NativeFunction,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
    limits: ExecutionLimits,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
) -> Result<StoredValue, ExecutionError> {
    let mut execution_budget = ExecutionBudget::new(limits);
    let mut prepared_frames = Vec::new();
    prepared_frames
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    let inputs = CallInputs {
        receiver,
        arguments: CallArguments::from_values(arguments),
        new_target: None,
    };
    let dispatch = dispatch_native_call_with_frames(
        runtime,
        function,
        native,
        inputs,
        None,
        None,
        &prepared_frames,
        0,
        0,
        compiler,
        &mut execution_budget,
    );
    let dispatch = match dispatch {
        Ok(dispatch) => resolve_native_dispatch(
            runtime,
            dispatch,
            &prepared_frames,
            0,
            0,
            compiler,
            &mut execution_budget,
        ),
        Err(error) => Err(error),
    };
    match dispatch {
        Ok(NativeDispatch::Immediate(value)) => Ok(value),
        Ok(
            NativeDispatch::Pair(_, _)
            | NativeDispatch::ForOfRecord { .. }
            | NativeDispatch::ForOfStep { .. }
            | NativeDispatch::ForOfClosed
            | NativeDispatch::CopyDataPropertiesDone,
        ) => Err(EngineFault::RuntimeInvariant {
            message: "host native entry returned a structured continuation result",
        }
        .into()),
        Ok(NativeDispatch::Frame(frame)) => {
            prepared_frames.push(frame);
            execute_prepared_frames_with_budget(
                runtime,
                prepared_frames,
                compiler,
                None,
                &mut execution_budget,
            )
        }
        Ok(NativeDispatch::Call(_)) => Err(EngineFault::RuntimeInvariant {
            message: "native dispatch resolver returned an unresolved call",
        }
        .into()),
        Err(NativeFailure::Execution(error)) => Err(error),
        Err(NativeFailure::Abrupt(pending)) => {
            let exception = finish_exception(runtime, pending, Vec::new())?;
            Err(ExecutionError::Exception(exception))
        }
        Err(NativeFailure::AbruptAfterTransient(pending)) => {
            let exception = match finish_exception(runtime, pending, Vec::new()) {
                Ok(exception) => exception,
                Err(error) => {
                    if runtime.collection_pending {
                        let _ = runtime.collect_cycles();
                    }
                    return Err(error);
                }
            };
            if runtime.collection_pending {
                let _ = runtime.collect_cycles();
            }
            Err(ExecutionError::Exception(exception))
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "native invocation, compilation, installation, and rollback remain one explicit audited boundary"
)]
#[cfg(test)]
pub(super) fn dispatch_native_call(
    runtime: &mut Runtime,
    function: FunctionId,
    native: NativeFunction,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
    active_frames: usize,
    active_frame_values: u64,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    dispatch_native_call_with_frames(
        runtime,
        function,
        native,
        inputs,
        return_to,
        origin,
        &[],
        active_frames,
        active_frame_values,
        compiler,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "native invocation, compilation, installation, and rollback remain one explicit audited boundary"
)]
pub(super) fn dispatch_native_call_with_frames(
    runtime: &mut Runtime,
    function: FunctionId,
    native: NativeFunction,
    mut inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
    active_root_frames: &[Frame],
    active_frames: usize,
    active_frame_values: u64,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if inputs.new_target.is_some() && !native.kind.is_constructor() {
        let Some(origin) = origin else {
            return Err(NativeFailure::Execution(
                EngineFault::RuntimeInvariant {
                    message: "host construction of a nonconstructor native function is not implemented",
                }
                .into(),
            ));
        };
        return Err(NativeFailure::Abrupt(PendingException {
            realm: native.realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: function_not_constructor_message(runtime, function)?,
            },
            origin,
        }));
    }
    match native.kind {
        NativeFunctionKind::FunctionPrototype => {
            Ok(NativeDispatch::Immediate(StoredValue::Undefined))
        }
        NativeFunctionKind::ThrowTypeError => Err(NativeFailure::Abrupt(PendingException {
            realm: native.realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("invalid property access")?,
            },
            origin: origin.unwrap_or_else(native_function_host_origin),
        })),
        NativeFunctionKind::FunctionPrototypeApply => begin_function_apply(
            runtime,
            native.realm,
            inputs,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            active_frames,
            active_frame_values,
            execution_budget,
            None,
            Some(SyntheticNativeFrame::Apply),
        ),
        NativeFunctionKind::FunctionPrototypeCall => {
            let origin = origin.unwrap_or_else(native_function_host_origin);
            let StoredValue::Function(function) = inputs.receiver else {
                return Err(NativeFailure::Abrupt(PendingException {
                    realm: native.realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::TypeError,
                        message: JsString::from_utf8("not a function")?,
                    },
                    origin,
                }));
            };
            let mut arguments = inputs.arguments;
            let receiver = arguments.take_first_or_undefined();
            let mut continuations = Vec::new();
            continuations
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 1,
                })?;
            continuations.push(NativeContinuation::FunctionCall);
            Ok(NativeDispatch::Call(NativeCall {
                function,
                receiver,
                arguments,
                return_to,
                origin,
                continuations,
                pre_call: None,
                new_target: None,
                native_caller: Some(SyntheticNativeFrame::Call),
            }))
        }
        NativeFunctionKind::FunctionPrototypeBind => begin_function_bind(
            runtime,
            native.realm,
            inputs,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            execution_budget,
        ),
        NativeFunctionKind::FunctionPrototypeHasInstance => {
            let mut arguments = inputs.arguments;
            let value = arguments.take_first_or_undefined();
            begin_function_has_instance(
                runtime,
                native.realm,
                value,
                inputs.receiver,
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        NativeFunctionKind::OrdinaryFunctionConstructor => {
            let Some(compiler) = compiler else {
                return Err(NativeFailure::Execution(
                    DynamicFunctionCompileFailure::Engine {
                        source: Arc::new(DynamicFunctionServiceUnavailable),
                    }
                    .into(),
                ));
            };
            let origin = origin.unwrap_or_else(native_function_host_origin);
            begin_function_source_conversion(
                runtime,
                native,
                inputs.arguments.into_remaining_values(),
                inputs.new_target,
                return_to,
                origin,
                active_frames,
                active_frame_values,
                compiler,
                execution_budget,
            )
        }
        NativeFunctionKind::ErrorConstructor(kind) => begin_error_constructor(
            runtime,
            function,
            kind,
            inputs.arguments,
            inputs.new_target,
            native.realm,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            active_root_frames,
            execution_budget,
        ),
        NativeFunctionKind::ErrorPrototypeToString => begin_error_to_string(
            runtime,
            native.realm,
            inputs.receiver,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            execution_budget,
        ),
        NativeFunctionKind::ErrorIsError => {
            let value = inputs.arguments.take_first_or_undefined();
            let is_error = match value {
                StoredValue::Object(object) => runtime.is_error_object(object)?,
                StoredValue::Undefined
                | StoredValue::Null
                | StoredValue::Boolean(_)
                | StoredValue::Number(_)
                | StoredValue::BigInt(_)
                | StoredValue::String(_)
                | StoredValue::Symbol(_)
                | StoredValue::Function(_) => false,
            };
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(is_error)))
        }
        NativeFunctionKind::ObjectConstructor => {
            let mut arguments = inputs.arguments;
            object_constructor(runtime, native.realm, arguments.take_first())
        }
        NativeFunctionKind::ObjectGetPrototypeOf => {
            let mut arguments = inputs.arguments;
            get_prototype_of(
                runtime,
                native.realm,
                arguments.take_first(),
                origin.as_ref(),
            )
        }
        NativeFunctionKind::ObjectCreate => object_create(
            runtime,
            native.realm,
            inputs.arguments,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            execution_budget,
        ),
        NativeFunctionKind::ObjectSetPrototypeOf => {
            set_prototype_of(runtime, native.realm, inputs.arguments, origin.as_ref())
        }
        NativeFunctionKind::ObjectPreventExtensions => {
            let mut arguments = inputs.arguments;
            prevent_extensions(
                runtime,
                native.realm,
                arguments.take_first(),
                origin.as_ref(),
            )
        }
        NativeFunctionKind::ObjectIsExtensible => {
            let mut arguments = inputs.arguments;
            is_extensible(
                runtime,
                native.realm,
                arguments.take_first(),
                origin.as_ref(),
            )
        }
        NativeFunctionKind::ObjectSeal | NativeFunctionKind::ObjectFreeze => {
            let level = if native.kind == NativeFunctionKind::ObjectSeal {
                IntegrityLevel::Sealed
            } else {
                IntegrityLevel::Frozen
            };
            let mut arguments = inputs.arguments;
            set_integrity_level(
                runtime,
                native.realm,
                arguments.take_first(),
                level,
                origin.as_ref(),
            )
        }
        NativeFunctionKind::ObjectIsSealed | NativeFunctionKind::ObjectIsFrozen => {
            let level = if native.kind == NativeFunctionKind::ObjectIsSealed {
                IntegrityLevel::Sealed
            } else {
                IntegrityLevel::Frozen
            };
            let mut arguments = inputs.arguments;
            test_integrity_level(
                runtime,
                native.realm,
                arguments.take_first(),
                level,
                origin.as_ref(),
            )
        }
        NativeFunctionKind::ObjectKeys
        | NativeFunctionKind::ObjectGetOwnPropertyNames
        | NativeFunctionKind::ObjectGetOwnPropertySymbols => {
            let listing = match native.kind {
                NativeFunctionKind::ObjectKeys => KeyListing::EnumerableOnly,
                NativeFunctionKind::ObjectGetOwnPropertyNames => KeyListing::AllStringKeys,
                NativeFunctionKind::ObjectGetOwnPropertySymbols => KeyListing::AllSymbolKeys,
                _ => unreachable!("the dispatch arm admits only Object own-key listings"),
            };
            let mut arguments = inputs.arguments;
            own_property_keys(
                runtime,
                native.realm,
                arguments.take_first(),
                listing,
                origin.as_ref(),
                execution_budget,
            )
        }
        NativeFunctionKind::ObjectValues | NativeFunctionKind::ObjectEntries => {
            let kind = if native.kind == NativeFunctionKind::ObjectValues {
                EnumerableOwnPropertiesKind::Value
            } else {
                EnumerableOwnPropertiesKind::KeyAndValue
            };
            let mut arguments = inputs.arguments;
            begin_enumerable_own_properties(
                runtime,
                native.realm,
                arguments.take_first(),
                kind,
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        NativeFunctionKind::ObjectGetOwnPropertyDescriptors => {
            let mut arguments = inputs.arguments;
            get_own_property_descriptors(
                runtime,
                native.realm,
                arguments.take_first(),
                &origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        NativeFunctionKind::ObjectIs => {
            let mut arguments = inputs.arguments;
            let first = arguments.take_first_or_undefined();
            let second = arguments.take_first_or_undefined();
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(
                first.same_value(&second),
            )))
        }
        NativeFunctionKind::ObjectAssign => {
            let mut arguments = inputs.arguments;
            let target = arguments.take_first_or_undefined();
            begin_object_assign(
                runtime,
                native.realm,
                target,
                arguments.into_remaining_values(),
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        NativeFunctionKind::ObjectFromEntries => {
            let mut arguments = inputs.arguments;
            begin_from_entries(
                runtime,
                arguments.take_first_or_undefined(),
                native.realm,
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        NativeFunctionKind::ObjectGroupBy => {
            let mut arguments = inputs.arguments;
            let items = arguments.take_first_or_undefined();
            let callback = arguments.take_first_or_undefined();
            begin_group_by(
                runtime,
                items,
                &callback,
                native.realm,
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        NativeFunctionKind::ObjectHasOwn => {
            let mut arguments = inputs.arguments;
            let target = arguments.take_first_or_undefined();
            let key = arguments.take_first_or_undefined();
            let origin = origin.unwrap_or_else(native_function_host_origin);
            // `Object.hasOwn` performs `ToObject(O)` before
            // `ToPropertyKey(P)`, so a nullish target throws without running
            // observable key coercion. Non-nullish primitives are resolved by
            // the shared boxed-own-property path after conversion.
            if matches!(target, StoredValue::Undefined | StoredValue::Null) {
                return Err(NativeFailure::Abrupt(PendingException {
                    realm: native.realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::TypeError,
                        message: JsString::from_utf8("cannot convert to object")?,
                    },
                    origin,
                }));
            }
            begin_property_key_conversion(
                runtime,
                key,
                PropertyKeyTarget::HasOwnProperty {
                    target,
                    realm: native.realm,
                },
                native.realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        NativeFunctionKind::ObjectDefineProperty => {
            let mut arguments = inputs.arguments;
            let target = arguments.take_first_or_undefined();
            let key = arguments.take_first_or_undefined();
            let descriptor = arguments.take_first_or_undefined();
            let origin = origin.unwrap_or_else(native_function_host_origin);
            // The key is converted first, because `ToPropertyKey` can run a
            // user `toString` before any descriptor field is read.
            begin_property_key_conversion(
                runtime,
                key,
                PropertyKeyTarget::DefineProperty {
                    target,
                    descriptor,
                    realm: native.realm,
                },
                native.realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        NativeFunctionKind::ObjectDefineProperties => {
            let mut arguments = inputs.arguments;
            let target = arguments.take_first_or_undefined();
            let properties = arguments.take_first_or_undefined();
            begin_define_properties(
                runtime,
                native.realm,
                target,
                properties,
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        NativeFunctionKind::ObjectGetOwnPropertyDescriptor => {
            let mut arguments = inputs.arguments;
            let target = arguments.take_first_or_undefined();
            let key = arguments.take_first_or_undefined();
            let origin = origin.unwrap_or_else(native_function_host_origin);
            begin_property_key_conversion(
                runtime,
                key,
                PropertyKeyTarget::OwnPropertyDescriptor {
                    target,
                    realm: native.realm,
                },
                native.realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        NativeFunctionKind::ObjectPrototypeHasOwnProperty
        | NativeFunctionKind::ObjectPrototypePropertyIsEnumerable => {
            let mut arguments = inputs.arguments;
            let key = arguments.take_first_or_undefined();
            let target = inputs.receiver;
            let realm = native.realm;
            let enumerable = matches!(
                native.kind,
                NativeFunctionKind::ObjectPrototypePropertyIsEnumerable
            );
            // The key is converted first, so its `toString` runs before the
            // receiver is inspected.
            begin_property_key_conversion(
                runtime,
                key,
                if enumerable {
                    PropertyKeyTarget::PropertyIsEnumerable { target, realm }
                } else {
                    PropertyKeyTarget::HasOwnProperty { target, realm }
                },
                realm,
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        // `isPrototypeOf` walks the argument's prototype chain, so it needs no
        // key conversion at all. A primitive argument has no chain to walk and a
        // receiver never precedes itself, which is why `p.isPrototypeOf(p)` is
        // `false`.
        NativeFunctionKind::ObjectPrototypeIsPrototypeOf => {
            let mut arguments = inputs.arguments;
            let candidate = arguments.take_first_or_undefined();
            let origin = origin.unwrap_or_else(native_function_host_origin);
            object_prototype_is_prototype_of(
                runtime,
                native.realm,
                &inputs.receiver,
                &candidate,
                &origin,
                execution_budget,
            )
        }
        NativeFunctionKind::Reflect(method) => begin_reflect_method(
            runtime,
            native.realm,
            method,
            inputs.arguments,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            active_frames,
            active_frame_values,
            execution_budget,
        ),
        NativeFunctionKind::JsonParse => begin_json_parse(
            runtime,
            inputs.arguments.take_first_or_undefined(),
            inputs.arguments.take_first_or_undefined(),
            native.realm,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            execution_budget,
        ),
        NativeFunctionKind::JsonIsRawJson => {
            let value = inputs.arguments.take_first_or_undefined();
            json_is_raw_json(runtime, &value)
        }
        NativeFunctionKind::JsonRawJson => begin_json_raw_json(
            runtime,
            inputs.arguments.take_first_or_undefined(),
            native.realm,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            execution_budget,
        ),
        NativeFunctionKind::ObjectPrototypeToString => begin_object_prototype_to_string(
            runtime,
            native.realm,
            inputs.receiver,
            return_to,
            origin,
        ),
        NativeFunctionKind::ObjectPrototypeValueOf => match inputs.receiver {
            value @ (StoredValue::Function(_) | StoredValue::Object(_)) => {
                Ok(NativeDispatch::Immediate(value))
            }
            StoredValue::Boolean(value) => {
                let object = runtime.allocate_boxed_boolean(native.realm, value)?;
                Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
            }
            StoredValue::Number(value) => {
                let object = runtime.allocate_boxed_number(native.realm, value)?;
                Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
            }
            StoredValue::BigInt(value) => {
                let object = runtime.allocate_boxed_bigint(native.realm, value)?;
                Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
            }
            StoredValue::String(value) => {
                let object = runtime.allocate_boxed_string(native.realm, value)?;
                Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
            }
            StoredValue::Undefined | StoredValue::Null => {
                let Some(origin) = origin else {
                    return Err(NativeFailure::Execution(
                        EngineFault::RuntimeInvariant {
                            message: "host Object.prototype.valueOf error has no source origin",
                        }
                        .into(),
                    ));
                };
                Err(NativeFailure::Abrupt(PendingException {
                    realm: native.realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::TypeError,
                        message: JsString::from_utf8("cannot convert to object")?,
                    },
                    origin,
                }))
            }
            StoredValue::Symbol(value) => {
                let object = runtime.allocate_boxed_symbol(native.realm, value)?;
                Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
            }
        },
        NativeFunctionKind::BooleanConstructor => {
            let mut arguments = inputs.arguments;
            let value = arguments.take_first_or_undefined().is_truthy();
            let Some(new_target) = inputs.new_target else {
                return Ok(NativeDispatch::Immediate(StoredValue::Boolean(value)));
            };
            begin_boolean_constructor_wrapper(runtime, new_target, value, return_to, origin)
        }
        NativeFunctionKind::BooleanPrototypeToString => {
            let value =
                boolean_receiver_value(runtime, native.realm, &inputs.receiver, origin.as_ref())?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(if value { "true" } else { "false" })?,
            )))
        }
        NativeFunctionKind::BooleanPrototypeValueOf => {
            let value =
                boolean_receiver_value(runtime, native.realm, &inputs.receiver, origin.as_ref())?;
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(value)))
        }
        NativeFunctionKind::NumberConstructor => {
            let mut arguments = inputs.arguments;
            let Some(argument) = arguments.take_first() else {
                let value = JsNumber::from_i32(0);
                return inputs.new_target.map_or_else(
                    || Ok(NativeDispatch::Immediate(StoredValue::Number(value))),
                    |new_target| {
                        begin_number_constructor_wrapper(
                            runtime, new_target, value, return_to, origin,
                        )
                    },
                );
            };
            begin_operator_primitive_conversion(
                runtime,
                argument,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::NumberIntrinsic {
                    new_target: inputs.new_target,
                },
                native.realm,
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        // The three decimal renderings share one entry point: each converts its
        // digit-count argument and then renders the receiver's exact value.
        NativeFunctionKind::NumberPrototypeFormat(format) => {
            let number =
                number_receiver_value(runtime, native.realm, &inputs.receiver, origin.as_ref())?;
            let mut arguments = inputs.arguments;
            let origin = origin.unwrap_or_else(native_function_host_origin);
            match arguments.take_first() {
                // An absent or `undefined` count needs no conversion; each
                // method's default follows from `ToIntegerOrInfinity(undefined)`
                // being `0`, except `toPrecision`, which then renders the value
                // the way `ToString` would.
                None | Some(StoredValue::Undefined) => {
                    finish_number_format_default(number, format, native.realm, &origin)
                }
                Some(digits) => begin_operator_primitive_conversion(
                    runtime,
                    digits,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::NumberFormatDigits { number, format },
                    native.realm,
                    return_to,
                    origin,
                    execution_budget,
                ),
            }
        }
        NativeFunctionKind::NumberPrototypeToString => {
            let number =
                number_receiver_value(runtime, native.realm, &inputs.receiver, origin.as_ref())?;
            let mut arguments = inputs.arguments;
            match arguments.take_first() {
                None | Some(StoredValue::Undefined) => Ok(NativeDispatch::Immediate(
                    StoredValue::String(number.to_radix_string(10)?),
                )),
                Some(radix) => begin_operator_primitive_conversion(
                    runtime,
                    radix,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::NumberToString { number },
                    native.realm,
                    return_to,
                    origin.unwrap_or_else(native_function_host_origin),
                    execution_budget,
                ),
            }
        }
        NativeFunctionKind::NumberPrototypeValueOf => {
            let value =
                number_receiver_value(runtime, native.realm, &inputs.receiver, origin.as_ref())?;
            Ok(NativeDispatch::Immediate(StoredValue::Number(value)))
        }
        NativeFunctionKind::BigIntConstructor => {
            let mut arguments = inputs.arguments;
            bigint_constructor(
                native.realm,
                arguments.take_first(),
                inputs.new_target,
                &origin.unwrap_or_else(native_function_host_origin),
            )
        }
        NativeFunctionKind::BigIntPrototypeToString => {
            let origin = origin.unwrap_or_else(native_function_host_origin);
            let value = this_bigint_value(runtime, native.realm, &inputs.receiver, &origin)?;
            let mut arguments = inputs.arguments;
            match arguments.take_first() {
                None | Some(StoredValue::Undefined) => {
                    bigint_prototype_to_string(&value, 10, native.realm, &origin)
                }
                // A supplied radix is converted with `ToNumber`, which can run a
                // user `valueOf`, so the conversion is resumable.
                Some(radix) => begin_operator_primitive_conversion(
                    runtime,
                    radix,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::BigIntToString { value },
                    native.realm,
                    return_to,
                    origin,
                    execution_budget,
                ),
            }
        }
        NativeFunctionKind::BigIntPrototypeValueOf => {
            let origin = origin.unwrap_or_else(native_function_host_origin);
            let value = this_bigint_value(runtime, native.realm, &inputs.receiver, &origin)?;
            Ok(NativeDispatch::Immediate(StoredValue::BigInt(value)))
        }
        NativeFunctionKind::BigIntAsIntN | NativeFunctionKind::BigIntAsUintN => {
            let truncation = if native.kind == NativeFunctionKind::BigIntAsIntN {
                BigIntTruncation::Signed
            } else {
                BigIntTruncation::Unsigned
            };
            let origin = origin.unwrap_or_else(native_function_host_origin);
            let mut arguments = inputs.arguments;
            let bits = arguments.take_first_or_undefined();
            let value = arguments.take_first_or_undefined();
            // `bits` goes through `ToIndex`, so it must reach the numeric domain
            // first; the value goes through `ToBigInt`.
            begin_operator_primitive_conversion(
                runtime,
                bits,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::BigIntTruncationBits { value, truncation },
                native.realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        NativeFunctionKind::StringConstructor => {
            let mut arguments = inputs.arguments;
            let Some(argument) = arguments.take_first() else {
                let value = JsString::empty();
                return if let Some(new_target) = inputs.new_target {
                    begin_string_constructor_wrapper(runtime, new_target, value, return_to, origin)
                } else {
                    Ok(NativeDispatch::Immediate(StoredValue::String(value)))
                };
            };
            if inputs.new_target.is_none()
                && let StoredValue::Symbol(symbol) = &argument
            {
                return Ok(NativeDispatch::Immediate(StoredValue::String(
                    symbol_descriptive_string(symbol)?,
                )));
            }
            begin_operator_primitive_conversion(
                runtime,
                argument,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::StringIntrinsic {
                    new_target: inputs.new_target,
                },
                native.realm,
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        // A `Number` predicate never converts its argument: it answers `false`
        // for anything that is not already a Number, which is what separates
        // `Number.isNaN` from the global `isNaN`.
        NativeFunctionKind::NumberPredicateStatic(predicate) => {
            let mut arguments = inputs.arguments;
            let argument = arguments.take_first_or_undefined();
            let answer = match argument {
                StoredValue::Number(value) => {
                    let value = value.as_f64();
                    match predicate {
                        NumberPredicate::IsNaN => value.is_nan(),
                        NumberPredicate::IsFinite => value.is_finite(),
                        // Integrality is an exact property of a finite value.
                        NumberPredicate::IsInteger => is_integral(value),
                        // A safe integer additionally fits the exact binary64
                        // integer range, so `2**53` is an integer but not safe.
                        NumberPredicate::IsSafeInteger => {
                            is_integral(value) && value.abs() <= max_safe_integer()
                        }
                    }
                }
                _ => false,
            };
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(answer)))
        }
        // The three searches share one resumable element loop; they differ only
        // in their equality and in whether a hole is skipped.
        NativeFunctionKind::ArrayPrototypeSearch(search) => {
            let mut arguments = inputs.arguments;
            let needle = arguments.take_first_or_undefined();
            let position = arguments.take_first();
            begin_array_search(
                runtime,
                search,
                native.realm,
                inputs.receiver,
                needle,
                position,
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        // The six mutators share one resumable driver: each reads `length`
        // once, performs a planned sequence of element steps, and writes
        // `length` back, with every step a possible accessor entry.
        NativeFunctionKind::ArrayPrototypeMutator(mutator) => begin_array_mutator(
            runtime,
            mutator,
            native.realm,
            inputs.receiver,
            inputs.arguments,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            execution_budget,
        ),
        // `slice`, `concat`, and `at` read without mutating; the first two build
        // a fresh Array while `at` answers one element.
        NativeFunctionKind::ArrayPrototypeCopier(copier) => begin_array_copier(
            runtime,
            copier,
            native.realm,
            inputs.receiver,
            inputs.arguments,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            execution_budget,
        ),
        // The nine callback methods share one resumable loop. Suspension is
        // intrinsic here rather than incidental: the callback is a user call on
        // every iteration.
        NativeFunctionKind::ArrayPrototypeCallback(method) => begin_array_callback(
            runtime,
            method,
            native.realm,
            inputs.receiver,
            inputs.arguments,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            execution_budget,
        ),
        // The two reductions share a fold whose accumulator threads through the
        // callback's result.
        NativeFunctionKind::ArrayPrototypeReduction(reduction) => begin_array_reduction(
            runtime,
            reduction,
            native.realm,
            inputs.receiver,
            inputs.arguments,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            execution_budget,
        ),
        // `splice` both extracts and mutates, so it collects every removed
        // element before anything shifts.
        NativeFunctionKind::ArrayPrototypeSplice => begin_array_splice(
            runtime,
            native.realm,
            inputs.receiver,
            inputs.arguments,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            execution_budget,
        ),
        NativeFunctionKind::ArrayIsArray => {
            let mut arguments = inputs.arguments;
            let answer = match arguments.take_first_or_undefined() {
                StoredValue::Object(object) => runtime.is_array_object(object)?,
                _ => false,
            };
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(answer)))
        }
        // Every `String.prototype` method shares one resumable coercion machine,
        // because they all convert the receiver with `ToString` and then each
        // declared argument in order, and every one of those steps can re-enter
        // the interpreter.
        NativeFunctionKind::StringPrototypeMethod(method) => begin_string_method(
            runtime,
            method,
            native.realm,
            inputs.receiver,
            inputs.arguments,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            execution_budget,
        ),
        NativeFunctionKind::StringPrototypeToString
        | NativeFunctionKind::StringPrototypeValueOf => {
            let value =
                string_receiver_value(runtime, native.realm, &inputs.receiver, origin.as_ref())?;
            Ok(NativeDispatch::Immediate(StoredValue::String(value)))
        }
        NativeFunctionKind::ArrayConstructor => {
            let arguments = inputs.arguments.into_remaining_values();
            let origin = origin.unwrap_or_else(native_function_host_origin);
            if let Some(new_target) = inputs.new_target {
                begin_array_constructor_prototype_get(
                    runtime,
                    native.realm,
                    new_target,
                    arguments,
                    return_to,
                    origin,
                    execution_budget,
                )
            } else {
                let prototype = HeapReference::Object(runtime.realm_array_prototype(native.realm)?);
                finish_array_constructor(
                    runtime,
                    native.realm,
                    prototype,
                    arguments,
                    origin,
                    execution_budget,
                )
            }
        }
        NativeFunctionKind::SymbolConstructor => {
            let mut arguments = inputs.arguments;
            let Some(argument) = arguments.take_first() else {
                return Ok(NativeDispatch::Immediate(StoredValue::Symbol(
                    runtime.new_unique_symbol(None)?,
                )));
            };
            if matches!(argument, StoredValue::Undefined) {
                return Ok(NativeDispatch::Immediate(StoredValue::Symbol(
                    runtime.new_unique_symbol(None)?,
                )));
            }
            begin_operator_primitive_conversion(
                runtime,
                argument,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::SymbolIntrinsic {
                    global_registry: false,
                },
                native.realm,
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        NativeFunctionKind::SymbolPrototypeToString => {
            let value =
                symbol_receiver_value(runtime, native.realm, &inputs.receiver, origin.as_ref())?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                symbol_descriptive_string(&value)?,
            )))
        }
        NativeFunctionKind::SymbolPrototypeValueOf
        | NativeFunctionKind::SymbolPrototypeToPrimitive => {
            let value =
                symbol_receiver_value(runtime, native.realm, &inputs.receiver, origin.as_ref())?;
            Ok(NativeDispatch::Immediate(StoredValue::Symbol(value)))
        }
        NativeFunctionKind::SymbolPrototypeDescription => {
            let value =
                symbol_receiver_value(runtime, native.realm, &inputs.receiver, origin.as_ref())?;
            Ok(NativeDispatch::Immediate(
                value
                    .description()
                    .map_or(StoredValue::Undefined, |description| {
                        StoredValue::String(description.clone())
                    }),
            ))
        }
        NativeFunctionKind::SymbolFor => {
            let argument = inputs.arguments.take_first_or_undefined();
            begin_operator_primitive_conversion(
                runtime,
                argument,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::SymbolIntrinsic {
                    global_registry: true,
                },
                native.realm,
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        NativeFunctionKind::SymbolKeyFor => {
            let argument = inputs.arguments.take_first_or_undefined();
            let StoredValue::Symbol(symbol) = argument else {
                return Err(NativeFailure::Abrupt(PendingException {
                    realm: native.realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::TypeError,
                        message: JsString::from_utf8("not a symbol")?,
                    },
                    origin: origin.unwrap_or_else(native_function_host_origin),
                }));
            };
            Ok(NativeDispatch::Immediate(
                if symbol.kind() == crate::AtomKind::GlobalSymbol {
                    symbol
                        .description()
                        .map_or(StoredValue::Undefined, |description| {
                            StoredValue::String(description.clone())
                        })
                } else {
                    StoredValue::Undefined
                },
            ))
        }
        NativeFunctionKind::IteratorPrototypeIterator => {
            Ok(NativeDispatch::Immediate(inputs.receiver))
        }
        NativeFunctionKind::ArrayPrototypeJoin => {
            let mut arguments = inputs.arguments;
            begin_array_join(
                runtime,
                native.realm,
                inputs.receiver,
                arguments.take_first(),
                return_to,
                origin.unwrap_or_else(native_function_host_origin),
                execution_budget,
            )
        }
        // `Array.prototype.toString` is `join` with no separator: the pinned
        // table dispatches it straight to `js_array_join`
        // (`quickjs.c:44558`).
        NativeFunctionKind::ArrayPrototypeToString => begin_array_join(
            runtime,
            native.realm,
            inputs.receiver,
            None,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            execution_budget,
        ),
        NativeFunctionKind::ArrayPrototypeValues => begin_array_iterator_method(
            runtime,
            inputs.receiver,
            crate::object::ArrayIteratorKind::Value,
            native.realm,
            origin.unwrap_or_else(native_function_host_origin),
        ),
        NativeFunctionKind::ArrayPrototypeKeys => begin_array_iterator_method(
            runtime,
            inputs.receiver,
            crate::object::ArrayIteratorKind::Key,
            native.realm,
            origin.unwrap_or_else(native_function_host_origin),
        ),
        NativeFunctionKind::ArrayPrototypeEntries => begin_array_iterator_method(
            runtime,
            inputs.receiver,
            crate::object::ArrayIteratorKind::KeyAndValue,
            native.realm,
            origin.unwrap_or_else(native_function_host_origin),
        ),
        NativeFunctionKind::ArrayIteratorNext => begin_array_iterator_next(
            runtime,
            inputs.receiver,
            native.realm,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            execution_budget,
        ),
        NativeFunctionKind::StringPrototypeIterator => begin_string_iterator_method(
            runtime,
            inputs.receiver,
            native.realm,
            return_to,
            origin.unwrap_or_else(native_function_host_origin),
            execution_budget,
        ),
        NativeFunctionKind::StringIteratorNext => begin_string_iterator_next(
            runtime,
            inputs.receiver,
            native.realm,
            origin.unwrap_or_else(native_function_host_origin),
        ),
        NativeFunctionKind::FunctionPrototypeToString => {
            let StoredValue::Function(function) = inputs.receiver else {
                let Some(origin) = origin else {
                    return Err(NativeFailure::Execution(
                        EngineFault::RuntimeInvariant {
                            message: "host Function.prototype.toString error has no source origin",
                        }
                        .into(),
                    ));
                };
                return Err(NativeFailure::Abrupt(PendingException {
                    realm: native.realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::TypeError,
                        message: JsString::from_utf8("not a function")?,
                    },
                    origin,
                }));
            };
            Ok(NativeDispatch::Immediate(StoredValue::String(
                function_to_string(runtime, function, native.realm, origin.as_ref())?,
            )))
        }
    }
}

const MAX_FUNCTION_APPLY_ARGUMENTS: u32 = 65_534;

#[allow(
    clippy::too_many_arguments,
    reason = "apply admission keeps callable validation, retained-value preflight, and native work budget explicit"
)]
pub(super) fn begin_function_apply(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    active_frames: usize,
    active_frame_values: u64,
    execution_budget: &mut ExecutionBudget,
    new_target: Option<FunctionId>,
    native_caller: Option<SyntheticNativeFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(target) = inputs.receiver else {
        return Err(function_apply_exception(
            realm,
            ExceptionKind::TypeError,
            "not a function",
            origin,
        )?);
    };
    let mut supplied = inputs.arguments;
    let receiver = supplied.take_first_or_undefined();
    let array_like = supplied.take_first_or_undefined();
    begin_array_like_call(
        runtime,
        realm,
        target,
        receiver,
        array_like,
        return_to,
        origin,
        active_frames,
        active_frame_values,
        execution_budget,
        new_target,
        native_caller,
        true,
    )
}

/// Implements `Reflect.construct` in the exact ECMA-262 order: validate the
/// target, select and validate `newTarget`, then collect the array-like list.
/// The last step reuses the same resumable indexed `Get` machine as
/// `Function.prototype.apply`, but nullish argument lists are rejected rather
/// than treated as empty.
#[allow(
    clippy::too_many_arguments,
    reason = "Reflect.construct keeps target/new-target validation and the shared array-like execution budget explicit"
)]
pub(super) fn begin_reflect_construct(
    runtime: &mut Runtime,
    realm: RealmId,
    mut supplied: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    active_frames: usize,
    active_frame_values: u64,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = match supplied.take_first_or_undefined() {
        StoredValue::Function(function) if function_is_constructor(runtime, function)? => function,
        _ => {
            return Err(function_apply_exception(
                realm,
                ExceptionKind::TypeError,
                "not a constructor",
                origin,
            )?);
        }
    };
    let array_like = supplied.take_first_or_undefined();
    let new_target = match supplied.take_first() {
        None => target,
        Some(StoredValue::Function(function)) if function_is_constructor(runtime, function)? => {
            function
        }
        Some(_) => {
            return Err(function_apply_exception(
                realm,
                ExceptionKind::TypeError,
                "not a constructor",
                origin,
            )?);
        }
    };
    begin_array_like_call(
        runtime,
        realm,
        target,
        StoredValue::Undefined,
        array_like,
        return_to,
        origin,
        active_frames,
        active_frame_values,
        execution_budget,
        Some(new_target),
        None,
        false,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the shared CreateListFromArrayLike machine keeps retained roots, construction identity, and resource accounting explicit"
)]
pub(super) fn begin_array_like_call(
    runtime: &mut Runtime,
    realm: RealmId,
    target: FunctionId,
    receiver: StoredValue,
    array_like: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    active_frames: usize,
    active_frame_values: u64,
    execution_budget: &mut ExecutionBudget,
    new_target: Option<FunctionId>,
    native_caller: Option<SyntheticNativeFrame>,
    nullish_is_empty: bool,
) -> Result<NativeDispatch, NativeFailure> {
    if nullish_is_empty && matches!(array_like, StoredValue::Undefined | StoredValue::Null) {
        return function_apply_target_call(
            target,
            receiver,
            Vec::new(),
            return_to,
            origin,
            None,
            native_caller,
        );
    }
    if !matches!(
        array_like,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        // Preserve QuickJS's historical Function.prototype.apply wording while
        // Reflect uses the specification-facing object-list diagnostic.
        let message = if nullish_is_empty {
            "not a object"
        } else {
            "not an object"
        };
        return Err(function_apply_exception(
            realm,
            ExceptionKind::TypeError,
            message,
            origin,
        )?);
    }

    check_execution_limit(
        RuntimeResource::Frames,
        u64::from(runtime.limits.max_active_frames),
        usize_to_u64(active_frames).saturating_add(1),
    )?;
    // The fourth retained slot covers an object-valued length while its
    // Number-hint ToPrimitive state is suspended.
    check_execution_limit(
        RuntimeResource::FrameValues,
        runtime.limits.max_active_frame_values,
        active_frame_values.saturating_add(4),
    )?;
    let state = FunctionApplyContinuation {
        target,
        receiver,
        array_like,
        realm,
        length: None,
        next_index: 0,
        arguments: Vec::new(),
        stage: FunctionApplyStage::AwaitLength,
        active_frame_values,
        origin,
        new_target,
        native_caller,
    };
    let length_key = runtime.predefined_property_key(PredefinedAtom::Length);
    charge_heap_property_lookup(runtime, &state.array_like, execution_budget)?;
    match read_static_property(runtime, realm, &state.array_like, &length_key)? {
        PropertyReadOutcome::Value(value) => begin_function_apply_length_conversion(
            runtime,
            state,
            value,
            return_to,
            execution_budget,
        ),
        PropertyReadOutcome::Getter { function, receiver } => {
            function_apply_getter_call(state, function, receiver, return_to)
        }
        PropertyReadOutcome::Failed(_) => Err(EngineFault::RuntimeInvariant {
            message: "object-valued apply argument list failed its length read",
        }
        .into()),
    }
}

fn advance_function_apply(
    runtime: &mut Runtime,
    mut state: FunctionApplyContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(value) = completion else {
        return Err(EngineFault::RuntimeInvariant {
            message: "apply continuation resumed without a getter completion",
        }
        .into());
    };
    match state.stage {
        FunctionApplyStage::AwaitLength => begin_function_apply_length_conversion(
            runtime,
            state,
            value,
            return_to,
            execution_budget,
        ),
        FunctionApplyStage::AwaitIndex => {
            let length = state.length.ok_or(EngineFault::RuntimeInvariant {
                message: "apply index continuation has no fixed length",
            })?;
            if state.next_index >= length {
                return Err(EngineFault::RuntimeInvariant {
                    message: "apply index continuation resumed after its fixed length",
                }
                .into());
            }
            state.arguments.push(value);
            state.next_index = state.next_index.saturating_add(1);
            advance_function_apply_indices(runtime, state, return_to, execution_budget)
        }
    }
}

fn begin_function_apply_length_conversion(
    runtime: &mut Runtime,
    state: FunctionApplyContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    let realm = state.realm;
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::FunctionApplyLength(state),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

/// Renders a `ToLength` result as a binary64 value for the argument-ceiling
/// comparison.
#[expect(
    clippy::cast_precision_loss,
    reason = "a ToLength result never exceeds 2^53 - 1, which binary64 represents exactly"
)]
fn length_bound_as_f64(length: u64) -> f64 {
    length as f64
}

pub(super) fn finish_function_apply_length(
    runtime: &mut Runtime,
    mut state: FunctionApplyContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let number = operator_to_number(value, state.realm, &state.origin)?;
    // `Function.prototype.apply` reads its argument-list length with `ToLength`
    // and then enforces the pinned 65,534 call-argument ceiling
    // (`quickjs.c:41058`).
    let length_bound = number_to_length(number);
    let integer = length_bound_as_f64(length_bound);
    if integer > f64::from(MAX_FUNCTION_APPLY_ARGUMENTS) {
        return Err(function_apply_exception(
            state.realm,
            ExceptionKind::RangeError,
            "too many arguments in function call (only 65534 allowed)",
            state.origin,
        )?);
    }
    let length = u32::try_from(length_bound).map_err(|_| EngineFault::RuntimeInvariant {
        message: "apply length passed the argument ceiling but exceeded the u32 domain",
    })?;
    check_execution_limit(
        RuntimeResource::FrameValues,
        runtime.limits.max_active_frame_values,
        state
            .active_frame_values
            .saturating_add(3)
            .saturating_add(u64::from(length)),
    )?;
    state
        .arguments
        .try_reserve_exact(length as usize)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: length as usize,
        })?;
    // Charge the complete fixed scan before the first observable indexed Get.
    execution_budget.charge_instructions(u64::from(length))?;
    state.length = Some(length);
    state.stage = FunctionApplyStage::AwaitIndex;
    advance_function_apply_indices(runtime, state, return_to, execution_budget)
}

fn advance_function_apply_indices(
    runtime: &mut Runtime,
    mut state: FunctionApplyContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let length = state.length.ok_or(EngineFault::RuntimeInvariant {
        message: "apply argument scan has no fixed length",
    })?;
    while state.next_index < length {
        let index = ArrayIndex::new(state.next_index).ok_or(EngineFault::RuntimeInvariant {
            message: "apply argument index exceeds the array-index domain",
        })?;
        let key = PropertyKey::from_index(index);
        charge_heap_property_lookup(runtime, &state.array_like, execution_budget)?;
        match read_static_property(runtime, state.realm, &state.array_like, &key)? {
            PropertyReadOutcome::Value(value) => {
                state.arguments.push(value);
                state.next_index = state.next_index.saturating_add(1);
            }
            PropertyReadOutcome::Getter { function, receiver } => {
                state.stage = FunctionApplyStage::AwaitIndex;
                return function_apply_getter_call(state, function, receiver, return_to);
            }
            PropertyReadOutcome::Failed(_) => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "object-valued apply argument list failed an indexed read",
                }
                .into());
            }
        }
    }
    function_apply_target_call(
        state.target,
        state.receiver,
        state.arguments,
        return_to,
        state.origin,
        state.new_target,
        state.native_caller,
    )
}

pub(super) fn charge_heap_property_lookup(
    runtime: &Runtime,
    base: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    let mut current = Some(base.heap_reference().ok_or(EngineFault::RuntimeInvariant {
        message: "apply property lookup base has no heap reference",
    })?);
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
        let record = runtime.object_record(reference)?;
        // `ObjectRecord::own_property` is a linear shape scan. Charge its
        // complete upper bound, plus the prototype transition, before the
        // observable Get so a hostile dense array-like cannot hide O(n²)
        // native work behind O(n) fuel.
        execution_budget
            .charge_instructions(usize_to_u64(record.property_count()).saturating_add(1))?;
        current = record.prototype();
    }
    Ok(())
}

fn function_apply_getter_call(
    state: FunctionApplyContinuation,
    function: FunctionId,
    receiver: StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    let native_caller = state.native_caller;
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::FunctionApply(state));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::empty(),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller,
    }))
}

fn function_apply_target_call(
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    new_target: Option<FunctionId>,
    native_caller: Option<SyntheticNativeFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::FunctionCall);
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target,
        native_caller,
    }))
}

fn function_apply_exception(
    realm: RealmId,
    kind: ExceptionKind,
    message: &str,
    origin: JsStackFrame,
) -> Result<NativeFailure, JsStringError> {
    Ok(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}

#[allow(
    clippy::too_many_arguments,
    reason = "bind admission keeps callable validation, retained-value preflight, and native work budget explicit"
)]
fn begin_function_bind(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(target) = inputs.receiver else {
        return Err(function_apply_exception(
            realm,
            ExceptionKind::TypeError,
            "not a function",
            origin,
        )?);
    };
    let mut supplied = inputs.arguments;
    let bound_this = supplied.take_first_or_undefined();
    let bound_arguments = supplied.into_remaining_values();
    let state = FunctionBindContinuation {
        target,
        bound_this,
        bound_arguments,
        length: JsNumber::from_i32(0),
        realm,
        stage: FunctionBindStage::AwaitLengthValue,
        origin,
    };
    let target_value = StoredValue::Function(target);
    let length_key = runtime.predefined_property_key(PredefinedAtom::Length);
    let has_length = runtime
        .object_record(HeapReference::Function(target))
        .map_err(NativeFailure::from)?
        .own_property(&length_key)
        .is_some();
    if !has_length {
        return bind_name_read(runtime, state, return_to, execution_budget);
    }
    charge_heap_property_lookup(runtime, &target_value, execution_budget)?;
    match read_static_property(runtime, realm, &target_value, &length_key)? {
        PropertyReadOutcome::Value(value) => {
            bind_length_value(runtime, state, &value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            let origin = state.origin.clone();
            iterator_getter_call(
                function,
                receiver,
                NativeContinuation::FunctionBind(state),
                return_to,
                origin,
                None,
            )
        }
        PropertyReadOutcome::Failed(_) => Err(EngineFault::RuntimeInvariant {
            message: "bind target length read failed",
        }
        .into()),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "bind admission keeps callable validation, retained-value preflight, and native work budget explicit"
)]
fn advance_function_bind(
    runtime: &mut Runtime,
    state: FunctionBindContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(value) = completion else {
        return Err(EngineFault::RuntimeInvariant {
            message: "bind continuation resumed without a getter completion",
        }
        .into());
    };
    match state.stage {
        FunctionBindStage::AwaitLengthValue => {
            bind_length_value(runtime, state, &value, return_to, execution_budget)
        }
        FunctionBindStage::AwaitNameValue => bind_name_value(runtime, state, value, return_to),
    }
}

fn bind_length_value(
    runtime: &mut Runtime,
    mut state: FunctionBindContinuation,
    value: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let argument_count = u32::try_from(state.bound_arguments.len()).unwrap_or(u32::MAX);
    let length = match value {
        StoredValue::Number(number) => {
            let mut length = number.as_f64().trunc();
            if length.is_nan() || length <= f64::from(argument_count) {
                length = 0.0;
            } else {
                length -= f64::from(argument_count);
            }
            JsNumber::from_f64(length)
        }
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Function(_)
        | StoredValue::Object(_) => JsNumber::from_i32(0),
    };
    state.length = length;
    state.stage = FunctionBindStage::AwaitNameValue;
    bind_name_read(runtime, state, return_to, execution_budget)
}

fn bind_name_read(
    runtime: &mut Runtime,
    mut state: FunctionBindContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = FunctionBindStage::AwaitNameValue;
    let target_value = StoredValue::Function(state.target);
    let name_key = runtime.predefined_property_key(PredefinedAtom::Name);
    charge_heap_property_lookup(runtime, &target_value, execution_budget)?;
    match read_static_property(runtime, state.realm, &target_value, &name_key)? {
        PropertyReadOutcome::Value(value) => bind_name_value(runtime, state, value, return_to),
        PropertyReadOutcome::Getter { function, receiver } => {
            let origin = state.origin.clone();
            iterator_getter_call(
                function,
                receiver,
                NativeContinuation::FunctionBind(state),
                return_to,
                origin,
                None,
            )
        }
        PropertyReadOutcome::Failed(_) => Err(EngineFault::RuntimeInvariant {
            message: "bind target name read failed",
        }
        .into()),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "bound-function allocation keeps limit checks, property construction, and publication atomic"
)]
fn bind_name_value(
    runtime: &mut Runtime,
    state: FunctionBindContinuation,
    value: StoredValue,
    _return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let name = match value {
        StoredValue::String(value) => value,
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::Symbol(_)
        | StoredValue::Function(_)
        | StoredValue::Object(_) => JsString::empty(),
    };
    let name = JsString::from_utf8("bound ")?.concat(&name)?;
    let prototype = runtime
        .realm_function_prototype(state.realm)
        .map_err(NativeFailure::from)?;
    check_execution_limit(
        RuntimeResource::HeapFunctions,
        runtime.limits.max_heap_functions,
        usize_to_u64(runtime.functions.len()).saturating_add(1),
    )?;
    check_execution_limit(
        RuntimeResource::ObjectProperties,
        runtime.limits.max_object_properties,
        runtime.object_properties.saturating_add(2),
    )?;
    runtime
        .functions
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapFunctions,
            additional: 1,
        })?;
    let length_key = runtime.predefined_property_key(PredefinedAtom::Length);
    let name_key = runtime.predefined_property_key(PredefinedAtom::Name);
    let mut object = crate::object::ObjectRecord::empty(Some(HeapReference::Function(prototype)));
    object
        .try_reserve_data(2)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: 2,
        })?;
    object
        .append_data(
            length_key,
            PropertyLayout::data(false, false, true),
            StoredValue::Number(state.length),
        )
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: 1,
        })?;
    object
        .append_data(
            name_key,
            PropertyLayout::data(false, false, true),
            StoredValue::String(name),
        )
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: 1,
        })?;
    let function = runtime
        .functions
        .try_insert(HeapFunction {
            implementation: FunctionImplementation::Bound(BoundFunction {
                target: state.target,
                bound_this: state.bound_this,
                bound_arguments: state.bound_arguments,
            }),
            object,
            public_roots: 0,
        })
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapFunctions,
            additional: 1,
        })?;
    runtime.object_properties = runtime.object_properties.saturating_add(2);
    runtime.collection_pending = true;
    Ok(NativeDispatch::Immediate(StoredValue::Function(function)))
}

/// Returns whether a binary64 value is an exact integer.
fn is_integral(value: f64) -> bool {
    // The comparison is deliberately exact: a finite value equal to its own
    // truncation is an integer, and no tolerance applies.
    #[expect(
        clippy::float_cmp,
        reason = "integrality is an exact property, so an epsilon comparison would be wrong"
    )]
    let integral = value.is_finite() && value.trunc() == value;
    integral
}

/// Returns `Number.MAX_SAFE_INTEGER` as an exact binary64 value.
fn max_safe_integer() -> f64 {
    f64::from_bits(0x433f_ffff_ffff_ffff)
}
