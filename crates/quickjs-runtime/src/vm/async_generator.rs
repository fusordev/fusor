//! Promise-backed asynchronous-generator request queues and suspension.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

fn allocate_request_capability(
    runtime: &mut Runtime,
    realm: RealmId,
) -> Result<crate::object::PromiseCapability, ExecutionError> {
    let prototype = runtime.realm_promise_prototype(realm)?;
    let promise = runtime.allocate_promise_with_prototype(HeapReference::Object(prototype))?;
    let (resolve, reject) = runtime.allocate_promise_resolving_functions(promise, realm)?;
    Ok(crate::object::PromiseCapability {
        promise: StoredValue::Object(promise),
        resolve,
        reject,
    })
}

fn request_promise(request: &AsyncGeneratorRequest) -> StoredValue {
    request.capability.promise.duplicate()
}

fn async_generator_type_error(
    runtime: &mut Runtime,
    capability: &crate::object::PromiseCapability,
    realm: RealmId,
    message: &'static str,
) -> Result<(), NativeFailure> {
    let error = runtime.materialize_error_object(
        realm,
        ExceptionKind::TypeError,
        JsString::from_utf8(message)?,
        None,
    )?;
    let StoredValue::Object(promise) = capability.promise else {
        return Err(EngineFault::RuntimeInvariant {
            message: "async-generator capability has no Promise object",
        }
        .into());
    };
    reject_promise(runtime, promise, StoredValue::Object(error))
}

fn iterator_result_value(
    runtime: &mut Runtime,
    realm: RealmId,
    value: StoredValue,
    done: bool,
) -> Result<StoredValue, NativeFailure> {
    let prepared = runtime.prepare_iterator_result_allocation(realm, None)?;
    let result = runtime.commit_prepared_iterator_result(prepared, value, done)?;
    Ok(StoredValue::Object(result))
}

fn settle_request(
    runtime: &mut Runtime,
    request: &AsyncGeneratorRequest,
    value: StoredValue,
    done: bool,
) -> Result<(), NativeFailure> {
    let StoredValue::Object(promise) = &request.capability.promise else {
        return Err(EngineFault::RuntimeInvariant {
            message: "async-generator capability has no Promise object",
        }
        .into());
    };
    let result = iterator_result_value(runtime, request.realm, value, done)?;
    fulfill_promise(runtime, *promise, result)
}

