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

//! JavaScript exception construction, provenance, and iterative unwinding.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) fn tdz_exception(
    runtime: &Runtime,
    frame: &Frame,
    binding: BindingName,
    pc: BytecodePc,
) -> Result<PendingException, ExecutionError> {
    let code = code(runtime, frame.code)?;
    let function =
        code.authority
            .function(frame.template)
            .ok_or(EngineFault::InvalidClosureEnvironment {
                function: frame.template,
            })?;
    let installed = installed_template(runtime, frame.code, frame.template)?;
    let domains = function.function().control_flow().domains();
    let atom = match binding {
        BindingName::Local(index) => function
            .metadata()
            .variables()
            .get(domains.argument_count() as usize + index as usize)
            .and_then(quickjs_bytecode::VariableDefinition::name),
        BindingName::Closure(index) => function
            .metadata()
            .closures()
            .get(index as usize)
            .and_then(quickjs_bytecode::ClosureVariableDefinition::name),
    };
    let message = if let Some(atom) = atom
        && let Some(name) = installed
            .atoms
            .get(atom.get() as usize)
            .and_then(AtomDescription::description)
    {
        name.concat(&JsString::from_utf8(" is not initialized")?)?
    } else {
        JsString::from_utf8("lexical variable is not initialized")?
    };
    Ok(PendingException {
        realm: code.realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::ReferenceError,
            message,
        },
        origin: instruction_location(runtime, frame, pc)?,
    })
}

pub(super) fn global_not_defined_exception(
    runtime: &Runtime,
    frame: &Frame,
    name: &JsString,
    pc: BytecodePc,
) -> Result<PendingException, ExecutionError> {
    let realm = code(runtime, frame.code)?.realm;
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::ReferenceError,
            message: named_property_message("'", name, "' is not defined")?,
        },
        origin: instruction_location(runtime, frame, pc)?,
    })
}

pub(super) fn not_callable_exception(
    runtime: &Runtime,
    frame: &Frame,
    pc: BytecodePc,
) -> Result<PendingException, ExecutionError> {
    let realm = code(runtime, frame.code)?.realm;
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a function")?,
        },
        origin: instruction_location(runtime, frame, pc)?,
    })
}

pub(super) fn not_constructor_exception(
    runtime: &Runtime,
    frame: &Frame,
    pc: BytecodePc,
) -> Result<PendingException, ExecutionError> {
    let realm = code(runtime, frame.code)?.realm;
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a constructor")?,
        },
        origin: instruction_location(runtime, frame, pc)?,
    })
}

pub(super) fn function_not_constructor_message(
    runtime: &Runtime,
    function: FunctionId,
) -> Result<JsString, ExecutionError> {
    let name_key = runtime.predefined_property_key(PredefinedAtom::Name);
    let name = match runtime
        .object_record(HeapReference::Function(function))?
        .own_property(&name_key)
    {
        Some(OwnProperty::Data {
            value: StoredValue::String(name),
            ..
        }) => name,
        Some(OwnProperty::Data { .. } | OwnProperty::Accessor { .. }) | None => JsString::empty(),
    };
    if name.is_empty() {
        return Ok(JsString::from_utf8("not a constructor")?);
    }
    Ok(name.concat(&JsString::from_utf8(" is not a constructor")?)?)
}

pub(super) fn property_exception(
    runtime: &Runtime,
    frame: &Frame,
    pc: BytecodePc,
    name: &JsString,
    failure: PropertyFailure,
) -> Result<PendingException, ExecutionError> {
    let realm = code(runtime, frame.code)?.realm;
    property_exception_at(
        realm,
        instruction_location(runtime, frame, pc)?,
        Some(name),
        failure,
    )
}

