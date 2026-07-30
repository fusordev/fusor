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

//! Iterative execution of runtime-installed verified bytecode.

use quickjs_bytecode::{
    BytecodePc, CompilerClosureSource, FinalOpcode, FunctionTemplateId, InstructionIndex, Operands,
    VerifiedSuccessorKind,
};

use crate::{
    Context, EngineFault, ExecutionError, Function, HandleError, HandleKind, JsException,
    JsStackFrame, JsString, JsValue, Runtime, RuntimeResource,
    ids::{BindingCellId, FunctionId, InstalledCodeId},
    runtime::{
        BindingCell, FrameBindingAddress, HeapFunction, InstalledCode, InstalledConstant,
        InstalledTemplate, check_execution_limit, usize_to_u64,
    },
    value::{SlotValue, StoredValue},
};

/// Inclusive per-call interpreter limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionLimits {
    instruction_fuel: u64,
}

impl ExecutionLimits {
    /// Replaces the maximum number of completed bytecode instructions.
    #[must_use]
    pub const fn with_instruction_fuel(mut self, instruction_fuel: u64) -> Self {
        self.instruction_fuel = instruction_fuel;
        self
    }
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            instruction_fuel: 10_000_000,
        }
    }
}

enum FrameBinding {
    Direct(SlotValue),
    Captured(BindingCellId),
}

struct Frame {
    function: FunctionId,
    code: InstalledCodeId,
    template: FunctionTemplateId,
    instruction: InstructionIndex,
    return_to: Option<InstructionIndex>,
    reserved_values: u64,
    arguments: Vec<FrameBinding>,
    locals: Vec<FrameBinding>,
    own_cells: Vec<Option<BindingCellId>>,
    own_cell_bindings: Vec<FrameBindingAddress>,
    environment: Vec<BindingCellId>,
    stack: Vec<StoredValue>,
}

#[derive(Clone, Copy)]
struct FramePlan {
    function: FunctionId,
    code: InstalledCodeId,
    template: FunctionTemplateId,
    argument_count: usize,
    local_count: usize,
    stack_capacity: usize,
    reserved_values: u64,
    instruction: InstructionIndex,
}

enum Step {
    Continue,
    Call {
        function: FunctionId,
        argument_count: usize,
        return_to: InstructionIndex,
    },
    Return(StoredValue),
}

enum FrameArguments<'a> {
    Public(&'a [JsValue]),
    Owned(Vec<StoredValue>),
}

#[derive(Clone, Copy)]
enum BindingName {
    Local(u32),
    Closure(u32),
}

enum ClosureCapturePlan {
    Existing(BindingCellId),
    New(usize),
}

struct PendingOwnCell {
    own_index: usize,
    address: FrameBindingAddress,
    value: SlotValue,
}

impl Context<'_> {
    /// Invokes one runtime-installed ordinary bytecode function.
    ///
    /// Execution starts only at verified instruction zero and advances only
    /// through verified successor identities. Direct ordinary JavaScript calls
    /// push runtime frames onto one explicit vector; Rust stack recursion is
    /// never used. Method calls and constructors remain outside this profile.
    ///
    /// # Errors
    ///
    /// Rejects orphaned, foreign, or stale handles before frame mutation.
    /// During execution it returns the admitted exact TDZ `ReferenceError` or
    /// non-callable `TypeError`, with verified origin and caller provenance,
    /// plus instruction interruption, resource/allocation failures, or
    /// internal engine faults.
    pub fn call(
        &mut self,
        function: &Function,
        arguments: &[JsValue],
        limits: ExecutionLimits,
    ) -> Result<JsValue, ExecutionError> {
        self.runtime.prepare_execution_safe_point()?;
        let owner = function.owner()?;
        self.runtime.validate_owner(&owner, HandleKind::Function)?;
        let function_id = function.id()?;
        if !self.runtime.functions.contains(function_id) {
            return Err(HandleError::Stale {
                kind: HandleKind::Function,
                index: function_id.index(),
                generation: function_id.generation(),
            }
            .into());
        }

        for argument in arguments {
            let owner = argument.owner()?;
            self.runtime.validate_owner(&owner, HandleKind::Value)?;
            if let StoredValue::Function(id) = argument.stored()?
                && !self.runtime.functions.contains(*id)
            {
                return Err(HandleError::Stale {
                    kind: HandleKind::Value,
                    index: id.index(),
                    generation: id.generation(),
                }
                .into());
            }
        }

        let plan = plan_frame(self.runtime, function_id, 0, 0)?;
        let frame = create_frame(self.runtime, plan, FrameArguments::Public(arguments), None)?;
        let value = execute_frames(self.runtime, frame, limits)?;
        self.runtime.public_value(value)
    }
}