fn drain_completed_queue(
    runtime: &mut Runtime,
    generator: ObjectId,
    completion: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        let front = runtime
            .async_generator_states
            .get(&generator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "async-generator state",
                index: generator.index(),
                generation: generator.generation(),
            })?
            .queue
            .front()
            .map(|request| {
                (
                    request.mode,
                    request.value.duplicate(),
                    request.origin.clone(),
                )
            });
        let Some((mode, value, origin)) = front else {
            let record = runtime.async_generator_states.get_mut(&generator).ok_or(
                EngineFault::StaleHeapEdge {
                    edge: "async-generator state",
                    index: generator.index(),
                    generation: generator.generation(),
                },
            )?;
            record.state = AsyncGeneratorLifecycle::Completed;
            return Ok(NativeDispatch::Immediate(completion));
        };
        if mode == GeneratorResumeMode::Return {
            return begin_async_generator_return_await(
                runtime,
                generator,
                AsyncGeneratorAwaitKind::ReturnComplete,
                completion,
                value,
                None,
                origin,
                execution_budget,
            );
        }
        let request = runtime
            .async_generator_states
            .get_mut(&generator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "async-generator state",
                index: generator.index(),
                generation: generator.generation(),
            })?
            .queue
            .pop_front()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "completed async-generator drain lost its front request",
            })?;
        if mode == GeneratorResumeMode::Throw {
            let StoredValue::Object(promise) = request.capability.promise else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "async-generator capability has no Promise object",
                }
                .into());
            };
            reject_promise(runtime, promise, value)?;
        } else {
            settle_request(runtime, &request, StoredValue::Undefined, true)?;
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "async-generator creation validates and commits one failure-atomic suspension record"
)]
pub(super) fn create_async_generator(
    runtime: &mut Runtime,
    mut frame: Frame,
) -> Result<StoredValue, ExecutionError> {
    if frame.generator_resume.is_some()
        || frame.generator_result.is_some()
        || frame.resume_abrupt.is_some()
    {
        return Err(EngineFault::RuntimeInvariant {
            message: "fresh async-generator frame already has resume state",
        }
        .into());
    }
    let instruction = code(runtime, frame.code)?
        .authority
        .function(frame.template)
        .and_then(|function| {
            function
                .function()
                .control_flow()
                .instruction(frame.instruction)
        })
        .ok_or(EngineFault::MissingInstruction {
            function: frame.template,
            instruction: frame.instruction.get(),
        })?;
    if instruction.decoded().instruction().opcode() != FinalOpcode::InitialYield {
        return Err(EngineFault::RuntimeInvariant {
            message: "async-generator entry does not start with initial_yield",
        }
        .into());
    }
    frame.instruction =
        instruction
            .successors()
            .fallthrough()
            .ok_or(EngineFault::InvalidSuccessor {
                function: frame.template,
                pc: instruction.decoded().pc(),
            })?;
    frame.return_to = None;

    let realm = code(runtime, frame.code)?.realm;
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let prototype = match runtime
        .object_record(HeapReference::Function(frame.function))?
        .own_property(&prototype_key)
    {
        Some(OwnProperty::Data {
            value: StoredValue::Function(function),
            ..
        }) => HeapReference::Function(function),
        Some(OwnProperty::Data {
            value: StoredValue::Object(object),
            ..
        }) => HeapReference::Object(object),
        Some(OwnProperty::Data { .. }) => {
            HeapReference::Object(runtime.realm_async_generator_prototype(realm)?)
        }
        None => HeapReference::Object(runtime.realm_async_generator_prototype(realm)?),
        Some(OwnProperty::Accessor { .. }) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "async-generator function has an own prototype accessor",
            }
            .into());
        }
    };
    check_execution_limit(
        RuntimeResource::HeapObjects,
        runtime.limits.max_heap_objects,
        usize_to_u64(runtime.objects.len()).saturating_add(1),
    )?;
    runtime
        .objects
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapObjects,
            additional: 1,
        })?;
    runtime.async_generator_states.try_reserve(1).map_err(|_| {
        ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapObjects,
            additional: 1,
        }
    })?;
    let object = runtime
        .objects
        .try_insert(crate::object::HeapObject::ordinary(
            crate::object::ObjectRecord::empty(Some(prototype)),
        ))
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapObjects,
            additional: 1,
        })?;
    let previous = runtime.async_generator_states.insert(
        object,
        AsyncGeneratorRecord {
            state: AsyncGeneratorLifecycle::SuspendedStart,
            frame: Some(frame),
            queue: VecDeque::new(),
            awaiting: None,
        },
    );
    debug_assert!(previous.is_none());
    runtime.collection_pending = true;
    Ok(StoredValue::Object(object))
}

pub(super) fn complete_async_generator_resume(
    runtime: &mut Runtime,
    generator: ObjectId,
) -> Result<(), ExecutionError> {
    let record =
        runtime
            .async_generator_states
            .get_mut(&generator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "async-generator state",
                index: generator.index(),
                generation: generator.generation(),
            })?;
    record.state = AsyncGeneratorLifecycle::Completed;
    record.frame = None;
    record.awaiting = None;
    runtime.collection_pending = true;
    Ok(())
}