pub(super) fn property_exception_at(
    realm: RealmId,
    origin: JsStackFrame,
    name: Option<&JsString>,
    failure: PropertyFailure,
) -> Result<PendingException, ExecutionError> {
    let message = match failure {
        PropertyFailure::ReadNull => match name {
            Some(name) => named_property_message("cannot read property '", name, "' of null")?,
            None => JsString::from_utf8("cannot read property of null")?,
        },
        PropertyFailure::ReadUndefined => match name {
            Some(name) => named_property_message("cannot read property '", name, "' of undefined")?,
            None => JsString::from_utf8("cannot read property of undefined")?,
        },
        PropertyFailure::WriteNull => named_property_message(
            "cannot set property '",
            required_property_name(name)?,
            "' of null",
        )?,
        PropertyFailure::WriteUndefined => named_property_message(
            "cannot set property '",
            required_property_name(name)?,
            "' of undefined",
        )?,
        PropertyFailure::NotObject => JsString::from_utf8("not an object")?,
        PropertyFailure::ReadOnly => {
            named_property_message("'", required_property_name(name)?, "' is read-only")?
        }
        PropertyFailure::NoSetter => JsString::from_utf8("no setter for property")?,
        PropertyFailure::NotConfigurable => JsString::from_utf8("property is not configurable")?,
        PropertyFailure::NonExtensible => JsString::from_utf8("object is not extensible")?,
        // `delete` coerces its base with `ToObject`, so a nullish base reports
        // the conversion failure rather than a property-read failure
        // (`quickjs.c:10926`).
        PropertyFailure::DeleteNull | PropertyFailure::DeleteUndefined => {
            JsString::from_utf8("cannot convert to object")?
        }
        PropertyFailure::NotDeletable => JsString::from_utf8("could not delete property")?,
    };
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message,
        },
        origin,
    })
}

fn required_property_name(name: Option<&JsString>) -> Result<&JsString, EngineFault> {
    name.ok_or(EngineFault::RuntimeInvariant {
        message: "property failure requiring a name did not retain one",
    })
}

fn named_property_message(
    prefix: &str,
    name: &JsString,
    suffix: &str,
) -> Result<JsString, JsStringError> {
    JsString::from_utf8(prefix)?
        .concat(name)?
        .concat(&JsString::from_utf8(suffix)?)
}

pub(super) fn instruction_location(
    runtime: &Runtime,
    frame: &Frame,
    pc: BytecodePc,
) -> Result<JsStackFrame, EngineFault> {
    let function = code(runtime, frame.code)?
        .authority
        .function(frame.template)
        .ok_or(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        })?;
    let mapping = function
        .metadata()
        .source()
        .mappings()
        .get(frame.instruction.get() as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "source mapping",
            index: frame.instruction.get(),
        })?;
    Ok(JsStackFrame::new(
        frame.template,
        pc,
        function.metadata().source().display_name_arc(),
        function.metadata().source().text_arc(),
        mapping.span(),
    ))
}

fn exception_caller_frames(
    runtime: &Runtime,
    frames: &[Frame],
) -> Result<Vec<JsStackFrame>, ExecutionError> {
    let caller_count = frames.len().saturating_sub(1);
    let mut caller_frames = Vec::new();
    caller_frames.try_reserve_exact(caller_count).map_err(|_| {
        ExecutionError::AllocationFailed {
            resource: RuntimeResource::ExceptionFrames,
            additional: caller_count,
        }
    })?;
    for caller in frames[..caller_count].iter().rev() {
        let instruction = code(runtime, caller.code)?
            .authority
            .function(caller.template)
            .and_then(|function| {
                function
                    .function()
                    .control_flow()
                    .instruction(caller.instruction)
            })
            .ok_or(EngineFault::MissingInstruction {
                function: caller.template,
                instruction: caller.instruction.get(),
            })?;
        if !matches!(
            instruction.decoded().instruction().opcode(),
            FinalOpcode::Call
                | FinalOpcode::Call0
                | FinalOpcode::Call1
                | FinalOpcode::Call2
                | FinalOpcode::Call3
                | FinalOpcode::CallMethod
                | FinalOpcode::CallConstructor
                | FinalOpcode::GetField
                | FinalOpcode::GetField2
                | FinalOpcode::PutField
                | FinalOpcode::GetArrayEl
                | FinalOpcode::GetArrayEl2
                | FinalOpcode::PutArrayEl
                | FinalOpcode::Apply
                | FinalOpcode::Append
                | FinalOpcode::ForOfStart
                | FinalOpcode::ForOfNext
                | FinalOpcode::IteratorClose
                | FinalOpcode::ToPropKey
                | FinalOpcode::DefineMethodComputed
                | FinalOpcode::Neg
                | FinalOpcode::Plus
                | FinalOpcode::Dec
                | FinalOpcode::Inc
                | FinalOpcode::PostDec
                | FinalOpcode::PostInc
                | FinalOpcode::Not
                | FinalOpcode::Mul
                | FinalOpcode::Div
                | FinalOpcode::Mod
                | FinalOpcode::Add
                | FinalOpcode::Sub
                | FinalOpcode::Pow
                | FinalOpcode::Shl
                | FinalOpcode::Sar
                | FinalOpcode::Shr
                | FinalOpcode::Lt
                | FinalOpcode::Lte
                | FinalOpcode::Gt
                | FinalOpcode::Gte
                | FinalOpcode::Eq
                | FinalOpcode::Neq
                | FinalOpcode::And
                | FinalOpcode::Xor
                | FinalOpcode::Or
        ) {
            return Err(EngineFault::RuntimeInvariant {
                message: "exception caller is not parked at a call or abstract operation",
            }
            .into());
        }
        caller_frames.push(instruction_location(
            runtime,
            caller,
            instruction.decoded().pc(),
        )?);
    }
    Ok(caller_frames)
}