fn execute_frames(
    runtime: &mut Runtime,
    initial: Frame,
    limits: ExecutionLimits,
) -> Result<StoredValue, ExecutionError> {
    let mut active_frame_values = initial.reserved_values;
    let mut frames = Vec::new();
    frames
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    frames.push(initial);

    let mut executed = 0_u64;
    loop {
        if executed == limits.instruction_fuel {
            return Err(ExecutionError::InstructionLimitExceeded {
                limit: limits.instruction_fuel,
                executed,
            });
        }
        let frame = frames.last_mut().ok_or(EngineFault::MissingInstruction {
            function: FunctionTemplateId::new(0),
            instruction: 0,
        })?;
        executed += 1;
        let step = execute_one(runtime, frame);
        let step = match step {
            Ok(step) => step,
            Err(ExecutionError::Exception(mut exception)) => {
                attach_exception_callers(runtime, &frames, &mut exception)?;
                return Err(ExecutionError::Exception(exception));
            }
            Err(error) => return Err(error),
        };
        match step {
            Step::Continue => {}
            Step::Call {
                function,
                argument_count,
                return_to,
            } => {
                let plan = plan_frame(runtime, function, frames.len(), active_frame_values)?;
                frames
                    .try_reserve(1)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::Frames,
                        additional: 1,
                    })?;
                let arguments = take_direct_call_arguments(
                    frames.last_mut().ok_or(EngineFault::MissingInstruction {
                        function: FunctionTemplateId::new(0),
                        instruction: 0,
                    })?,
                    function,
                    argument_count,
                )?;
                let child = create_frame(
                    runtime,
                    plan,
                    FrameArguments::Owned(arguments),
                    Some(return_to),
                )?;
                active_frame_values = active_frame_values.saturating_add(child.reserved_values);
                frames.push(child);
            }
            Step::Return(value) => {
                let finished = frames.pop().ok_or(EngineFault::MissingInstruction {
                    function: FunctionTemplateId::new(0),
                    instruction: 0,
                })?;
                active_frame_values = active_frame_values.saturating_sub(finished.reserved_values);
                if let Some(parent) = frames.last_mut() {
                    let return_to = finished.return_to.ok_or(EngineFault::RuntimeInvariant {
                        message: "nested frame has no caller continuation",
                    })?;
                    if parent.stack.len() == parent.stack.capacity() {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "verified call result exceeds frame stack capacity",
                        }
                        .into());
                    }
                    parent.stack.push(value);
                    parent.instruction = return_to;
                    continue;
                }
                if finished.return_to.is_some() {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "host frame has a caller continuation",
                    }
                    .into());
                }
                return Ok(value);
            }
        }
    }
}