fn resume_front_request(
    runtime: &mut Runtime,
    generator: ObjectId,
    return_to: Option<CallReturn>,
) -> Result<Frame, NativeFailure> {
    let (state, mode, argument, realm, origin) =
        {
            let record = runtime.async_generator_states.get(&generator).ok_or(
                EngineFault::StaleHeapEdge {
                    edge: "async-generator state",
                    index: generator.index(),
                    generation: generator.generation(),
                },
            )?;
            let request = record.queue.front().ok_or(EngineFault::RuntimeInvariant {
                message: "async-generator resume has no queued request",
            })?;
            (
                record.state,
                request.mode,
                request.value.duplicate(),
                request.realm,
                request.origin.clone(),
            )
        };
    let record =
        runtime
            .async_generator_states
            .get_mut(&generator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "async-generator state",
                index: generator.index(),
                generation: generator.generation(),
            })?;
    let mut frame = record.frame.take().ok_or(EngineFault::RuntimeInvariant {
        message: "suspended async generator has no frame",
    })?;
    record.state = AsyncGeneratorLifecycle::Executing;
    frame.generator_resume = Some(generator);
    frame.return_to = return_to;
    if state == AsyncGeneratorLifecycle::SuspendedYieldStar {
        let resume_mode = match mode {
            GeneratorResumeMode::Next => 0,
            GeneratorResumeMode::Return => 1,
            GeneratorResumeMode::Throw => 2,
        };
        push(&mut frame, argument);
        push(
            &mut frame,
            StoredValue::Number(JsNumber::from_i32(resume_mode)),
        );
    } else if state == AsyncGeneratorLifecycle::SuspendedYield {
        match mode {
            GeneratorResumeMode::Next | GeneratorResumeMode::Return => {
                push(&mut frame, argument);
                push(
                    &mut frame,
                    StoredValue::Boolean(mode == GeneratorResumeMode::Return),
                );
            }
            GeneratorResumeMode::Throw => {
                frame.resume_abrupt = Some(PendingException {
                    realm,
                    payload: PendingExceptionPayload::ThrownValue(argument),
                    origin,
                });
            }
        }
    }
    Ok(frame)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the intrinsic call owns one complete queued request and dispatches every lifecycle state"
)]
pub(super) fn begin_async_generator_resume(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    mode: GeneratorResumeMode,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let capability = allocate_request_capability(runtime, realm)?;
    let promise = capability.promise.duplicate();
    let argument = arguments.take_first_or_undefined();
    let return_value = argument.duplicate();
    let StoredValue::Object(generator) = receiver else {
        async_generator_type_error(runtime, &capability, realm, "not an async generator")?;
        return Ok(NativeDispatch::Immediate(promise));
    };
    let generator = *generator;
    let Some(state) = runtime
        .async_generator_states
        .get(&generator)
        .map(|record| record.state)
    else {
        async_generator_type_error(runtime, &capability, realm, "not an async generator")?;
        return Ok(NativeDispatch::Immediate(promise));
    };
    let request_origin = origin.clone();
    let request = AsyncGeneratorRequest {
        mode,
        value: argument,
        capability,
        realm,
        origin,
    };
    let record =
        runtime
            .async_generator_states
            .get_mut(&generator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "async-generator state",
                index: generator.index(),
                generation: generator.generation(),
            })?;
    record
        .queue
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    record.queue.push_back(request);

    if state == AsyncGeneratorLifecycle::Completed {
        record.state = AsyncGeneratorLifecycle::DrainingQueue;
        if mode == GeneratorResumeMode::Return {
            return begin_async_generator_return_await(
                runtime,
                generator,
                AsyncGeneratorAwaitKind::ReturnComplete,
                promise,
                return_value,
                return_to,
                request_origin,
                execution_budget,
            );
        }
        return drain_completed_queue(runtime, generator, promise, execution_budget);
    }
    if state == AsyncGeneratorLifecycle::Executing
        || state == AsyncGeneratorLifecycle::DrainingQueue
    {
        return Ok(NativeDispatch::Immediate(promise));
    }
    if state == AsyncGeneratorLifecycle::SuspendedStart && mode != GeneratorResumeMode::Next {
        let record = runtime.async_generator_states.get_mut(&generator).ok_or(
            EngineFault::StaleHeapEdge {
                edge: "async-generator state",
                index: generator.index(),
                generation: generator.generation(),
            },
        )?;
        record.frame = None;
        record.state = AsyncGeneratorLifecycle::DrainingQueue;
        if mode == GeneratorResumeMode::Return {
            return begin_async_generator_return_await(
                runtime,
                generator,
                AsyncGeneratorAwaitKind::ReturnComplete,
                promise,
                return_value,
                return_to,
                request_origin,
                execution_budget,
            );
        }
        return drain_completed_queue(runtime, generator, promise, execution_budget);
    }
    if matches!(
        state,
        AsyncGeneratorLifecycle::SuspendedYield | AsyncGeneratorLifecycle::SuspendedYieldStar
    ) && mode == GeneratorResumeMode::Return
    {
        let record = runtime.async_generator_states.get_mut(&generator).ok_or(
            EngineFault::StaleHeapEdge {
                edge: "async-generator state",
                index: generator.index(),
                generation: generator.generation(),
            },
        )?;
        record.state = AsyncGeneratorLifecycle::Executing;
        return begin_async_generator_return_await(
            runtime,
            generator,
            if state == AsyncGeneratorLifecycle::SuspendedYieldStar {
                AsyncGeneratorAwaitKind::ReturnResumeYieldStar
            } else {
                AsyncGeneratorAwaitKind::ReturnResume
            },
            promise,
            return_value,
            return_to,
            request_origin,
            execution_budget,
        );
    }
    Ok(NativeDispatch::Frame(resume_front_request(
        runtime, generator, return_to,
    )?))
}

