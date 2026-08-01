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

//! Frame planning, construction, and verified-bytecode opcode execution.

use super::instanceof::begin_instance_of;

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[allow(
    clippy::too_many_lines,
    reason = "verified environment validation and cumulative frame-budget planning remain one read-only transaction"
)]
pub(super) fn plan_frame(
    runtime: &Runtime,
    function_id: FunctionId,
    active_frames: usize,
    active_frame_values: u64,
) -> Result<FramePlan, ExecutionError> {
    let observed_frames = usize_to_u64(active_frames).saturating_add(1);
    check_execution_limit(
        RuntimeResource::Frames,
        u64::from(runtime.limits.max_active_frames),
        observed_frames,
    )?;

    let function = runtime
        .functions
        .get(function_id)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "function",
            index: function_id.index(),
            generation: function_id.generation(),
        })?;
    let bytecode = function.bytecode()?;
    let code_id = bytecode.code;
    let template_id = bytecode.template;

    let code = runtime
        .code
        .get(code_id)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "installed code",
            index: code_id.index(),
            generation: code_id.generation(),
        })?;
    if !runtime.contains_realm(code.realm) {
        return Err(EngineFault::StaleHeapEdge {
            edge: "realm",
            index: code.realm.index(),
            generation: code.realm.generation(),
        }
        .into());
    }
    let verified =
        code.authority
            .function(template_id)
            .ok_or(EngineFault::InvalidClosureEnvironment {
                function: template_id,
            })?;
    if bytecode.environment.len() != verified.metadata().closures().len() {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: template_id,
        }
        .into());
    }
    for (binding, definition) in bytecode
        .environment
        .iter()
        .copied()
        .zip(verified.metadata().closures())
    {
        match (binding, definition.binding()) {
            (EnvironmentBinding::Captured(cell), CompilerClosureBinding::Captured(_)) => {
                if !runtime.cells.contains(cell) {
                    return Err(EngineFault::StaleHeapEdge {
                        edge: "closure cell",
                        index: cell.index(),
                        generation: cell.generation(),
                    }
                    .into());
                }
            }
            (EnvironmentBinding::RealmGlobal(global), CompilerClosureBinding::RealmGlobal(_)) => {
                let valid = runtime
                    .global_bindings
                    .get(global)
                    .is_some_and(|binding| binding.realm == code.realm);
                if !valid {
                    return Err(EngineFault::StaleHeapEdge {
                        edge: "realm global binding",
                        index: global.index(),
                        generation: global.generation(),
                    }
                    .into());
                }
            }
            (
                EnvironmentBinding::Captured(_) | EnvironmentBinding::RealmGlobal(_),
                CompilerClosureBinding::Captured(_) | CompilerClosureBinding::RealmGlobal(_),
            ) => {
                return Err(EngineFault::InvalidClosureEnvironment {
                    function: template_id,
                }
                .into());
            }
        }
    }

    let control_flow = verified.function().control_flow();
    let domains = control_flow.domains();
    let argument_count = domains.argument_count() as usize;
    let local_count = domains.local_count() as usize;
    let stack_capacity = control_flow.computed_stack_size() as usize;
    let frame_values = argument_count
        .checked_add(local_count)
        .and_then(|value| value.checked_add(stack_capacity))
        .and_then(|value| value.checked_add(1))
        .map_or(u64::MAX, usize_to_u64);
    let observed_frame_values = active_frame_values.saturating_add(frame_values);
    check_execution_limit(
        RuntimeResource::FrameValues,
        runtime.limits.max_active_frame_values,
        observed_frame_values,
    )?;

    let installed_index =
        usize::try_from(template_id.get()).map_err(|_| EngineFault::InvalidClosureEnvironment {
            function: template_id,
        })?;
    if code.templates.get(installed_index).is_none() {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: template_id,
        }
        .into());
    }
    let instruction = control_flow.instruction_index_at(BytecodePc::ZERO).ok_or(
        EngineFault::MissingInstruction {
            function: template_id,
            instruction: 0,
        },
    )?;
    let (strict, receiver_access) = receiver_profile(&verified);
    Ok(FramePlan {
        function: function_id,
        code: code_id,
        template: template_id,
        argument_count,
        local_count,
        stack_capacity,
        reserved_values: frame_values,
        strict,
        receiver_access,
        instruction,
    })
}