fn plan_frame(
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
    let code_id = function.code;
    let template_id = function.template;

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
    if function.environment.len() != verified.metadata().closures().len() {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: template_id,
        }
        .into());
    }
    for cell in function.environment.iter().copied() {
        if !runtime.cells.contains(cell) {
            return Err(EngineFault::StaleHeapEdge {
                edge: "closure cell",
                index: cell.index(),
                generation: cell.generation(),
            }
            .into());
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

    Ok(FramePlan {
        function: function_id,
        code: code_id,
        template: template_id,
        argument_count,
        local_count,
        stack_capacity,
        reserved_values: frame_values,
        instruction,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "failure-atomic frame allocation and initialization remain one transaction"
)]
fn create_frame(
    runtime: &Runtime,
    plan: FramePlan,
    supplied: FrameArguments<'_>,
    return_to: Option<InstructionIndex>,
) -> Result<Frame, ExecutionError> {
    let function = runtime
        .functions
        .get(plan.function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "function",
            index: plan.function.index(),
            generation: plan.function.generation(),
        })?;
    let environment = copy_ids(&function.environment, RuntimeResource::FrameValues)?;
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
            let mut supplied = supplied.into_iter();
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

    Ok(Frame {
        function: plan.function,
        code: plan.code,
        template: plan.template,
        instruction: plan.instruction,
        return_to,
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
fn execute_one(runtime: &mut Runtime, frame: &mut Frame) -> Result<Step, ExecutionError> {
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
            frame.stack.push(StoredValue::Number(value.into()));
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
            frame.stack.push(StoredValue::Number(value.into()));
        }
        FinalOpcode::PushI8 => {
            let Operands::I8(value) = operands else {
                return unsupported_dispatch(opcode);
            };
            frame
                .stack
                .push(StoredValue::Number(i32::from(value).into()));
        }
        FinalOpcode::PushI16 => {
            let Operands::I16(value) = operands else {
                return unsupported_dispatch(opcode);
            };
            frame
                .stack
                .push(StoredValue::Number(i32::from(value).into()));
        }
        FinalOpcode::PushConst | FinalOpcode::PushConst8 => {
            let index = constant_index(operands).ok_or(EngineFault::MissingPoolEntry {
                pool: "constant",
                index: u32::MAX,
            })?;
            frame.stack.push(materialize_constant(
                runtime,
                frame.code,
                frame.template,
                index,
            )?);
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
            frame.stack.push(StoredValue::String(string));
        }
        FinalOpcode::PushEmptyString => {
            frame.stack.push(StoredValue::String(JsString::empty()));
        }
        FinalOpcode::Undefined => frame.stack.push(StoredValue::Undefined),
        FinalOpcode::Null => frame.stack.push(StoredValue::Null),
        FinalOpcode::PushFalse => frame.stack.push(StoredValue::Boolean(false)),
        FinalOpcode::PushTrue => frame.stack.push(StoredValue::Boolean(true)),
        FinalOpcode::Drop => {
            pop(frame)?;
        }
        FinalOpcode::Dup => {
            let value = frame
                .stack
                .last()
                .ok_or(EngineFault::StackDepthMismatch {
                    function: frame.template,
                    pc: source_pc,
                    expected: 1,
                    actual: 0,
                })?
                .duplicate();
            frame.stack.push(value);
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
            let StoredValue::Function(function) = &frame.stack[callee_index] else {
                return Err(ExecutionError::Exception(not_callable_exception(
                    runtime, frame, source_pc,
                )?));
            };
            let return_to = verified_instruction.successors().fallthrough().ok_or(
                EngineFault::InvalidSuccessor {
                    function: frame.template,
                    pc: source_pc,
                },
            )?;
            return Ok(Step::Call {
                function: *function,
                argument_count,
                return_to,
            });
        }
        FinalOpcode::FClosure | FinalOpcode::FClosure8 => {
            let index = constant_index(operands).ok_or(EngineFault::MissingPoolEntry {
                pool: "function constant",
                index: u32::MAX,
            })?;
            let child = function_constant(runtime, frame.code, frame.template, index)?;
            let function = create_closure(runtime, frame, child)?;
            frame.stack.push(StoredValue::Function(function));
        }
        FinalOpcode::GetArg
        | FinalOpcode::GetArg0
        | FinalOpcode::GetArg1
        | FinalOpcode::GetArg2
        | FinalOpcode::GetArg3 => {
            let index = argument_index(opcode, operands)?;
            let value = duplicate_binding(runtime, frame_argument(frame, index)?, false, frame)?;
            frame.stack.push(value);
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
        FinalOpcode::GetLoc
        | FinalOpcode::GetLoc8
        | FinalOpcode::GetLoc0
        | FinalOpcode::GetLoc1
        | FinalOpcode::GetLoc2
        | FinalOpcode::GetLoc3 => {
            let index = local_index(opcode, operands)?;
            let value = duplicate_binding(runtime, frame_local(frame, index)?, false, frame)?;
            frame.stack.push(value);
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
            frame
                .stack
                .push(duplicate_environment(runtime, frame, index, false)?);
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
                    return Err(ExecutionError::Exception(tdz_exception(
                        runtime,
                        frame,
                        BindingName::Local(index),
                        source_pc,
                    )?));
                }
                Err(BindingAccessError::Fault(fault)) => return Err(fault.into()),
            };
            frame.stack.push(value);
        }
        FinalOpcode::PutLocCheck => {
            let index = local_index(opcode, operands)?;
            if binding_is_uninitialized(runtime, frame_local(frame, index)?)? {
                return Err(ExecutionError::Exception(tdz_exception(
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
                return Err(ExecutionError::Exception(tdz_exception(
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
                    return Err(ExecutionError::Exception(tdz_exception(
                        runtime,
                        frame,
                        BindingName::Closure(index),
                        source_pc,
                    )?));
                }
                Err(BindingAccessError::Fault(fault)) => return Err(fault.into()),
            };
            frame.stack.push(value);
        }
        FinalOpcode::PutVarRefCheck => {
            let index = closure_index(opcode, operands)?;
            if environment_is_uninitialized(runtime, frame, index)? {
                return Err(ExecutionError::Exception(tdz_exception(
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
        FinalOpcode::Lnot => {
            let value = pop(frame)?;
            frame.stack.push(StoredValue::Boolean(!value.is_truthy()));
        }
        FinalOpcode::Typeof => {
            let value = pop(frame)?;
            let name = match value {
                StoredValue::Undefined => "undefined",
                StoredValue::Null => "object",
                StoredValue::Boolean(_) => "boolean",
                StoredValue::Number(_) => "number",
                StoredValue::String(_) => "string",
                StoredValue::Function(_) => "function",
            };
            frame
                .stack
                .push(StoredValue::String(JsString::from_utf8(name)?));
        }
        FinalOpcode::StrictEq | FinalOpcode::StrictNeq => {
            let right = pop(frame)?;
            let left = pop(frame)?;
            let equal = left.strict_equals(&right);
            frame
                .stack
                .push(StoredValue::Boolean(if opcode == FinalOpcode::StrictEq {
                    equal
                } else {
                    !equal
                }));
        }
        FinalOpcode::IsUndefinedOrNull => {
            let value = pop(frame)?;
            frame.stack.push(StoredValue::Boolean(matches!(
                value,
                StoredValue::Undefined | StoredValue::Null
            )));
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

trait AtomDescription {
    fn description(&self) -> Option<&JsString>;
}

impl AtomDescription for crate::Atom {
    fn description(&self) -> Option<&JsString> {
        crate::Atom::description(self)
    }
}

fn code(runtime: &Runtime, id: InstalledCodeId) -> Result<&InstalledCode, EngineFault> {
    runtime.code.get(id).ok_or(EngineFault::StaleHeapEdge {
        edge: "installed code",
        index: id.index(),
        generation: id.generation(),
    })
}

fn installed_template(
    runtime: &Runtime,
    code_id: InstalledCodeId,
    template: FunctionTemplateId,
) -> Result<&InstalledTemplate, EngineFault> {
    let code = code(runtime, code_id)?;
    let index = usize::try_from(template.get())
        .map_err(|_| EngineFault::InvalidClosureEnvironment { function: template })?;
    code.templates
        .get(index)
        .ok_or(EngineFault::InvalidClosureEnvironment { function: template })
}

fn materialize_constant(
    runtime: &Runtime,
    code_id: InstalledCodeId,
    template: FunctionTemplateId,
    index: u32,
) -> Result<StoredValue, ExecutionError> {
    match installed_template(runtime, code_id, template)?
        .constants
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "constant",
            index,
        })? {
        InstalledConstant::Number(value) => Ok(StoredValue::Number(*value)),
        InstalledConstant::String(value) => Ok(StoredValue::String(value.clone())),
        InstalledConstant::Function(_) => Err(EngineFault::MissingPoolEntry {
            pool: "ordinary value constant",
            index,
        }
        .into()),
    }
}

fn function_constant(
    runtime: &Runtime,
    code_id: InstalledCodeId,
    template: FunctionTemplateId,
    index: u32,
) -> Result<FunctionTemplateId, ExecutionError> {
    match installed_template(runtime, code_id, template)?
        .constants
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "function constant",
            index,
        })? {
        InstalledConstant::Function(function) => Ok(*function),
        InstalledConstant::Number(_) | InstalledConstant::String(_) => {
            Err(EngineFault::MissingPoolEntry {
                pool: "function constant",
                index,
            }
            .into())
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "closure validation, capture materialization, and publication are one transaction"
)]
fn create_closure(
    runtime: &mut Runtime,
    frame: &mut Frame,
    child: FunctionTemplateId,
) -> Result<FunctionId, ExecutionError> {
    let parent = runtime
        .functions
        .get(frame.function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "active function",
            index: frame.function.index(),
            generation: frame.function.generation(),
        })?;
    if parent.code != frame.code
        || parent.template != frame.template
        || parent.environment.as_slice() != frame.environment.as_slice()
    {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        }
        .into());
    }
    let (sources, expected) = {
        let code = code(runtime, frame.code)?;
        let function = code
            .authority
            .function(child)
            .ok_or(EngineFault::InvalidClosureEnvironment { function: child })?;
        let source = function.function().closure_sources();
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(source.len())
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: source.len(),
            })?;
        copied.extend_from_slice(source);
        (copied, function.metadata().closures().len())
    };
    if sources.len() != expected {
        return Err(EngineFault::InvalidClosureEnvironment { function: child }.into());
    }

    let mut capture_plans = Vec::new();
    capture_plans
        .try_reserve_exact(sources.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: sources.len(),
        })?;
    let mut pending_by_own = Vec::new();
    pending_by_own
        .try_reserve_exact(frame.own_cells.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: frame.own_cells.len(),
        })?;
    pending_by_own.resize(frame.own_cells.len(), None);
    let mut pending_cells = Vec::new();
    pending_cells
        .try_reserve_exact(sources.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: sources.len(),
        })?;

    for source in sources {
        match source {
            CompilerClosureSource::ParentVariableReference(index) => {
                let own_index = index as usize;
                let own_cell = frame.own_cells.get(own_index).copied().ok_or(
                    EngineFault::MissingPoolEntry {
                        pool: "own variable-reference",
                        index,
                    },
                )?;
                let address = *frame.own_cell_bindings.get(own_index).ok_or(
                    EngineFault::MissingPoolEntry {
                        pool: "own captured binding",
                        index,
                    },
                )?;
                if let Some(cell) = own_cell {
                    if !runtime.cells.contains(cell) {
                        return Err(EngineFault::StaleHeapEdge {
                            edge: "binding cell",
                            index: cell.index(),
                            generation: cell.generation(),
                        }
                        .into());
                    }
                    capture_plans.push(ClosureCapturePlan::Existing(cell));
                    continue;
                }

                if let Some(pending) = pending_by_own[own_index] {
                    capture_plans.push(ClosureCapturePlan::New(pending));
                    continue;
                }

                let binding = match address {
                    FrameBindingAddress::Argument(binding) => frame_argument(frame, binding)?,
                    FrameBindingAddress::Local(binding) => frame_local(frame, binding)?,
                };
                let FrameBinding::Direct(value) = binding else {
                    return Err(EngineFault::InvalidClosureEnvironment {
                        function: frame.template,
                    }
                    .into());
                };
                let pending = pending_cells.len();
                pending_cells.push(PendingOwnCell {
                    own_index,
                    address,
                    value: value.duplicate(),
                });
                pending_by_own[own_index] = Some(pending);
                capture_plans.push(ClosureCapturePlan::New(pending));
            }
            CompilerClosureSource::ParentClosure(index) => {
                let cell = *frame.environment.get(index as usize).ok_or(
                    EngineFault::MissingPoolEntry {
                        pool: "parent closure",
                        index,
                    },
                )?;
                if !runtime.cells.contains(cell) {
                    return Err(EngineFault::StaleHeapEdge {
                        edge: "closure cell",
                        index: cell.index(),
                        generation: cell.generation(),
                    }
                    .into());
                }
                capture_plans.push(ClosureCapturePlan::Existing(cell));
            }
        }
    }

    check_execution_limit(
        RuntimeResource::HeapFunctions,
        runtime.limits.max_heap_functions,
        usize_to_u64(runtime.functions.len()).saturating_add(1),
    )?;
    check_execution_limit(
        RuntimeResource::BindingCells,
        runtime.limits.max_binding_cells,
        usize_to_u64(runtime.cells.len()).saturating_add(usize_to_u64(pending_cells.len())),
    )?;
    runtime
        .functions
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapFunctions,
            additional: 1,
        })?;
    runtime
        .cells
        .try_reserve(pending_cells.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: pending_cells.len(),
        })?;
    let mut environment = Vec::new();
    environment
        .try_reserve_exact(capture_plans.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: capture_plans.len(),
        })?;

    let mut new_cells = Vec::new();
    new_cells
        .try_reserve_exact(pending_cells.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: pending_cells.len(),
        })?;
    for pending in &pending_cells {
        if let Ok(cell) = runtime.cells.try_insert(BindingCell {
            value: pending.value.duplicate(),
        }) {
            new_cells.push(cell);
        } else {
            for cell in new_cells {
                let removed = runtime.cells.remove(cell);
                debug_assert!(removed.is_some());
            }
            return Err(ExecutionError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: 1,
            });
        }
    }

    for capture in capture_plans {
        let cell = match capture {
            ClosureCapturePlan::Existing(cell) => cell,
            ClosureCapturePlan::New(index) => {
                let Some(cell) = new_cells.get(index).copied() else {
                    rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
                    return Err(EngineFault::InvalidClosureEnvironment { function: child }.into());
                };
                cell
            }
        };
        environment.push(cell);
    }

    for (pending, cell) in pending_cells.iter().zip(new_cells.iter().copied()) {
        let binding = match pending.address {
            FrameBindingAddress::Argument(index) => frame_argument_mut(frame, index),
            FrameBindingAddress::Local(index) => frame_local_mut(frame, index),
        };
        let binding = match binding {
            Ok(binding) => binding,
            Err(fault) => {
                rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
                return Err(fault.into());
            }
        };
        *binding = FrameBinding::Captured(cell);
        frame.own_cells[pending.own_index] = Some(cell);
    }

    let Ok(function) = runtime.functions.try_insert(HeapFunction {
        code: frame.code,
        template: child,
        environment,
        public_roots: 0,
    }) else {
        rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
        return Err(ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapFunctions,
            additional: 1,
        });
    };
    let Some(code) = runtime.code.get_mut(frame.code) else {
        let removed = runtime.functions.remove(function);
        debug_assert!(removed.is_some());
        rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
        return Err(EngineFault::StaleHeapEdge {
            edge: "installed code",
            index: frame.code.index(),
            generation: frame.code.generation(),
        }
        .into());
    };
    code.live_functions = code.live_functions.saturating_add(1);
    runtime.collection_pending = true;
    Ok(function)
}