pub(super) fn suspend_async_generator_await(
    runtime: &mut Runtime,
    generator: ObjectId,
    mut frame: Frame,
    promise: ObjectId,
    origin: JsStackFrame,
) -> Result<(StoredValue, Option<CallReturn>), ExecutionError> {
    let return_to = frame.return_to.take();
    let result = runtime
        .async_generator_states
        .get(&generator)
        .and_then(|record| record.queue.front())
        .map(request_promise)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "awaiting async generator has no queued request",
        })?;
    let record =
        runtime
            .async_generator_states
            .get_mut(&generator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "async-generator state",
                index: generator.index(),
                generation: generator.generation(),
            })?;
    if record.state != AsyncGeneratorLifecycle::Executing
        || record.frame.is_some()
        || record.awaiting.is_some()
    {
        return Err(EngineFault::RuntimeInvariant {
            message: "async-generator await has inconsistent activation state",
        }
        .into());
    }
    record.frame = Some(frame);
    record.awaiting = Some(AsyncGeneratorAwait {
        promise,
        origin,
        kind: AsyncGeneratorAwaitKind::Body,
    });
    perform_async_generator_await(runtime, promise, generator).map_err(
        |failure| match failure {
            NativeFailure::Execution(error) => error,
            NativeFailure::Abrupt(_) | NativeFailure::AbruptAfterTransient(_) => {
                EngineFault::RuntimeInvariant {
                    message: "internal async-generator reaction registration threw JavaScript",
                }
                .into()
            }
        },
    )?;
    runtime.collection_pending = true;
    Ok((result, return_to))
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive await-kind transition preserves distinct body, return, delegated-return, and completion semantics"
)]
pub(super) fn begin_async_generator_await_resume(
    runtime: &mut Runtime,
    generator: ObjectId,
    kind: crate::object::PromiseReactionKind,
    argument: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let record =
        runtime
            .async_generator_states
            .get_mut(&generator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "async-generator state",
                index: generator.index(),
                generation: generator.generation(),
            })?;
    let awaited = record
        .awaiting
        .take()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "async-generator reaction lost its awaited Promise",
        })?;
    if !runtime.objects.contains(awaited.promise) {
        return Err(EngineFault::StaleHeapEdge {
            edge: "suspended async-generator await Promise",
            index: awaited.promise.index(),
            generation: awaited.promise.generation(),
        }
        .into());
    }
    match awaited.kind {
        AsyncGeneratorAwaitKind::Body => {
            let mut frame = record.frame.take().ok_or(EngineFault::RuntimeInvariant {
                message: "async-generator reaction lost its suspended frame",
            })?;
            match kind {
                crate::object::PromiseReactionKind::Fulfill => push(&mut frame, argument),
                crate::object::PromiseReactionKind::Reject => {
                    let realm = code(runtime, frame.code)?.realm;
                    frame.resume_abrupt = Some(PendingException {
                        realm,
                        payload: PendingExceptionPayload::ThrownValue(argument),
                        origin: awaited.origin,
                    });
                }
            }
            runtime.collection_pending = true;
            Ok(NativeDispatch::Frame(frame))
        }
        AsyncGeneratorAwaitKind::ReturnResume => {
            let mut frame = record.frame.take().ok_or(EngineFault::RuntimeInvariant {
                message: "async-generator return reaction lost its suspended frame",
            })?;
            frame.generator_resume = Some(generator);
            record.state = AsyncGeneratorLifecycle::Executing;
            match kind {
                crate::object::PromiseReactionKind::Fulfill => {
                    push(&mut frame, argument);
                    push(&mut frame, StoredValue::Boolean(true));
                }
                crate::object::PromiseReactionKind::Reject => {
                    let realm = code(runtime, frame.code)?.realm;
                    frame.resume_abrupt = Some(PendingException {
                        realm,
                        payload: PendingExceptionPayload::ThrownValue(argument),
                        origin: awaited.origin,
                    });
                }
            }
            runtime.collection_pending = true;
            Ok(NativeDispatch::Frame(frame))
        }
        AsyncGeneratorAwaitKind::ReturnResumeYieldStar => {
            let mut frame = record.frame.take().ok_or(EngineFault::RuntimeInvariant {
                message: "async-generator delegated return reaction lost its suspended frame",
            })?;
            frame.generator_resume = Some(generator);
            record.state = AsyncGeneratorLifecycle::Executing;
            let resume_mode = match kind {
                crate::object::PromiseReactionKind::Fulfill => 1,
                crate::object::PromiseReactionKind::Reject => 2,
            };
            push(&mut frame, argument);
            push(
                &mut frame,
                StoredValue::Number(JsNumber::from_i32(resume_mode)),
            );
            runtime.collection_pending = true;
            Ok(NativeDispatch::Frame(frame))
        }
        AsyncGeneratorAwaitKind::ReturnComplete => {
            let request = record
                .queue
                .pop_front()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "completed async-generator return lost its queued request",
                })?;
            let StoredValue::Object(output) = request.capability.promise else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "async-generator capability has no Promise object",
                }
                .into());
            };
            match kind {
                crate::object::PromiseReactionKind::Fulfill => {
                    let result = iterator_result_value(runtime, request.realm, argument, true)?;
                    fulfill_promise(runtime, output, result)?;
                }
                crate::object::PromiseReactionKind::Reject => {
                    reject_promise(runtime, output, argument)?;
                }
            }
            runtime.collection_pending = true;
            drain_completed_queue(runtime, generator, StoredValue::Undefined, execution_budget)
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "PromiseResolve return-await setup carries its typed generator completion"
)]
fn begin_async_generator_return_await(
    runtime: &mut Runtime,
    generator: ObjectId,
    kind: AsyncGeneratorAwaitKind,
    completion: StoredValue,
    value: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let realm = runtime
        .async_generator_states
        .get(&generator)
        .and_then(|record| record.queue.front())
        .map(|request| request.realm)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "async-generator return await has no queued request",
        })?;
    let constructor = runtime.realm_promise_constructor(realm)?;
    let dispatch = begin_promise_resolve_with_constructor(
        runtime,
        realm,
        constructor,
        value,
        return_to,
        origin.clone(),
        execution_budget,
    )?;
    match dispatch {
        NativeDispatch::Immediate(value) => finish_async_generator_return_await(
            runtime, generator, kind, origin, completion, &value,
        ),
        NativeDispatch::Call(mut call) => {
            let mut outer = Vec::new();
            outer
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 1,
                })?;
            outer.push(NativeContinuation::AsyncGeneratorReturnAwait {
                generator,
                kind,
                origin,
                completion,
            });
            prepend_native_continuations(&mut call, outer)?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            let mut outer = Vec::new();
            outer
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 1,
                })?;
            outer.push(NativeContinuation::AsyncGeneratorReturnAwait {
                generator,
                kind,
                origin,
                completion,
            });
            attach_native_continuations(&mut frame, outer)?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "async-generator return PromiseResolve produced an invalid dispatch",
        }
        .into()),
    }
}

