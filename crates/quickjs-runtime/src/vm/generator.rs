//! Synchronous generator construction and resume state transitions.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

const ITERATOR_RESULT_FRAME_VALUES: u64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GeneratorResumeMode {
    Next,
    Return,
    Throw,
}

pub(super) fn create_generator(
    runtime: &mut Runtime,
    mut frame: Frame,
) -> Result<StoredValue, ExecutionError> {
    if frame.generator_resume.is_some()
        || frame.generator_result.is_some()
        || frame.resume_abrupt.is_some()
    {
        return Err(EngineFault::RuntimeInvariant {
            message: "fresh generator frame already has resume state",
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
            message: "generator entry does not start with initial_yield",
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
            HeapReference::Object(runtime.realm_generator_prototype(realm)?)
        }
        Some(OwnProperty::Accessor { .. }) | None => {
            return Err(EngineFault::RuntimeInvariant {
                message: "generator function lost its own prototype data property",
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
    runtime
        .generator_states
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapObjects,
            additional: 1,
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
    let previous = runtime.generator_states.insert(
        object,
        GeneratorRecord {
            state: GeneratorLifecycle::SuspendedStart,
            frame: Some(frame),
        },
    );
    debug_assert!(previous.is_none());
    runtime.collection_pending = true;
    Ok(StoredValue::Object(object))
}

fn iterator_result(
    runtime: &mut Runtime,
    realm: RealmId,
    value: StoredValue,
    done: bool,
) -> Result<NativeDispatch, NativeFailure> {
    let prepared = runtime.prepare_iterator_result_allocation(realm, None)?;
    let result = runtime.commit_prepared_iterator_result(prepared, value, done)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
}

fn generator_type_error(
    realm: RealmId,
    message: &'static str,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "generator resume owns native call inputs and keeps lifecycle, admission, and abrupt-mode transitions together"
)]
pub(super) fn begin_generator_resume(
    runtime: &mut Runtime,
    receiver: StoredValue,
    mut arguments: CallArguments,
    mode: GeneratorResumeMode,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    active_frame_values: u64,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(generator) = receiver else {
        return generator_type_error(realm, "not a generator", origin);
    };
    let argument = arguments.take_first_or_undefined();
    let (state, suspended_values) = {
        let Some(record) = runtime.generator_states.get(&generator) else {
            return generator_type_error(realm, "not a generator", origin);
        };
        (
            record.state,
            record.frame.as_ref().map(|frame| frame.reserved_values),
        )
    };

    match state {
        GeneratorLifecycle::Executing => {
            return generator_type_error(realm, "generator is already running", origin);
        }
        GeneratorLifecycle::Completed => {
            return match mode {
                GeneratorResumeMode::Next => {
                    iterator_result(runtime, realm, StoredValue::Undefined, true)
                }
                GeneratorResumeMode::Return => iterator_result(runtime, realm, argument, true),
                GeneratorResumeMode::Throw => Err(NativeFailure::Abrupt(PendingException {
                    realm,
                    payload: PendingExceptionPayload::ThrownValue(argument),
                    origin,
                })),
            };
        }
        GeneratorLifecycle::SuspendedStart => match mode {
            GeneratorResumeMode::Return => {
                let result = iterator_result(runtime, realm, argument, true)?;
                let record = runtime.generator_states.get_mut(&generator).ok_or(
                    EngineFault::StaleHeapEdge {
                        edge: "generator state",
                        index: generator.index(),
                        generation: generator.generation(),
                    },
                )?;
                record.state = GeneratorLifecycle::Completed;
                record.frame = None;
                return Ok(result);
            }
            GeneratorResumeMode::Throw => {
                let record = runtime.generator_states.get_mut(&generator).ok_or(
                    EngineFault::StaleHeapEdge {
                        edge: "generator state",
                        index: generator.index(),
                        generation: generator.generation(),
                    },
                )?;
                record.state = GeneratorLifecycle::Completed;
                record.frame = None;
                return Err(NativeFailure::Abrupt(PendingException {
                    realm,
                    payload: PendingExceptionPayload::ThrownValue(argument),
                    origin,
                }));
            }
            GeneratorResumeMode::Next => {}
        },
        GeneratorLifecycle::SuspendedYield => {}
    }

    let suspended_values = suspended_values.ok_or(EngineFault::RuntimeInvariant {
        message: "suspended generator has no frame",
    })?;
    check_execution_limit(
        RuntimeResource::FrameValues,
        runtime.limits.max_active_frame_values,
        active_frame_values
            .saturating_add(suspended_values)
            .saturating_add(ITERATOR_RESULT_FRAME_VALUES),
    )?;
    if state == GeneratorLifecycle::SuspendedYield
        && mode != GeneratorResumeMode::Throw
        && runtime
            .generator_states
            .get(&generator)
            .and_then(|record| record.frame.as_ref())
            .is_none_or(|frame| frame.stack.capacity().saturating_sub(frame.stack.len()) < 2)
    {
        return Err(EngineFault::RuntimeInvariant {
            message: "verified generator resume exceeds frame stack capacity",
        }
        .into());
    }
    let prepared_result = runtime.prepare_iterator_result_allocation(realm, None)?;
    let result =
        runtime.commit_prepared_iterator_result(prepared_result, StoredValue::Undefined, false)?;
    let record =
        runtime
            .generator_states
            .get_mut(&generator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "generator state",
                index: generator.index(),
                generation: generator.generation(),
            })?;
    let mut frame = record.frame.take().ok_or(EngineFault::RuntimeInvariant {
        message: "suspended generator has no frame",
    })?;
    record.state = GeneratorLifecycle::Executing;
    frame.generator_resume = Some(generator);
    frame.generator_result = Some(result);
    frame.reserved_values = frame.reserved_values.saturating_add(1);
    frame.return_to = return_to;
    if state == GeneratorLifecycle::SuspendedYield {
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
    } else if mode != GeneratorResumeMode::Next {
        return Err(EngineFault::RuntimeInvariant {
            message: "non-next suspended-start resume escaped immediate handling",
        }
        .into());
    }
    Ok(NativeDispatch::Frame(frame))
}

pub(super) fn suspend_generator_frame(
    runtime: &mut Runtime,
    generator: ObjectId,
    mut frame: Frame,
) -> Result<(), ExecutionError> {
    if frame.generator_result.is_some() {
        return Err(EngineFault::RuntimeInvariant {
            message: "yielding generator retained a consumed iterator result",
        }
        .into());
    }
    frame.generator_resume = None;
    frame.return_to = None;
    let record =
        runtime
            .generator_states
            .get_mut(&generator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "generator state",
                index: generator.index(),
                generation: generator.generation(),
            })?;
    if record.state != GeneratorLifecycle::Executing || record.frame.is_some() {
        return Err(EngineFault::RuntimeInvariant {
            message: "yielding generator is not executing",
        }
        .into());
    }
    record.state = GeneratorLifecycle::SuspendedYield;
    record.frame = Some(frame);
    Ok(())
}

pub(super) fn complete_generator_resume(
    runtime: &mut Runtime,
    generator: ObjectId,
) -> Result<(), ExecutionError> {
    let record =
        runtime
            .generator_states
            .get_mut(&generator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "generator state",
                index: generator.index(),
                generation: generator.generation(),
            })?;
    record.state = GeneratorLifecycle::Completed;
    record.frame = None;
    Ok(())
}

pub(super) fn complete_active_generator_resumes(
    runtime: &mut Runtime,
    frames: &[Frame],
) -> Result<(), ExecutionError> {
    for generator in frames.iter().filter_map(|frame| frame.generator_resume) {
        complete_generator_resume(runtime, generator)?;
    }
    Ok(())
}