fn rollback_new_cells(
    runtime: &mut Runtime,
    frame: &mut Frame,
    pending_cells: &[PendingOwnCell],
    new_cells: &[BindingCellId],
) {
    for (pending, cell) in pending_cells.iter().zip(new_cells.iter().copied()) {
        let binding = match pending.address {
            FrameBindingAddress::Argument(index) => frame.arguments.get_mut(index as usize),
            FrameBindingAddress::Local(index) => frame.locals.get_mut(index as usize),
        };
        if let Some(binding) = binding {
            *binding = FrameBinding::Direct(pending.value.duplicate());
        }
        if let Some(own_cell) = frame.own_cells.get_mut(pending.own_index) {
            *own_cell = None;
        }
        let removed = runtime.cells.remove(cell);
        debug_assert!(removed.is_some());
    }
}

fn close_local(runtime: &Runtime, frame: &mut Frame, local: u32) -> Result<(), ExecutionError> {
    let Some(index) = frame.own_cell_bindings.iter().position(
        |address| matches!(address, FrameBindingAddress::Local(index) if *index == local),
    ) else {
        return Err(EngineFault::MissingPoolEntry {
            pool: "captured local",
            index: local,
        }
        .into());
    };
    let Some(cell) = frame.own_cells[index] else {
        return Ok(());
    };
    let value = runtime
        .cells
        .get(cell)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "binding cell",
            index: cell.index(),
            generation: cell.generation(),
        })?
        .value
        .duplicate();
    *frame_local_mut(frame, local)? = FrameBinding::Direct(value);
    frame.own_cells[index] = None;
    Ok(())
}