pub(super) fn resume_async_generator_return_await_abrupt(
    runtime: &mut Runtime,
    generator: ObjectId,
    kind: AsyncGeneratorAwaitKind,
    completion: StoredValue,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match kind {
        AsyncGeneratorAwaitKind::Body => Err(EngineFault::RuntimeInvariant {
            message: "body await used the async-generator return abrupt continuation",
        }
        .into()),
        AsyncGeneratorAwaitKind::ReturnResume | AsyncGeneratorAwaitKind::ReturnResumeYieldStar => {
            let record = runtime.async_generator_states.get_mut(&generator).ok_or(
                EngineFault::StaleHeapEdge {
                    edge: "async-generator state",
                    index: generator.index(),
                    generation: generator.generation(),
                },
            )?;
            if record.state != AsyncGeneratorLifecycle::Executing || record.awaiting.is_some() {
                return Err(EngineFault::RuntimeInvariant {
                    message: "async-generator return abrupt continuation has invalid state",
                }
                .into());
            }
            let mut frame = record.frame.take().ok_or(EngineFault::RuntimeInvariant {
                message: "async-generator return abrupt continuation lost its frame",
            })?;
            frame.generator_resume = Some(generator);
            frame.return_to = return_to;
            if kind == AsyncGeneratorAwaitKind::ReturnResumeYieldStar {
                let reason = pending_exception_value(runtime, pending)?;
                push(&mut frame, reason);
                push(&mut frame, StoredValue::Number(JsNumber::from_i32(2)));
            } else {
                frame.resume_abrupt = Some(pending);
            }
            runtime.collection_pending = true;
            Ok(NativeDispatch::Frame(frame))
        }
        AsyncGeneratorAwaitKind::ReturnComplete => {
            let reason = pending_exception_value(runtime, pending)?;
            let request = runtime
                .async_generator_states
                .get_mut(&generator)
                .ok_or(EngineFault::StaleHeapEdge {
                    edge: "async-generator state",
                    index: generator.index(),
                    generation: generator.generation(),
                })?
                .queue
                .pop_front()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "completed async-generator return abrupt continuation lost its request",
                })?;
            let StoredValue::Object(promise) = request.capability.promise else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "async-generator capability has no Promise object",
                }
                .into());
            };
            reject_promise(runtime, promise, reason)?;
            let record = runtime.async_generator_states.get_mut(&generator).ok_or(
                EngineFault::StaleHeapEdge {
                    edge: "async-generator state",
                    index: generator.index(),
                    generation: generator.generation(),
                },
            )?;
            record.state = AsyncGeneratorLifecycle::DrainingQueue;
            record.frame = None;
            record.awaiting = None;
            runtime.collection_pending = true;
            drain_completed_queue(runtime, generator, completion, execution_budget)
        }
    }
}