/// Implements the pinned `ToObject` conversion: objects and functions pass
/// through, primitives are boxed into their realm wrapper, and `null` or
/// `undefined` produce the exact `cannot convert to object` `TypeError`.
/// Returns the converted object or a pending exception.
fn to_object_value(
    runtime: &mut Runtime,
    realm: RealmId,
    value: StoredValue,
    origin: JsStackFrame,
) -> Result<Result<StoredValue, PendingException>, ExecutionError> {
    match value {
        StoredValue::Function(_) | StoredValue::Object(_) => Ok(Ok(value)),
        StoredValue::Boolean(value) => {
            let object = runtime.allocate_boxed_boolean(realm, value)?;
            Ok(Ok(StoredValue::Object(object)))
        }
        StoredValue::Number(value) => {
            let object = runtime.allocate_boxed_number(realm, value)?;
            Ok(Ok(StoredValue::Object(object)))
        }
        StoredValue::String(value) => {
            let object = runtime.allocate_boxed_string(realm, value)?;
            Ok(Ok(StoredValue::Object(object)))
        }
        StoredValue::Symbol(value) => {
            let object = runtime.allocate_boxed_symbol(realm, value)?;
            Ok(Ok(StoredValue::Object(object)))
        }
        StoredValue::Undefined | StoredValue::Null => Ok(Err(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("cannot convert to object")?,
            },
            origin,
        })),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "failure-atomic frame allocation and initialization remain one transaction"
)]
pub(super) fn create_frame(
    runtime: &mut Runtime,
    plan: FramePlan,
    receiver: StoredValue,
    supplied: FrameArguments<'_>,
    return_to: Option<CallReturn>,
    dynamic_return: Option<DynamicFunctionReturn>,
) -> Result<Frame, ExecutionError> {
    let function = runtime
        .functions
        .get(plan.function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "function",
            index: plan.function.index(),
            generation: plan.function.generation(),
        })?;
    let environment = copy_environment(
        &function.bytecode()?.environment,
        RuntimeResource::FrameValues,
    )?;
    let code = runtime
        .code
        .get(plan.code)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "installed code",
            index: plan.code.index(),
            generation: plan.code.generation(),
        })?;

    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(plan.argument_count)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: plan.argument_count,
        })?;
    match supplied {
        FrameArguments::Public(supplied) => {
            for index in 0..plan.argument_count {
                let value = supplied
                    .get(index)
                    .map(JsValue::stored)
                    .transpose()?
                    .map_or(StoredValue::Undefined, StoredValue::duplicate);
                arguments.push(FrameBinding::Direct(SlotValue::Value(value)));
            }
        }
        FrameArguments::Owned(supplied) => {
            let mut supplied = supplied.into_remaining_iter();
            for _ in 0..plan.argument_count {
                let value = supplied.next().unwrap_or(StoredValue::Undefined);
                arguments.push(FrameBinding::Direct(SlotValue::Value(value)));
            }
        }
    }

    let mut locals = Vec::new();
    locals
        .try_reserve_exact(plan.local_count)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: plan.local_count,
        })?;
    for _ in 0..plan.local_count {
        locals.push(FrameBinding::Direct(SlotValue::Value(
            StoredValue::Undefined,
        )));
    }

    let verified =
        code.authority
            .function(plan.template)
            .ok_or(EngineFault::InvalidClosureEnvironment {
                function: plan.template,
            })?;
    let variable_count = plan.argument_count.checked_add(plan.local_count).ok_or(
        EngineFault::InvalidClosureEnvironment {
            function: plan.template,
        },
    )?;
    if verified.metadata().variables().len() != variable_count {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: plan.template,
        }
        .into());
    }
    for (local, definition) in verified
        .metadata()
        .variables()
        .iter()
        .skip(plan.argument_count)
        .enumerate()
    {
        if definition.policy().kind() == CompilerBindingKind::FunctionName {
            let binding = locals
                .get_mut(local)
                .ok_or(EngineFault::InvalidClosureEnvironment {
                    function: plan.template,
                })?;
            *binding = FrameBinding::Direct(SlotValue::Value(StoredValue::Function(plan.function)));
        }
    }

    let installed_index = usize::try_from(plan.template.get()).map_err(|_| {
        EngineFault::InvalidClosureEnvironment {
            function: plan.template,
        }
    })?;
    let installed =
        code.templates
            .get(installed_index)
            .ok_or(EngineFault::InvalidClosureEnvironment {
                function: plan.template,
            })?;
    let own_cell_bindings = copy_addresses(&installed.own_cell_bindings)?;
    let mut own_cells = Vec::new();
    own_cells
        .try_reserve_exact(own_cell_bindings.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: own_cell_bindings.len(),
        })?;
    own_cells.resize(own_cell_bindings.len(), None);

    let mut stack = Vec::new();
    stack
        .try_reserve_exact(plan.stack_capacity)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: plan.stack_capacity,
        })?;

    let receiver = normalize_receiver(runtime, code.realm, plan.receiver_access, receiver)?;

    Ok(Frame {
        function: plan.function,
        code: plan.code,
        template: plan.template,
        strict: plan.strict,
        receiver,
        instruction: plan.instruction,
        return_to,
        dynamic_return,
        native_returns: Vec::new(),
        transient_cleanup_pending: false,
        ordinary_constructor: false,
        native_caller: None,
        reserved_values: plan.reserved_values,
        arguments,
        locals,
        own_cells,
        own_cell_bindings,
        environment,
        stack,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive admitted-opcode dispatcher is intentionally centralized"
)]
pub(super) fn execute_one(
    runtime: &mut Runtime,
    frame: &mut Frame,
    execution_budget: &mut ExecutionBudget,
) -> Result<Step, ExecutionError> {
    let (verified_instruction, source_pc) = {
        let code = code(runtime, frame.code)?;
        let function = code.authority.function(frame.template).ok_or(
            EngineFault::InvalidClosureEnvironment {
                function: frame.template,
            },
        )?;
        let instruction = function
            .function()
            .control_flow()
            .instruction(frame.instruction)
            .copied()
            .ok_or(EngineFault::MissingInstruction {
                function: frame.template,
                instruction: frame.instruction.get(),
            })?;
        (instruction, instruction.decoded().pc())
    };

    let expected_depth =
        verified_instruction
            .entry_stack_depth()
            .ok_or(EngineFault::UnreachableInstruction {
                function: frame.template,
                pc: source_pc,
            })?;
    if frame.stack.len() != expected_depth as usize {
        return Err(EngineFault::StackDepthMismatch {
            function: frame.template,
            pc: source_pc,
            expected: expected_depth,
            actual: frame.stack.len(),
        }
        .into());
    }

    let instruction = verified_instruction.decoded().instruction();
    let opcode = instruction.opcode();
    let operands = instruction.operands();

    match opcode {
        FinalOpcode::PushI32 => {
            let Operands::I32(value) = operands else {
                return unsupported_dispatch(opcode);
            };
            push(frame, StoredValue::Number(value.into()));
        }
        FinalOpcode::PushMinus1
        | FinalOpcode::Push0
        | FinalOpcode::Push1
        | FinalOpcode::Push2
        | FinalOpcode::Push3
        | FinalOpcode::Push4
        | FinalOpcode::Push5
        | FinalOpcode::Push6
        | FinalOpcode::Push7 => {
            let value =
                implied_integer(opcode).ok_or(EngineFault::UnsupportedDispatch { opcode })?;
            push(frame, StoredValue::Number(value.into()));
        }
        FinalOpcode::PushI8 => {
            let Operands::I8(value) = operands else {
                return unsupported_dispatch(opcode);
            };
            push(frame, StoredValue::Number(i32::from(value).into()));
        }
        FinalOpcode::PushI16 => {
            let Operands::I16(value) = operands else {
                return unsupported_dispatch(opcode);
            };
            push(frame, StoredValue::Number(i32::from(value).into()));
        }
        FinalOpcode::PushConst | FinalOpcode::PushConst8 => {
            let index = constant_index(operands).ok_or(EngineFault::MissingPoolEntry {
                pool: "constant",
                index: u32::MAX,
            })?;
            push(
                frame,
                materialize_constant(runtime, frame.code, frame.template, index)?,
            );
        }
        FinalOpcode::PushAtomValue => {
            let Operands::Atom(index) = operands else {
                return unsupported_dispatch(opcode);
            };
            let string = {
                let installed = installed_template(runtime, frame.code, frame.template)?;
                installed
                    .atoms
                    .get(index.get() as usize)
                    .and_then(AtomDescription::description)
                    .cloned()
                    .ok_or(EngineFault::MissingPoolEntry {
                        pool: "atom",
                        index: index.get(),
                    })?
            };
            push(frame, StoredValue::String(string));
        }
        FinalOpcode::PushEmptyString => {
            push(frame, StoredValue::String(JsString::empty()));
        }
        FinalOpcode::Undefined => push(frame, StoredValue::Undefined),
        FinalOpcode::Null => push(frame, StoredValue::Null),
        FinalOpcode::PushThis => {
            push(frame, frame.receiver.duplicate());
        }
        FinalOpcode::PushFalse => push(frame, StoredValue::Boolean(false)),
        FinalOpcode::PushTrue => push(frame, StoredValue::Boolean(true)),
        FinalOpcode::ToObject => {
            let value = pop(frame)?;
            let realm = code(runtime, frame.code)?.realm;
            let origin = instruction_location(runtime, frame, source_pc)?;
            match to_object_value(runtime, realm, value, origin)? {
                Ok(object) => push(frame, object),
                Err(pending) => return Ok(Step::Abrupt(pending)),
            }
        }
        FinalOpcode::Object => {
            let realm = code(runtime, frame.code)?.realm;
            let prototype = runtime.realm_object_prototype(realm)?;
            let object = runtime.allocate_ordinary_object(prototype)?;
            push(frame, StoredValue::Object(object));
        }
        FinalOpcode::ArrayFrom => {
            let Operands::NPop { argument_count } = operands else {
                return unsupported_dispatch(opcode);
            };
            let element_count = usize::from(argument_count);
            if frame.stack.len() < element_count {
                return Err(EngineFault::StackDepthMismatch {
                    function: frame.template,
                    pc: source_pc,
                    expected: u32::from(argument_count),
                    actual: frame.stack.len(),
                }
                .into());
            }
            let first = frame.stack.len() - element_count;
            for index in first..frame.stack.len() {
                stack_value_at(frame, index)?;
            }
            execution_budget.charge_instructions(usize_to_u64(element_count))?;

            let mut elements = Vec::new();
            elements.try_reserve_exact(element_count).map_err(|_| {
                ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: element_count,
                }
            })?;
            for index in first..frame.stack.len() {
                elements.push(stack_value_at(frame, index)?.duplicate());
            }
            let realm = code(runtime, frame.code)?.realm;
            let array = runtime.allocate_array(realm, elements)?;
            frame.stack.truncate(first);
            push(frame, StoredValue::Object(array));
        }
        FinalOpcode::Append => {
            let iterable = pop(frame)?;
            let position = pop(frame)?;
            let array = pop(frame)?;
            let StoredValue::Object(array) = array else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "verified append destination is not an object",
                }
                .into());
            };
            if !runtime.is_array_object(array)? {
                return Err(EngineFault::RuntimeInvariant {
                    message: "verified append destination is not an Array",
                }
                .into());
            }
            let StoredValue::Number(position) = position else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "verified append cursor is not a number",
                }
                .into());
            };
            let position_number = position.as_f64();
            if !position_number.is_finite()
                || position_number < 0.0
                || position_number.fract() != 0.0
                || position_number > f64::from(u32::MAX)
            {
                return Err(EngineFault::RuntimeInvariant {
                    message: "verified append cursor is outside the u32 domain",
                }
                .into());
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the verified append cursor was checked against the exact u32 integer domain"
            )]
            let position = position_number as u32;
            let realm = code(runtime, frame.code)?.realm;
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_iterator_append(
                    runtime,
                    array,
                    position,
                    iterable,
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::Catch => {
            let handler = branch_successor(verified_instruction, true, frame)?;
            frame.stack.push(OperandStackEntry::Catch { handler });
        }
        FinalOpcode::Gosub => {
            enter_finally_subroutine(verified_instruction, frame)?;
            return Ok(Step::Continue);
        }
        FinalOpcode::Ret => {
            frame.instruction = pop_finally_continuation(frame)?;
            return Ok(Step::Continue);
        }
        FinalOpcode::Drop => {
            drop_stack_entry(frame)?;
        }
        FinalOpcode::Nip => {
            let top = pop(frame)?;
            drop_stack_entry(frame)?;
            push(frame, top);
        }
        FinalOpcode::NipCatch => {
            nip_catch(frame, execution_budget)?;
        }
        FinalOpcode::Dup => {
            let value = peek(frame)?.duplicate();
            push(frame, value);
        }
        FinalOpcode::Dup1 => {
            let top = pop(frame)?;
            let index =
                frame
                    .stack
                    .len()
                    .checked_sub(1)
                    .ok_or(EngineFault::StackDepthMismatch {
                        function: frame.template,
                        pc: source_pc,
                        expected: 2,
                        actual: frame.stack.len().saturating_add(1),
                    })?;
            let value = stack_value_at(frame, index)?.duplicate();
            push(frame, value);
            push(frame, top);
        }
        FinalOpcode::Insert2 => {
            let right = pop(frame)?;
            let left = pop(frame)?;
            push(frame, right.duplicate());
            push(frame, left);
            push(frame, right);
        }
        FinalOpcode::Insert3 => {
            let third = pop(frame)?;
            let second = pop(frame)?;
            let first = pop(frame)?;
            push(frame, third.duplicate());
            push(frame, first);
            push(frame, second);
            push(frame, third);
        }
        FinalOpcode::Swap => {
            let right = pop(frame)?;
            let left = pop(frame)?;
            push(frame, right);
            push(frame, left);
        }
        FinalOpcode::Perm3 => {
            let third = pop(frame)?;
            let second = pop(frame)?;
            let first = pop(frame)?;
            push(frame, second);
            push(frame, first);
            push(frame, third);
        }
        FinalOpcode::Rot3l => {
            let third = pop(frame)?;
            let second = pop(frame)?;
            let first = pop(frame)?;
            push(frame, second);
            push(frame, third);
            push(frame, first);
        }
        FinalOpcode::Rot3r => {
            let third = pop(frame)?;
            let second = pop(frame)?;
            let first = pop(frame)?;
            push(frame, third);
            push(frame, first);
            push(frame, second);
        }
        FinalOpcode::Call
        | FinalOpcode::Call0
        | FinalOpcode::Call1
        | FinalOpcode::Call2
        | FinalOpcode::Call3 => {
            let argument_count = direct_call_argument_count(opcode, operands)?;
            let required = argument_count.saturating_add(1);
            if frame.stack.len() < required {
                return Err(EngineFault::StackDepthMismatch {
                    function: frame.template,
                    pc: source_pc,
                    expected: u32::try_from(required).unwrap_or(u32::MAX),
                    actual: frame.stack.len(),
                }
                .into());
            }
            let callee_index = frame.stack.len() - required;
            let StoredValue::Function(function) = stack_value_at(frame, callee_index)? else {
                return Ok(Step::Abrupt(not_callable_exception(
                    runtime, frame, source_pc,
                )?));
            };
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            return Ok(Step::Call {
                function: *function,
                inputs: CallInputSource::Frame {
                    argument_count,
                    kind: CallKind::Direct,
                },
                return_to,
                source_pc,
            });
        }
        FinalOpcode::CallMethod => {
            let Operands::NPop { argument_count } = operands else {
                return unsupported_dispatch(opcode);
            };
            let argument_count = usize::from(argument_count);
            let required = argument_count.saturating_add(2);
            if frame.stack.len() < required {
                return Err(EngineFault::StackDepthMismatch {
                    function: frame.template,
                    pc: source_pc,
                    expected: u32::try_from(required).unwrap_or(u32::MAX),
                    actual: frame.stack.len(),
                }
                .into());
            }
            let callee_index = frame.stack.len() - argument_count - 1;
            let StoredValue::Function(function) = stack_value_at(frame, callee_index)? else {
                return Ok(Step::Abrupt(not_callable_exception(
                    runtime, frame, source_pc,
                )?));
            };
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            return Ok(Step::Call {
                function: *function,
                inputs: CallInputSource::Frame {
                    argument_count,
                    kind: CallKind::Method,
                },
                return_to,
                source_pc,
            });
        }
        FinalOpcode::CallConstructor => {
            let Operands::NPop { argument_count } = operands else {
                return unsupported_dispatch(opcode);
            };
            let argument_count = usize::from(argument_count);
            let required = argument_count.saturating_add(2);
            if frame.stack.len() < required {
                return Err(EngineFault::StackDepthMismatch {
                    function: frame.template,
                    pc: source_pc,
                    expected: u32::try_from(required).unwrap_or(u32::MAX),
                    actual: frame.stack.len(),
                }
                .into());
            }
            let callee_index = frame.stack.len() - required;
            let new_target_index = callee_index + 1;
            let (StoredValue::Function(function), StoredValue::Function(_new_target)) = (
                stack_value_at(frame, callee_index)?,
                stack_value_at(frame, new_target_index)?,
            ) else {
                return Ok(Step::Abrupt(not_constructor_exception(
                    runtime, frame, source_pc,
                )?));
            };
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            return Ok(Step::Call {
                function: *function,
                inputs: CallInputSource::Frame {
                    argument_count,
                    kind: CallKind::Constructor,
                },
                return_to,
                source_pc,
            });
        }
        FinalOpcode::Apply => {
            let Operands::U16(magic) = operands else {
                return unsupported_dispatch(opcode);
            };
            let required = 3_u32;
            if frame.stack.len() < usize::try_from(required).unwrap_or(usize::MAX) {
                return Err(EngineFault::StackDepthMismatch {
                    function: frame.template,
                    pc: source_pc,
                    expected: required,
                    actual: frame.stack.len(),
                }
                .into());
            }
            let callee_index = frame.stack.len() - 3;
            let StoredValue::Function(function) = *stack_value_at(frame, callee_index)? else {
                return Ok(Step::Abrupt(not_callable_exception(
                    runtime, frame, source_pc,
                )?));
            };
            let receiver = stack_value_at(frame, callee_index + 1)?.duplicate();
            let array_like = stack_value_at(frame, callee_index + 2)?.duplicate();
            frame.stack.truncate(callee_index);
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            return Ok(Step::Apply {
                function,
                receiver,
                array_like,
                magic,
                return_to,
                source_pc,
            });
        }
        FinalOpcode::FClosure | FinalOpcode::FClosure8 => {
            let index = constant_index(operands).ok_or(EngineFault::MissingPoolEntry {
                pool: "function constant",
                index: u32::MAX,
            })?;
            let child = function_constant(runtime, frame.code, frame.template, index)?;
            let function = create_closure(runtime, frame, child)?;
            push(frame, StoredValue::Function(function));
        }
        FinalOpcode::GetArrayEl | FinalOpcode::GetArrayEl2 => {
            let realm = code(runtime, frame.code)?.realm;
            let key = pop(frame)?;
            let base = if opcode == FinalOpcode::GetArrayEl {
                pop(frame)?
            } else {
                peek(frame)?.duplicate()
            };
            let origin = instruction_location(runtime, frame, source_pc)?;
            let nullish_failure = match base {
                StoredValue::Null => Some(PropertyFailure::ReadNull),
                StoredValue::Undefined => Some(PropertyFailure::ReadUndefined),
                StoredValue::Boolean(_)
                | StoredValue::Number(_)
                | StoredValue::String(_)
                | StoredValue::Symbol(_)
                | StoredValue::Function(_)
                | StoredValue::Object(_) => None,
            };
            if let Some(failure) = nullish_failure {
                return Ok(Step::Abrupt(property_exception_at(
                    realm, origin, None, failure,
                )?));
            }
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            return native_step(
                begin_property_key_conversion(
                    runtime,
                    key,
                    PropertyKeyTarget::Read { base, realm },
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::PutArrayEl => {
            let realm = code(runtime, frame.code)?.realm;
            let value = pop(frame)?;
            let key = pop(frame)?;
            let base = pop(frame)?;
            let return_to =
                CallReturn::discard(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_property_key_conversion(
                    runtime,
                    key,
                    PropertyKeyTarget::Write {
                        base,
                        value,
                        strict: frame.strict,
                        realm,
                    },
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::ToPropKey => {
            let realm = code(runtime, frame.code)?.realm;
            let value = pop(frame)?;
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_property_key_conversion(
                    runtime,
                    value,
                    PropertyKeyTarget::ToKey,
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::DefineArrayEl => {
            let realm = code(runtime, frame.code)?.realm;
            let value = pop(frame)?;
            let key_value = pop(frame)?;
            let base = peek(frame)?.duplicate();
            let property = match &key_value {
                StoredValue::Number(number) => {
                    let raw = number.as_f64();
                    if !raw.is_finite()
                        || raw < 0.0
                        || raw.fract() != 0.0
                        || raw >= f64::from(u32::MAX)
                    {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "verified Array literal cursor is not an array index",
                        }
                        .into());
                    }
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "the verified Array literal cursor was checked against the exact array-index domain"
                    )]
                    let raw = raw as u32;
                    let index = ArrayIndex::new(raw).ok_or(EngineFault::RuntimeInvariant {
                        message: "verified Array literal cursor is not an array index",
                    })?;
                    StaticPropertyOperand {
                        key: PropertyKey::from_index(index),
                        name: number.to_javascript_string()?,
                    }
                }
                StoredValue::String(_) | StoredValue::Symbol(_) => {
                    computed_property_operand(runtime, &key_value)?
                }
                StoredValue::Undefined
                | StoredValue::Null
                | StoredValue::Boolean(_)
                | StoredValue::Function(_)
                | StoredValue::Object(_) => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "computed Array property operand was not a verified key",
                    }
                    .into());
                }
            };
            if let PropertyWriteOutcome::Failed(failure) =
                define_static_property(runtime, &base, property.key, value, execution_budget)?
            {
                return Ok(Step::Abrupt(property_exception_at(
                    realm,
                    instruction_location(runtime, frame, source_pc)?,
                    Some(&property.name),
                    failure,
                )?));
            }
            push(frame, key_value);
        }
        FinalOpcode::DefineMethodComputed => {
            let realm = code(runtime, frame.code)?.realm;
            let method = define_method_computed_operand(operands)?;
            let function = pop(frame)?;
            let key = pop(frame)?;
            let base = peek(frame)?.duplicate();
            let return_to =
                CallReturn::discard(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_property_key_conversion(
                    runtime,
                    key,
                    PropertyKeyTarget::DefineMethod {
                        base,
                        function,
                        kind: method.kind,
                        enumerable: method.enumerable,
                        realm,
                    },
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::GetField | FinalOpcode::GetField2 => {
            let realm = code(runtime, frame.code)?.realm;
            let property = static_property_operand(runtime, frame, operands)?;
            let base = if opcode == FinalOpcode::GetField {
                pop(frame)?
            } else {
                peek(frame)?.duplicate()
            };
            match read_static_property(runtime, realm, &base, &property.key)? {
                PropertyReadOutcome::Value(value) => push(frame, value),
                PropertyReadOutcome::Getter { function, receiver } => {
                    let return_to =
                        CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                            EngineFault::InvalidSuccessor {
                                function: frame.template,
                                pc: source_pc,
                            },
                        )?);
                    return Ok(Step::Call {
                        function,
                        inputs: CallInputSource::Prepared(CallInputs {
                            receiver,
                            arguments: CallArguments::empty(),
                            new_target: None,
                        }),
                        return_to,
                        source_pc,
                    });
                }
                PropertyReadOutcome::Failed(failure) => {
                    return Ok(Step::Abrupt(property_exception(
                        runtime,
                        frame,
                        source_pc,
                        &property.name,
                        failure,
                    )?));
                }
            }
        }
        FinalOpcode::PutField => {
            let realm = code(runtime, frame.code)?.realm;
            let property = static_property_operand(runtime, frame, operands)?;
            let value = pop(frame)?;
            let base = pop(frame)?;
            if is_array_length_target(runtime, &base, &property.key)? {
                let return_to =
                    CallReturn::discard(verified_instruction.successors().fallthrough().ok_or(
                        EngineFault::InvalidSuccessor {
                            function: frame.template,
                            pc: source_pc,
                        },
                    )?);
                let origin = instruction_location(runtime, frame, source_pc)?;
                let target = array_length_write_target(base, property.name, frame.strict, &value);
                return native_step(
                    begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::Number,
                        target,
                        realm,
                        Some(return_to),
                        origin,
                        execution_budget,
                    ),
                    return_to,
                );
            }
            match write_static_property(
                runtime,
                realm,
                &base,
                property.key,
                value,
                frame.strict,
                execution_budget,
            )? {
                PropertyWriteOutcome::Complete => {}
                PropertyWriteOutcome::Setter {
                    function,
                    receiver,
                    value,
                } => {
                    let mut arguments = Vec::new();
                    arguments.try_reserve_exact(1).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::FrameValues,
                            additional: 1,
                        }
                    })?;
                    arguments.push(value);
                    let return_to = CallReturn::discard(
                        verified_instruction.successors().fallthrough().ok_or(
                            EngineFault::InvalidSuccessor {
                                function: frame.template,
                                pc: source_pc,
                            },
                        )?,
                    );
                    return Ok(Step::Call {
                        function,
                        inputs: CallInputSource::Prepared(CallInputs {
                            receiver,
                            arguments: CallArguments::from_values(arguments),
                            new_target: None,
                        }),
                        return_to,
                        source_pc,
                    });
                }
                PropertyWriteOutcome::Failed(failure) => {
                    return Ok(Step::Abrupt(property_exception(
                        runtime,
                        frame,
                        source_pc,
                        &property.name,
                        failure,
                    )?));
                }
            }
        }
        FinalOpcode::DefineField => {
            let property = static_property_operand(runtime, frame, operands)?;
            let value = pop(frame)?;
            let base = peek(frame)?.duplicate();
            if let PropertyWriteOutcome::Failed(failure) =
                define_static_property(runtime, &base, property.key, value, execution_budget)?
            {
                return Ok(Step::Abrupt(property_exception(
                    runtime,
                    frame,
                    source_pc,
                    &property.name,
                    failure,
                )?));
            }
        }
        FinalOpcode::DefineMethod => {
            let method = define_method_operand(runtime, frame, operands)?;
            let value = pop(frame)?;
            let StoredValue::Function(function) = value else {
                return Ok(Step::Abrupt(not_callable_exception(
                    runtime, frame, source_pc,
                )?));
            };
            let base = peek(frame)?.duplicate();
            match define_static_method(
                runtime,
                &base,
                method.property.key,
                &method.property.name,
                function,
                method.kind,
                method.enumerable,
            )? {
                PropertyDefinitionOutcome::Complete => {}
                PropertyDefinitionOutcome::Failed(failure) => {
                    return Ok(Step::Abrupt(property_exception(
                        runtime,
                        frame,
                        source_pc,
                        &method.property.name,
                        failure,
                    )?));
                }
            }
        }
        FinalOpcode::GetArg
        | FinalOpcode::GetArg0
        | FinalOpcode::GetArg1
        | FinalOpcode::GetArg2
        | FinalOpcode::GetArg3 => {
            let index = argument_index(opcode, operands)?;
            let value = duplicate_binding(runtime, frame_argument(frame, index)?, false, frame)?;
            push(frame, value);
        }
        FinalOpcode::PutArg
        | FinalOpcode::PutArg0
        | FinalOpcode::PutArg1
        | FinalOpcode::PutArg2
        | FinalOpcode::PutArg3 => {
            let index = argument_index(opcode, operands)?;
            let value = pop(frame)?;
            write_argument(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::SetArg
        | FinalOpcode::SetArg0
        | FinalOpcode::SetArg1
        | FinalOpcode::SetArg2
        | FinalOpcode::SetArg3 => {
            let index = argument_index(opcode, operands)?;
            let value = peek(frame)?.duplicate();
            write_argument(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::GetVarUndef | FinalOpcode::GetVar => {
            let index = closure_index(opcode, operands)?;
            let global = global_reference_operand(runtime, frame, index)?;
            match read_realm_global(runtime, &global)? {
                RealmGlobalReadOutcome::Value(value) => push(frame, value),
                RealmGlobalReadOutcome::Missing if opcode == FinalOpcode::GetVarUndef => {
                    push(frame, StoredValue::Undefined);
                }
                RealmGlobalReadOutcome::Missing => {
                    return Ok(Step::Abrupt(global_not_defined_exception(
                        runtime,
                        frame,
                        &global.name,
                        source_pc,
                    )?));
                }
            }
        }
        FinalOpcode::PutVar => {
            let index = closure_index(opcode, operands)?;
            let global = global_reference_operand(runtime, frame, index)?;
            let name = global.name.clone();
            let value = pop(frame)?;
            match write_realm_global(runtime, global, value, frame.strict, execution_budget)? {
                RealmGlobalWriteOutcome::Complete => {}
                RealmGlobalWriteOutcome::Missing => {
                    return Ok(Step::Abrupt(global_not_defined_exception(
                        runtime, frame, &name, source_pc,
                    )?));
                }
                RealmGlobalWriteOutcome::Property(failure) => {
                    return Ok(Step::Abrupt(property_exception(
                        runtime, frame, source_pc, &name, failure,
                    )?));
                }
            }
        }
        FinalOpcode::GetLoc
        | FinalOpcode::GetLoc8
        | FinalOpcode::GetLoc0
        | FinalOpcode::GetLoc1
        | FinalOpcode::GetLoc2
        | FinalOpcode::GetLoc3 => {
            let index = local_index(opcode, operands)?;
            let value = duplicate_binding(runtime, frame_local(frame, index)?, false, frame)?;
            push(frame, value);
        }
        FinalOpcode::PutLoc
        | FinalOpcode::PutLoc8
        | FinalOpcode::PutLoc0
        | FinalOpcode::PutLoc1
        | FinalOpcode::PutLoc2
        | FinalOpcode::PutLoc3 => {
            let index = local_index(opcode, operands)?;
            let value = pop(frame)?;
            write_local(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::SetLoc
        | FinalOpcode::SetLoc8
        | FinalOpcode::SetLoc0
        | FinalOpcode::SetLoc1
        | FinalOpcode::SetLoc2
        | FinalOpcode::SetLoc3 => {
            let index = local_index(opcode, operands)?;
            let value = peek(frame)?.duplicate();
            write_local(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::GetVarRef
        | FinalOpcode::GetVarRef0
        | FinalOpcode::GetVarRef1
        | FinalOpcode::GetVarRef2
        | FinalOpcode::GetVarRef3 => {
            let index = closure_index(opcode, operands)?;
            let value = duplicate_environment(runtime, frame, index, false)?;
            push(frame, value);
        }
        FinalOpcode::PutVarRef
        | FinalOpcode::PutVarRef0
        | FinalOpcode::PutVarRef1
        | FinalOpcode::PutVarRef2
        | FinalOpcode::PutVarRef3 => {
            let index = closure_index(opcode, operands)?;
            let value = pop(frame)?;
            write_environment(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::SetVarRef
        | FinalOpcode::SetVarRef0
        | FinalOpcode::SetVarRef1
        | FinalOpcode::SetVarRef2
        | FinalOpcode::SetVarRef3 => {
            let index = closure_index(opcode, operands)?;
            let value = peek(frame)?.duplicate();
            write_environment(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::SetLocUninitialized => {
            let index = local_index(opcode, operands)?;
            write_local(runtime, frame, index, SlotValue::Uninitialized)?;
        }
        FinalOpcode::GetLocCheck => {
            let index = local_index(opcode, operands)?;
            let value = match duplicate_binding(runtime, frame_local(frame, index)?, true, frame) {
                Ok(value) => value,
                Err(BindingAccessError::Uninitialized) => {
                    return Ok(Step::Abrupt(tdz_exception(
                        runtime,
                        frame,
                        BindingName::Local(index),
                        source_pc,
                    )?));
                }
                Err(BindingAccessError::Fault(fault)) => return Err(fault.into()),
            };
            push(frame, value);
        }
        FinalOpcode::PutLocCheck => {
            let index = local_index(opcode, operands)?;
            if binding_is_uninitialized(runtime, frame_local(frame, index)?)? {
                return Ok(Step::Abrupt(tdz_exception(
                    runtime,
                    frame,
                    BindingName::Local(index),
                    source_pc,
                )?));
            }
            let value = pop(frame)?;
            write_local(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::SetLocCheck => {
            let index = local_index(opcode, operands)?;
            if binding_is_uninitialized(runtime, frame_local(frame, index)?)? {
                return Ok(Step::Abrupt(tdz_exception(
                    runtime,
                    frame,
                    BindingName::Local(index),
                    source_pc,
                )?));
            }
            let value = peek(frame)?.duplicate();
            write_local(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::GetVarRefCheck => {
            let index = closure_index(opcode, operands)?;
            let value = match duplicate_environment(runtime, frame, index, true) {
                Ok(value) => value,
                Err(BindingAccessError::Uninitialized) => {
                    return Ok(Step::Abrupt(tdz_exception(
                        runtime,
                        frame,
                        BindingName::Closure(index),
                        source_pc,
                    )?));
                }
                Err(BindingAccessError::Fault(fault)) => return Err(fault.into()),
            };
            push(frame, value);
        }
        FinalOpcode::PutVarRefCheck => {
            let index = closure_index(opcode, operands)?;
            if environment_is_uninitialized(runtime, frame, index)? {
                return Ok(Step::Abrupt(tdz_exception(
                    runtime,
                    frame,
                    BindingName::Closure(index),
                    source_pc,
                )?));
            }
            let value = pop(frame)?;
            write_environment(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::CloseLoc => {
            let index = local_index(opcode, operands)?;
            close_local(runtime, frame, index)?;
        }
        FinalOpcode::ForInStart => {
            let work = runtime.preview_for_in_iterator_work(peek(frame)?)?;
            execution_budget.charge_instructions(work)?;
            let value = pop(frame)?;
            let realm = code(runtime, frame.code)?.realm;
            let (iterator, actual_work) = runtime.allocate_for_in_iterator(realm, value)?;
            debug_assert!(actual_work <= work);
            push(frame, StoredValue::Object(iterator));
        }
        FinalOpcode::ForInNext => {
            let iterator = match frame.stack.last() {
                Some(OperandStackEntry::JavaScript(StoredValue::Object(object)))
                    if runtime.is_for_in_iterator(*object)? =>
                {
                    *object
                }
                Some(
                    OperandStackEntry::JavaScript(
                        StoredValue::Undefined
                        | StoredValue::Null
                        | StoredValue::Boolean(_)
                        | StoredValue::Number(_)
                        | StoredValue::String(_)
                        | StoredValue::Symbol(_)
                        | StoredValue::Function(_)
                        | StoredValue::Object(_),
                    )
                    | OperandStackEntry::Catch { .. }
                    | OperandStackEntry::ForOfCatch { .. }
                    | OperandStackEntry::FinallyReturn { .. },
                ) => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "verified for_in_next cursor is not a for-in iterator",
                    }
                    .into());
                }
                None => {
                    return Err(EngineFault::StackDepthMismatch {
                        function: frame.template,
                        pc: source_pc,
                        expected: 1,
                        actual: 0,
                    }
                    .into());
                }
            };
            loop {
                let work = runtime.preview_for_in_advance_work(iterator)?;
                execution_budget.charge_instructions(work)?;
                let advance = runtime.advance_for_in_iterator(iterator)?;
                debug_assert!(advance.work() <= work);
                match advance {
                    ForInAdvance::Continue { .. } => {}
                    ForInAdvance::Yield { key, .. } => {
                        push(frame, for_in_key_value(&key)?);
                        push(frame, StoredValue::Boolean(false));
                        break;
                    }
                    ForInAdvance::Done { .. } => {
                        push(frame, StoredValue::Undefined);
                        push(frame, StoredValue::Boolean(true));
                        break;
                    }
                }
            }
        }
        FinalOpcode::ForOfStart => {
            let iterable = pop(frame)?;
            let realm = code(runtime, frame.code)?.realm;
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_for_of_start(
                    runtime,
                    iterable,
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::ForOfNext => {
            let Operands::U8(offset) = operands else {
                return unsupported_dispatch(opcode);
            };
            // Pinned QuickJS `js_for_of_next` behavior: once a step observed
            // `done`, the iterator slot is replaced with `undefined`, and
            // every later step yields `{ value: undefined, done: true }`
            // without invoking `next()` again. Array destructuring relies on
            // this for post-exhaustion elements, elisions, and rest.
            let marker = frame
                .stack
                .len()
                .checked_sub(1_usize.saturating_add(usize::from(offset)))
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "verified for-of next has no record marker",
                })?;
            let iterator_slot = marker.checked_sub(2).ok_or(EngineFault::RuntimeInvariant {
                message: "verified for-of next has an incomplete record",
            })?;
            let exhausted = matches!(
                frame.stack.get(iterator_slot),
                Some(OperandStackEntry::JavaScript(StoredValue::Undefined))
            );
            if exhausted {
                let return_to =
                    CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                        EngineFault::InvalidSuccessor {
                            function: frame.template,
                            pc: source_pc,
                        },
                    )?);
                push(frame, StoredValue::Undefined);
                push(frame, StoredValue::Boolean(true));
                frame.instruction = return_to.instruction;
                return Ok(Step::Continue);
            }
            let (iterator, next) = deactivate_for_of_record(frame, false, offset)?;
            let realm = code(runtime, frame.code)?.realm;
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_for_of_next(
                    iterator,
                    next,
                    realm,
                    offset,
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::IteratorClose => {
            let (iterator, _next) = deactivate_for_of_record(frame, true, 0)?;
            let realm = code(runtime, frame.code)?.realm;
            let return_to =
                CallReturn::discard(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_for_of_close(
                    runtime,
                    iterator,
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::CopyDataProperties => {
            let Operands::U8(mask) = operands else {
                return unsupported_dispatch(opcode);
            };
            let target_depth = usize::from(mask & 0b11);
            let source_depth = usize::from((mask >> 2) & 0b111);
            let excluded_depth = usize::from((mask >> 5) & 0b111);
            let target = stack_value_at(
                frame,
                frame
                    .stack
                    .len()
                    .checked_sub(1_usize.saturating_add(target_depth))
                    .ok_or(EngineFault::StackDepthMismatch {
                        function: frame.template,
                        pc: source_pc,
                        expected: u32::try_from(target_depth.saturating_add(1)).unwrap_or(u32::MAX),
                        actual: frame.stack.len(),
                    })?,
            )?
            .duplicate();
            let source = stack_value_at(
                frame,
                frame
                    .stack
                    .len()
                    .checked_sub(1_usize.saturating_add(source_depth))
                    .ok_or(EngineFault::StackDepthMismatch {
                        function: frame.template,
                        pc: source_pc,
                        expected: u32::try_from(source_depth.saturating_add(1)).unwrap_or(u32::MAX),
                        actual: frame.stack.len(),
                    })?,
            )?
            .duplicate();
            let excluded = stack_value_at(
                frame,
                frame
                    .stack
                    .len()
                    .checked_sub(1_usize.saturating_add(excluded_depth))
                    .ok_or(EngineFault::StackDepthMismatch {
                        function: frame.template,
                        pc: source_pc,
                        expected: u32::try_from(excluded_depth.saturating_add(1))
                            .unwrap_or(u32::MAX),
                        actual: frame.stack.len(),
                    })?,
            )?
            .duplicate();
            let realm = code(runtime, frame.code)?.realm;
            let return_to =
                CallReturn::discard(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_copy_data_properties(
                    runtime,
                    target,
                    source,
                    excluded,
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::IfFalse | FinalOpcode::IfFalse8 => {
            let condition = pop(frame)?;
            frame.instruction =
                branch_successor(verified_instruction, !condition.is_truthy(), frame)?;
            return Ok(Step::Continue);
        }
        FinalOpcode::IfTrue | FinalOpcode::IfTrue8 => {
            let condition = pop(frame)?;
            frame.instruction =
                branch_successor(verified_instruction, condition.is_truthy(), frame)?;
            return Ok(Step::Continue);
        }
        FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16 => {
            frame.instruction = verified_instruction.successors().jump_target().ok_or(
                EngineFault::InvalidSuccessor {
                    function: frame.template,
                    pc: source_pc,
                },
            )?;
            return Ok(Step::Continue);
        }
        FinalOpcode::Neg
        | FinalOpcode::Plus
        | FinalOpcode::Dec
        | FinalOpcode::Inc
        | FinalOpcode::PostDec
        | FinalOpcode::PostInc
        | FinalOpcode::Not => {
            let realm = code(runtime, frame.code)?.realm;
            let value = pop(frame)?;
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::Unary { opcode },
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::Mul
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
        | FinalOpcode::Or => {
            let realm = code(runtime, frame.code)?.realm;
            let right = pop(frame)?;
            let left = pop(frame)?;
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            let dispatch = if matches!(opcode, FinalOpcode::Eq | FinalOpcode::Neq) {
                begin_abstract_equality(
                    runtime,
                    left,
                    right,
                    opcode,
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                )
            } else {
                let hint = if opcode == FinalOpcode::Add {
                    OperatorPrimitiveHint::Default
                } else {
                    OperatorPrimitiveHint::Number
                };
                begin_operator_primitive_conversion(
                    runtime,
                    left,
                    hint,
                    OperatorPrimitiveTarget::BinaryRight {
                        opcode,
                        right,
                        hint,
                    },
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                )
            };
            return native_step(dispatch, return_to);
        }
        FinalOpcode::InstanceOf => {
            let realm = code(runtime, frame.code)?.realm;
            let right = pop(frame)?;
            let left = pop(frame)?;
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            let dispatch = begin_instance_of(
                runtime,
                left,
                right,
                realm,
                Some(return_to),
                origin,
                execution_budget,
            );
            return native_step(dispatch, return_to);
        }
        FinalOpcode::Lnot => {
            let value = pop(frame)?;
            push(frame, StoredValue::Boolean(!value.is_truthy()));
        }
        FinalOpcode::Typeof => {
            let value = pop(frame)?;
            let name = match value {
                StoredValue::Undefined => "undefined",
                StoredValue::Null | StoredValue::Object(_) => "object",
                StoredValue::Boolean(_) => "boolean",
                StoredValue::Number(_) => "number",
                StoredValue::String(_) => "string",
                StoredValue::Symbol(_) => "symbol",
                StoredValue::Function(_) => "function",
            };
            push(frame, StoredValue::String(JsString::from_utf8(name)?));
        }
        FinalOpcode::StrictEq | FinalOpcode::StrictNeq => {
            let right = pop(frame)?;
            let left = pop(frame)?;
            let equal = left.strict_equals(&right);
            push(
                frame,
                StoredValue::Boolean(if opcode == FinalOpcode::StrictEq {
                    equal
                } else {
                    !equal
                }),
            );
        }
        FinalOpcode::IsUndefinedOrNull => {
            let value = pop(frame)?;
            push(
                frame,
                StoredValue::Boolean(matches!(value, StoredValue::Undefined | StoredValue::Null)),
            );
        }
        FinalOpcode::Throw => {
            let realm = code(runtime, frame.code)?.realm;
            let origin = instruction_location(runtime, frame, source_pc)?;
            let value = pop(frame)?;
            return Ok(Step::Abrupt(PendingException {
                realm,
                payload: PendingExceptionPayload::ThrownValue(value),
                origin,
            }));
        }
        FinalOpcode::Return => return Ok(Step::Return(pop(frame)?)),
        FinalOpcode::ReturnUndef => return Ok(Step::Return(StoredValue::Undefined)),
        FinalOpcode::Nop => {}
        _ => return unsupported_dispatch(opcode),
    }

    frame.instruction =
        verified_instruction
            .successors()
            .fallthrough()
            .ok_or(EngineFault::InvalidSuccessor {
                function: frame.template,
                pc: source_pc,
            })?;
    Ok(Step::Continue)
}