enum BindingAccessError {
    Uninitialized,
    Fault(EngineFault),
}

impl From<BindingAccessError> for ExecutionError {
    fn from(error: BindingAccessError) -> Self {
        match error {
            BindingAccessError::Uninitialized => EngineFault::UnexpectedUninitialized {
                function: FunctionTemplateId::new(u32::MAX),
            }
            .into(),
            BindingAccessError::Fault(fault) => fault.into(),
        }
    }
}

fn duplicate_binding(
    runtime: &Runtime,
    binding: &FrameBinding,
    checked: bool,
    frame: &Frame,
) -> Result<StoredValue, BindingAccessError> {
    let value = match binding {
        FrameBinding::Direct(value) => value,
        FrameBinding::Captured(cell) => {
            &runtime
                .cells
                .get(*cell)
                .ok_or_else(|| {
                    BindingAccessError::Fault(EngineFault::StaleHeapEdge {
                        edge: "binding cell",
                        index: cell.index(),
                        generation: cell.generation(),
                    })
                })?
                .value
        }
    };
    match value {
        SlotValue::Uninitialized if checked => Err(BindingAccessError::Uninitialized),
        SlotValue::Uninitialized => Err(BindingAccessError::Fault(
            EngineFault::UnexpectedUninitialized {
                function: frame.template,
            },
        )),
        SlotValue::Value(value) => Ok(value.duplicate()),
    }
}