#[allow(
    clippy::too_many_lines,
    reason = "catch and abrupt native-continuation unwinding share one iterative ownership boundary"
)]
pub(super) fn dispatch_pending_exception(
    runtime: &mut Runtime,
    frames: &mut Vec<Frame>,
    active_frame_values: &mut u64,
    mut pending: PendingException,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), ExecutionError> {
    let frozen_engine_stack = if matches!(
        &pending.payload,
        PendingExceptionPayload::EngineError { .. }
    ) {
        let snapshot = capture_error_stack(runtime, frames, &pending.origin)?;
        Some(render_error_stack(runtime, &snapshot)?)
    } else {
        None
    };
    loop {
        #[derive(Clone, Copy)]
        enum Handler {
            Catch { frame: usize, marker: usize },
            ForOf { frame: usize, marker: usize },
            Native(usize),
        }
        let mut handler = None;
        for (index, frame) in frames.iter().enumerate().rev() {
            if let Some(marker) = frame.stack.iter().rposition(|entry| {
                matches!(
                    entry,
                    OperandStackEntry::Catch { .. } | OperandStackEntry::ForOfCatch { .. }
                )
            }) {
                handler = Some(match frame.stack.get(marker) {
                    Some(OperandStackEntry::Catch { .. }) => Handler::Catch {
                        frame: index,
                        marker,
                    },
                    Some(OperandStackEntry::ForOfCatch { .. }) => Handler::ForOf {
                        frame: index,
                        marker,
                    },
                    Some(
                        OperandStackEntry::JavaScript(_) | OperandStackEntry::FinallyReturn { .. },
                    )
                    | None => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "exception handler search selected a non-handler marker",
                        }
                        .into());
                    }
                });
                break;
            }
            if frame.native_returns.iter().any(|continuation| {
                matches!(
                    continuation,
                    NativeContinuation::AggregateError(_)
                        | NativeContinuation::FromEntries(_)
                        | NativeContinuation::GroupBy(_)
                        | NativeContinuation::IteratorAppend(_)
                        | NativeContinuation::IteratorClose(_)
                )
            }) {
                handler = Some(Handler::Native(index));
                break;
            }
        }
        let Some(handler) = handler else {
            let caller_frames = exception_caller_frames(runtime, frames)?;
            let exception = finish_exception(runtime, pending, caller_frames)?;
            return Err(ExecutionError::Exception(exception));
        };

        if let Handler::ForOf {
            frame: handler_frame,
            marker,
        } = handler
        {
            let cleanup_temporary_receivers =
                frames_have_temporary_receiver(&frames[handler_frame..]);
            while frames.len() > handler_frame.saturating_add(1) {
                let mut frame = frames.pop().ok_or(EngineFault::RuntimeInvariant {
                    message: "exception unwinder lost a frame above its for-of handler",
                })?;
                *active_frame_values = active_frame_values.saturating_sub(frame.reserved_values);
                if let Some(dynamic) = frame.dynamic_return.take() {
                    runtime.retire_dynamic_root(dynamic.root)?;
                }
            }
            if cleanup_temporary_receivers && runtime.collection_pending {
                let pending_root = match &pending.payload {
                    PendingExceptionPayload::ThrownValue(value) => Some(value),
                    PendingExceptionPayload::EngineError { .. } => None,
                };
                let pending_roots = pending_root.map_or(&[][..], std::slice::from_ref);
                collect_cycles_with_execution_roots(runtime, frames, &[], pending_roots)?;
                for frame in frames.iter_mut() {
                    frame.transient_cleanup_pending = false;
                }
            }
            let frame = frames
                .get_mut(handler_frame)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "exception unwinder lost its for-of handler frame",
                })?;
            let (iterator, _next, active) = take_for_of_record_at(frame, marker)?;
            if !active || matches!(iterator, StoredValue::Undefined) {
                continue;
            }

            let active_frames = active_execution_frames(frames);
            frames
                .try_reserve(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 1,
                })?;
            let dispatch = begin_exceptional_iterator_close(
                runtime,
                iterator,
                pending,
                None,
                execution_budget,
            );
            let dispatch = match dispatch {
                Ok(dispatch) => resolve_native_dispatch(
                    runtime,
                    dispatch,
                    frames,
                    active_frames,
                    *active_frame_values,
                    compiler,
                    execution_budget,
                ),
                Err(error) => Err(error),
            };
            match dispatch {
                Ok(NativeDispatch::Frame(child)) => {
                    *active_frame_values =
                        active_frame_values.saturating_add(child.reserved_values);
                    frames.push(child);
                    return Ok(());
                }
                Ok(NativeDispatch::Call(_)) => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "exceptional IteratorClose resolver returned an unresolved call",
                    }
                    .into());
                }
                Ok(
                    NativeDispatch::Immediate(_)
                    | NativeDispatch::Pair(_, _)
                    | NativeDispatch::ForOfRecord { .. }
                    | NativeDispatch::ForOfStep { .. }
                    | NativeDispatch::ForOfClosed
                    | NativeDispatch::CopyDataPropertiesDone,
                ) => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "exceptional IteratorClose completed without rethrowing",
                    }
                    .into());
                }
                Err(NativeFailure::Abrupt(next)) => {
                    pending = next;
                    continue;
                }
                Err(NativeFailure::AbruptAfterTransient(next)) => {
                    if let Some(parent) = frames.last_mut() {
                        parent.transient_cleanup_pending = true;
                    }
                    pending = next;
                    continue;
                }
                Err(NativeFailure::Execution(error)) => return Err(error),
            }
        }

        let Handler::Catch {
            frame: handler_frame,
            marker: catch_marker,
        } = handler
        else {
            let Handler::Native(handler_frame) = handler else {
                unreachable!("exception handler classification is exhaustive")
            };
            let cleanup_temporary_receivers =
                frames_have_temporary_receiver(&frames[handler_frame..]);
            while frames.len() > handler_frame.saturating_add(1) {
                let mut frame = frames.pop().ok_or(EngineFault::RuntimeInvariant {
                    message: "exception unwinder lost a frame above its native handler",
                })?;
                *active_frame_values = active_frame_values.saturating_sub(frame.reserved_values);
                if let Some(dynamic) = frame.dynamic_return.take() {
                    runtime.retire_dynamic_root(dynamic.root)?;
                }
            }
            let mut frame = frames.pop().ok_or(EngineFault::RuntimeInvariant {
                message: "exception unwinder lost its native handler frame",
            })?;
            *active_frame_values = active_frame_values.saturating_sub(frame.reserved_values);
            if let Some(dynamic) = frame.dynamic_return.take() {
                runtime.retire_dynamic_root(dynamic.root)?;
            }
            let return_to = frame.return_to;
            let native_returns = std::mem::take(&mut frame.native_returns);
            let active_frames = active_execution_frames(frames);
            let dispatch = resume_iterator_abrupt_continuations(
                runtime,
                native_returns,
                pending,
                return_to,
                execution_budget,
            );
            let dispatch = match dispatch {
                Ok(dispatch) => resolve_native_dispatch(
                    runtime,
                    dispatch,
                    frames,
                    active_frames,
                    *active_frame_values,
                    compiler,
                    execution_budget,
                ),
                Err(error) => Err(error),
            };
            match dispatch {
                Ok(NativeDispatch::Immediate(value)) => {
                    let parent = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                        message: "iterator native handler completed without a parent frame",
                    })?;
                    push_call_result(
                        parent,
                        value,
                        return_to.ok_or(EngineFault::RuntimeInvariant {
                            message: "iterator native handler has no caller continuation",
                        })?,
                    )?;
                }
                Ok(NativeDispatch::Pair(original, updated)) => {
                    let parent = frames.last_mut().ok_or(EngineFault::RuntimeInvariant {
                        message: "iterator native handler produced a pair without a parent frame",
                    })?;
                    push_operator_pair(
                        parent,
                        original,
                        updated,
                        return_to.ok_or(EngineFault::RuntimeInvariant {
                            message: "iterator native handler has no caller continuation",
                        })?,
                    )?;
                }
                Ok(
                    NativeDispatch::ForOfRecord { .. }
                    | NativeDispatch::ForOfStep { .. }
                    | NativeDispatch::ForOfClosed
                    | NativeDispatch::CopyDataPropertiesDone,
                ) => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "iterator abrupt resolver produced a for-of normal result",
                    }
                    .into());
                }
                Ok(NativeDispatch::Frame(child)) => {
                    *active_frame_values =
                        active_frame_values.saturating_add(child.reserved_values);
                    frames.push(child);
                }
                Ok(NativeDispatch::Call(_)) => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "iterator abrupt resolver returned an unresolved call",
                    }
                    .into());
                }
                Err(NativeFailure::Abrupt(next)) => {
                    pending = next;
                    continue;
                }
                Err(NativeFailure::AbruptAfterTransient(next)) => {
                    if let Some(parent) = frames.last_mut() {
                        parent.transient_cleanup_pending = true;
                    }
                    pending = next;
                    continue;
                }
                Err(NativeFailure::Execution(error)) => return Err(error),
            }
            if cleanup_temporary_receivers && runtime.collection_pending {
                collect_cycles_with_execution_roots(runtime, frames, &[], &[])?;
            }
            return Ok(());
        };

        let cleanup_temporary_receivers = frames_have_temporary_receiver(&frames[handler_frame..]);
        let PendingException {
            realm,
            payload,
            origin: _,
        } = pending;
        let caught = match payload {
            PendingExceptionPayload::ThrownValue(value) => value,
            PendingExceptionPayload::EngineError { kind, message } => {
                let stack = frozen_engine_stack
                    .clone()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "caught engine error has no frozen stack snapshot",
                    })?;
                StoredValue::Object(runtime.materialize_error_object(
                    realm,
                    kind,
                    message,
                    Some(stack),
                )?)
            }
        };

        while frames.len() > handler_frame.saturating_add(1) {
            let mut frame = frames.pop().ok_or(EngineFault::RuntimeInvariant {
                message: "exception unwinder lost a frame above its catch handler",
            })?;
            *active_frame_values = active_frame_values.saturating_sub(frame.reserved_values);
            if let Some(dynamic) = frame.dynamic_return.take() {
                runtime.retire_dynamic_root(dynamic.root)?;
            }
        }

        let frame = frames
            .get_mut(handler_frame)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "exception unwinder lost its catch handler frame",
            })?;
        let handler = match frame.stack.get(catch_marker) {
            Some(OperandStackEntry::Catch { handler }) => *handler,
            Some(
                OperandStackEntry::JavaScript(_)
                | OperandStackEntry::ForOfCatch { .. }
                | OperandStackEntry::FinallyReturn { .. },
            )
            | None => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "exception unwinder selected a non-catch operand entry",
                }
                .into());
            }
        };
        frame.stack.truncate(catch_marker);
        push(frame, caught);
        frame.instruction = handler;

        if cleanup_temporary_receivers && runtime.collection_pending {
            collect_cycles_with_execution_roots(runtime, frames, &[], &[])?;
            for frame in frames {
                frame.transient_cleanup_pending = false;
            }
        }

        return Ok(());
    }
}

pub(super) fn finish_exception(
    runtime: &mut Runtime,
    pending: PendingException,
    caller_frames: Vec<JsStackFrame>,
) -> Result<JsException, ExecutionError> {
    let PendingException {
        realm: _,
        payload,
        origin,
    } = pending;
    Ok(match payload {
        PendingExceptionPayload::EngineError { kind, message } => {
            JsException::engine_error(kind, message, origin, caller_frames)
        }
        PendingExceptionPayload::ThrownValue(value) => {
            JsException::explicit_throw(runtime.public_value(value)?, origin, caller_frames)
        }
    })
}