pub(super) fn finish_async_generator_return_await(
    runtime: &mut Runtime,
    generator: ObjectId,
    kind: AsyncGeneratorAwaitKind,
    origin: JsStackFrame,
    completion: StoredValue,
    resolved: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(promise) = resolved else {
        return Err(EngineFault::RuntimeInvariant {
            message: "async-generator return PromiseResolve produced a non-Promise",
        }
        .into());
    };
    let record =
        runtime
            .async_generator_states
            .get_mut(&generator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "async-generator state",
                index: generator.index(),
                generation: generator.generation(),
            })?;
    if record.awaiting.is_some() {
        return Err(EngineFault::RuntimeInvariant {
            message: "async-generator return registered two awaited Promises",
        }
        .into());
    }
    record.awaiting = Some(AsyncGeneratorAwait {
        promise: *promise,
        origin,
        kind,
    });
    perform_async_generator_await(runtime, *promise, generator)?;
    runtime.collection_pending = true;
    Ok(NativeDispatch::Immediate(completion))
}

pub(super) enum AsyncGeneratorYieldOutcome {
    Suspended,
    Frame(Frame),
    Dispatch(NativeDispatch),
}

pub(super) fn finish_async_generator_yield(
    runtime: &mut Runtime,
    generator: ObjectId,
    mut frame: Frame,
    value: StoredValue,
    suspension: AsyncGeneratorLifecycle,
    execution_budget: &mut ExecutionBudget,
) -> Result<AsyncGeneratorYieldOutcome, NativeFailure> {
    if !matches!(
        suspension,
        AsyncGeneratorLifecycle::SuspendedYield | AsyncGeneratorLifecycle::SuspendedYieldStar
    ) {
        return Err(EngineFault::RuntimeInvariant {
            message: "async-generator yield received an invalid suspension kind",
        }
        .into());
    }
    frame.generator_resume = None;
    frame.return_to = None;
    let request = runtime
        .async_generator_states
        .get_mut(&generator)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "async-generator state",
            index: generator.index(),
            generation: generator.generation(),
        })?
        .queue
        .pop_front()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "yielding async generator has no queued request",
        })?;
    settle_request(runtime, &request, value, false)?;
    let record =
        runtime
            .async_generator_states
            .get_mut(&generator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "async-generator state",
                index: generator.index(),
                generation: generator.generation(),
            })?;
    record.state = suspension;
    record.frame = Some(frame);
    if record.queue.is_empty() {
        return Ok(AsyncGeneratorYieldOutcome::Suspended);
    }
    let (mode, value, origin) = {
        let request = record.queue.front().ok_or(EngineFault::RuntimeInvariant {
            message: "queued async-generator resume disappeared",
        })?;
        (
            request.mode,
            request.value.duplicate(),
            request.origin.clone(),
        )
    };
    if mode == GeneratorResumeMode::Return {
        record.state = AsyncGeneratorLifecycle::Executing;
        let dispatch = begin_async_generator_return_await(
            runtime,
            generator,
            if suspension == AsyncGeneratorLifecycle::SuspendedYieldStar {
                AsyncGeneratorAwaitKind::ReturnResumeYieldStar
            } else {
                AsyncGeneratorAwaitKind::ReturnResume
            },
            StoredValue::Undefined,
            value,
            None,
            origin,
            execution_budget,
        )?;
        return Ok(AsyncGeneratorYieldOutcome::Dispatch(dispatch));
    }
    Ok(AsyncGeneratorYieldOutcome::Frame(resume_front_request(
        runtime, generator, None,
    )?))
}