fn binding_is_uninitialized(
    runtime: &Runtime,
    binding: &FrameBinding,
) -> Result<bool, EngineFault> {
    Ok(match binding {
        FrameBinding::Direct(value) => matches!(value, SlotValue::Uninitialized),
        FrameBinding::Captured(cell) => matches!(
            runtime
                .cells
                .get(*cell)
                .ok_or(EngineFault::StaleHeapEdge {
                    edge: "binding cell",
                    index: cell.index(),
                    generation: cell.generation(),
                })?
                .value,
            SlotValue::Uninitialized
        ),
    })
}

fn write_argument(
    runtime: &mut Runtime,
    frame: &mut Frame,
    index: u32,
    value: SlotValue,
) -> Result<(), ExecutionError> {
    write_binding(runtime, frame_argument_mut(frame, index)?, value)
}

fn write_local(
    runtime: &mut Runtime,
    frame: &mut Frame,
    index: u32,
    value: SlotValue,
) -> Result<(), ExecutionError> {
    write_binding(runtime, frame_local_mut(frame, index)?, value)
}

fn write_binding(
    runtime: &mut Runtime,
    binding: &mut FrameBinding,
    value: SlotValue,
) -> Result<(), ExecutionError> {
    match binding {
        FrameBinding::Direct(current) => *current = value,
        FrameBinding::Captured(cell) => {
            runtime
                .cells
                .get_mut(*cell)
                .ok_or(EngineFault::StaleHeapEdge {
                    edge: "binding cell",
                    index: cell.index(),
                    generation: cell.generation(),
                })?
                .value = value;
            runtime.collection_pending = true;
        }
    }
    Ok(())
}

fn duplicate_environment(
    runtime: &Runtime,
    frame: &Frame,
    index: u32,
    checked: bool,
) -> Result<StoredValue, BindingAccessError> {
    let cell = *frame.environment.get(index as usize).ok_or({
        BindingAccessError::Fault(EngineFault::MissingPoolEntry {
            pool: "closure environment",
            index,
        })
    })?;
    let value = &runtime
        .cells
        .get(cell)
        .ok_or_else(|| {
            BindingAccessError::Fault(EngineFault::StaleHeapEdge {
                edge: "binding cell",
                index: cell.index(),
                generation: cell.generation(),
            })
        })?
        .value;
    match value {
        SlotValue::Uninitialized if checked => Err(BindingAccessError::Uninitialized),
        SlotValue::Uninitialized => Err(BindingAccessError::Fault(
            EngineFault::InvalidClosureEnvironment {
                function: frame.template,
            },
        )),
        SlotValue::Value(value) => Ok(value.duplicate()),
    }
}

fn environment_is_uninitialized(
    runtime: &Runtime,
    frame: &Frame,
    index: u32,
) -> Result<bool, EngineFault> {
    let cell = *frame
        .environment
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "closure environment",
            index,
        })?;
    Ok(matches!(
        runtime
            .cells
            .get(cell)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "binding cell",
                index: cell.index(),
                generation: cell.generation(),
            })?
            .value,
        SlotValue::Uninitialized
    ))
}

fn write_environment(
    runtime: &mut Runtime,
    frame: &Frame,
    index: u32,
    value: SlotValue,
) -> Result<(), ExecutionError> {
    let cell = *frame
        .environment
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "closure environment",
            index,
        })?;
    runtime
        .cells
        .get_mut(cell)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "binding cell",
            index: cell.index(),
            generation: cell.generation(),
        })?
        .value = value;
    runtime.collection_pending = true;
    Ok(())
}

fn tdz_exception(
    runtime: &Runtime,
    frame: &Frame,
    binding: BindingName,
    pc: BytecodePc,
) -> Result<JsException, ExecutionError> {
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
    Ok(JsException::reference_error(
        message,
        instruction_location(runtime, frame, pc)?,
    ))
}

fn not_callable_exception(
    runtime: &Runtime,
    frame: &Frame,
    pc: BytecodePc,
) -> Result<JsException, ExecutionError> {
    Ok(JsException::type_error(
        JsString::from_utf8("not a function")?,
        instruction_location(runtime, frame, pc)?,
    ))
}

fn instruction_location(
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

fn attach_exception_callers(
    runtime: &Runtime,
    frames: &[Frame],
    exception: &mut JsException,
) -> Result<(), ExecutionError> {
    let caller_count = frames.len().saturating_sub(1);
    exception
        .try_reserve_caller_frames(caller_count)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ExceptionFrames,
            additional: caller_count,
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
        ) {
            return Err(EngineFault::RuntimeInvariant {
                message: "exception caller is not parked at a direct call",
            }
            .into());
        }
        exception.push_caller_frame(instruction_location(
            runtime,
            caller,
            instruction.decoded().pc(),
        )?);
    }
    Ok(())
}

fn frame_argument(frame: &Frame, index: u32) -> Result<&FrameBinding, EngineFault> {
    frame
        .arguments
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "argument",
            index,
        })
}

fn frame_argument_mut(frame: &mut Frame, index: u32) -> Result<&mut FrameBinding, EngineFault> {
    frame
        .arguments
        .get_mut(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "argument",
            index,
        })
}

fn frame_local(frame: &Frame, index: u32) -> Result<&FrameBinding, EngineFault> {
    frame
        .locals
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "local",
            index,
        })
}

fn frame_local_mut(frame: &mut Frame, index: u32) -> Result<&mut FrameBinding, EngineFault> {
    frame
        .locals
        .get_mut(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "local",
            index,
        })
}

fn take_direct_call_arguments(
    frame: &mut Frame,
    expected_function: FunctionId,
    argument_count: usize,
) -> Result<Vec<StoredValue>, ExecutionError> {
    let required = argument_count.saturating_add(1);
    if frame.stack.len() < required {
        return Err(EngineFault::StackDepthMismatch {
            function: frame.template,
            pc: BytecodePc::ZERO,
            expected: u32::try_from(required).unwrap_or(u32::MAX),
            actual: frame.stack.len(),
        }
        .into());
    }
    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(argument_count)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: argument_count,
        })?;
    for _ in 0..argument_count {
        arguments.push(pop(frame)?);
    }
    arguments.reverse();
    match pop(frame)? {
        StoredValue::Function(actual) if actual == expected_function => Ok(arguments),
        StoredValue::Function(_) => Err(EngineFault::RuntimeInvariant {
            message: "parked direct-call callee changed before frame creation",
        }
        .into()),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_) => Err(EngineFault::RuntimeInvariant {
            message: "validated direct-call callee changed value kind",
        }
        .into()),
    }
}

fn pop(frame: &mut Frame) -> Result<StoredValue, EngineFault> {
    frame.stack.pop().ok_or(EngineFault::StackDepthMismatch {
        function: frame.template,
        pc: BytecodePc::ZERO,
        expected: 1,
        actual: 0,
    })
}

fn peek(frame: &Frame) -> Result<&StoredValue, EngineFault> {
    frame.stack.last().ok_or(EngineFault::StackDepthMismatch {
        function: frame.template,
        pc: BytecodePc::ZERO,
        expected: 1,
        actual: 0,
    })
}

fn branch_successor(
    instruction: quickjs_bytecode::VerifiedInstruction,
    taken: bool,
    frame: &Frame,
) -> Result<InstructionIndex, EngineFault> {
    let successors = instruction.successors();
    if successors.kind() != VerifiedSuccessorKind::Branch {
        return Err(EngineFault::InvalidSuccessor {
            function: frame.template,
            pc: instruction.decoded().pc(),
        });
    }
    if taken {
        successors.branch_target()
    } else {
        successors.fallthrough()
    }
    .ok_or(EngineFault::InvalidSuccessor {
        function: frame.template,
        pc: instruction.decoded().pc(),
    })
}

fn constant_index(operands: Operands) -> Option<u32> {
    match operands {
        Operands::Const(index) => Some(index),
        Operands::Const8(index) => Some(u32::from(index)),
        _ => None,
    }
}