pub(super) fn finish_async_generator_return(
    runtime: &mut Runtime,
    generator: ObjectId,
    value: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let request = runtime
        .async_generator_states
        .get_mut(&generator)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "async-generator state",
            index: generator.index(),
            generation: generator.generation(),
        })?
        .queue
        .pop_front()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "returning async generator has no queued request",
        })?;
    let promise = request_promise(&request);
    settle_request(runtime, &request, value, true)?;
    let record =
        runtime
            .async_generator_states
            .get_mut(&generator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "async-generator state",
                index: generator.index(),
                generation: generator.generation(),
            })?;
    record.state = AsyncGeneratorLifecycle::DrainingQueue;
    record.frame = None;
    record.awaiting = None;
    drain_completed_queue(runtime, generator, promise, execution_budget)
}

pub(super) fn complete_async_generator_throw(
    runtime: &mut Runtime,
    generator: ObjectId,
    value: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let request = runtime
        .async_generator_states
        .get_mut(&generator)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "async-generator state",
            index: generator.index(),
            generation: generator.generation(),
        })?
        .queue
        .pop_front()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "throwing async generator has no queued request",
        })?;
    let promise = request_promise(&request);
    let StoredValue::Object(output) = request.capability.promise else {
        return Err(EngineFault::RuntimeInvariant {
            message: "async-generator capability has no Promise object",
        }
        .into());
    };
    reject_promise(runtime, output, value)?;
    let record =
        runtime
            .async_generator_states
            .get_mut(&generator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "async-generator state",
                index: generator.index(),
                generation: generator.generation(),
            })?;
    record.state = AsyncGeneratorLifecycle::DrainingQueue;
    record.frame = None;
    record.awaiting = None;
    drain_completed_queue(runtime, generator, promise, execution_budget)
}