fn direct_call_argument_count(
    opcode: FinalOpcode,
    operands: Operands,
) -> Result<usize, EngineFault> {
    match (opcode, operands) {
        (FinalOpcode::Call, Operands::NPop { argument_count }) => Ok(usize::from(argument_count)),
        (FinalOpcode::Call0, Operands::NPopX) => Ok(0),
        (FinalOpcode::Call1, Operands::NPopX) => Ok(1),
        (FinalOpcode::Call2, Operands::NPopX) => Ok(2),
        (FinalOpcode::Call3, Operands::NPopX) => Ok(3),
        _ => Err(EngineFault::UnsupportedDispatch { opcode }),
    }
}

fn argument_index(opcode: FinalOpcode, operands: Operands) -> Result<u32, EngineFault> {
    match operands {
        Operands::Arg(index) => Ok(u32::from(index)),
        Operands::NoneArg => implied_index(opcode).ok_or(EngineFault::MissingPoolEntry {
            pool: "implied argument",
            index: u32::MAX,
        }),
        _ => Err(EngineFault::UnsupportedDispatch { opcode }),
    }
}

fn local_index(opcode: FinalOpcode, operands: Operands) -> Result<u32, EngineFault> {
    match operands {
        Operands::Loc(index) => Ok(u32::from(index)),
        Operands::Loc8(index) => Ok(u32::from(index)),
        Operands::NoneLoc => implied_index(opcode).ok_or(EngineFault::MissingPoolEntry {
            pool: "implied local",
            index: u32::MAX,
        }),
        _ => Err(EngineFault::UnsupportedDispatch { opcode }),
    }
}

fn closure_index(opcode: FinalOpcode, operands: Operands) -> Result<u32, EngineFault> {
    match operands {
        Operands::VarRef(index) => Ok(u32::from(index)),
        Operands::NoneVarRef => implied_index(opcode).ok_or(EngineFault::MissingPoolEntry {
            pool: "implied closure",
            index: u32::MAX,
        }),
        _ => Err(EngineFault::UnsupportedDispatch { opcode }),
    }
}

const fn implied_index(opcode: FinalOpcode) -> Option<u32> {
    match opcode {
        FinalOpcode::GetLoc0
        | FinalOpcode::PutLoc0
        | FinalOpcode::SetLoc0
        | FinalOpcode::GetArg0
        | FinalOpcode::PutArg0
        | FinalOpcode::SetArg0
        | FinalOpcode::GetVarRef0
        | FinalOpcode::PutVarRef0
        | FinalOpcode::SetVarRef0 => Some(0),
        FinalOpcode::GetLoc1
        | FinalOpcode::PutLoc1
        | FinalOpcode::SetLoc1
        | FinalOpcode::GetArg1
        | FinalOpcode::PutArg1
        | FinalOpcode::SetArg1
        | FinalOpcode::GetVarRef1
        | FinalOpcode::PutVarRef1
        | FinalOpcode::SetVarRef1 => Some(1),
        FinalOpcode::GetLoc2
        | FinalOpcode::PutLoc2
        | FinalOpcode::SetLoc2
        | FinalOpcode::GetArg2
        | FinalOpcode::PutArg2
        | FinalOpcode::SetArg2
        | FinalOpcode::GetVarRef2
        | FinalOpcode::PutVarRef2
        | FinalOpcode::SetVarRef2 => Some(2),
        FinalOpcode::GetLoc3
        | FinalOpcode::PutLoc3
        | FinalOpcode::SetLoc3
        | FinalOpcode::GetArg3
        | FinalOpcode::PutArg3
        | FinalOpcode::SetArg3
        | FinalOpcode::GetVarRef3
        | FinalOpcode::PutVarRef3
        | FinalOpcode::SetVarRef3 => Some(3),
        _ => None,
    }
}

const fn implied_integer(opcode: FinalOpcode) -> Option<i32> {
    match opcode {
        FinalOpcode::PushMinus1 => Some(-1),
        FinalOpcode::Push0 => Some(0),
        FinalOpcode::Push1 => Some(1),
        FinalOpcode::Push2 => Some(2),
        FinalOpcode::Push3 => Some(3),
        FinalOpcode::Push4 => Some(4),
        FinalOpcode::Push5 => Some(5),
        FinalOpcode::Push6 => Some(6),
        FinalOpcode::Push7 => Some(7),
        _ => None,
    }
}

fn copy_ids(
    values: &[BindingCellId],
    resource: RuntimeResource,
) -> Result<Vec<BindingCellId>, ExecutionError> {
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(values.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource,
            additional: values.len(),
        })?;
    copied.extend_from_slice(values);
    Ok(copied)
}

fn copy_addresses(
    values: &[FrameBindingAddress],
) -> Result<Vec<FrameBindingAddress>, ExecutionError> {
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(values.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: values.len(),
        })?;
    copied.extend_from_slice(values);
    Ok(copied)
}

fn unsupported_dispatch<T>(opcode: FinalOpcode) -> Result<T, ExecutionError> {
    Err(EngineFault::UnsupportedDispatch { opcode }.into())
}
