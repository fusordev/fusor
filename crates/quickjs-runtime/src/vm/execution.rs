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

use super::async_function::allocate_async_function_settlement;
use super::instanceof::begin_instance_of;
use crate::AtomKind;

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
    supplied_argument_count: usize,
    construction: bool,
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
    let asynchronous = control_flow.function_header().kind() == FunctionKind::Async;
    let constructor_profile = if control_flow
        .function_header()
        .flags()
        .is_derived_class_constructor()
    {
        if control_flow.instructions().iter().any(|instruction| {
            instruction.decoded().instruction().opcode() == FinalOpcode::InitCtor
        }) {
            ConstructorProfile::DerivedDefault
        } else {
            ConstructorProfile::Derived
        }
    } else {
        ConstructorProfile::OrdinaryOrNone
    };
    let domains = control_flow.domains();
    let argument_count = domains.argument_count() as usize;
    let local_count = domains.local_count() as usize;
    let stack_capacity = control_flow.computed_stack_size() as usize;
    let has_arguments_object = control_flow.instructions().iter().any(|instruction| {
        matches!(
            (
                instruction.decoded().instruction().opcode(),
                instruction.decoded().instruction().operands()
            ),
            (FinalOpcode::SpecialObject, Operands::U8(0 | 1))
        )
    });
    let has_rest_parameter = control_flow
        .instructions()
        .iter()
        .any(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::Rest);
    let arguments_snapshot_use = match (has_arguments_object, has_rest_parameter) {
        (false, false) => ArgumentsSnapshotUse::None,
        (true, false) => ArgumentsSnapshotUse::ArgumentsObject,
        (false, true) => ArgumentsSnapshotUse::RestParameter,
        (true, true) => ArgumentsSnapshotUse::ArgumentsObjectAndRestParameter,
    };
    let frame_values = argument_count
        .checked_add(local_count)
        .and_then(|value| value.checked_add(stack_capacity))
        .and_then(|value| value.checked_add(1))
        .and_then(|value| value.checked_add(usize::from(construction)))
        .and_then(|value| value.checked_add(if asynchronous { 3 } else { 0 }))
        .and_then(|value| {
            value.checked_add(
                if arguments_snapshot_use.is_needed() || constructor_profile.is_derived_default() {
                    supplied_argument_count
                } else {
                    0
                },
            )
        })
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
        arguments_snapshot_use,
        construction,
        constructor_profile,
        strict,
        receiver_access,
        asynchronous,
        instruction,
    })
}

/// Implements the pinned `ToObject` conversion: objects and functions pass
/// through, primitives are boxed into their realm wrapper, and `null` or
/// `undefined` produce the exact `cannot convert to object` `TypeError`.
/// Returns the converted object or a pending exception.
pub(super) fn to_object_value(
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
        StoredValue::BigInt(value) => {
            let object = runtime.allocate_boxed_bigint(realm, value)?;
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

fn duplicate_arguments_snapshot(frame: &Frame) -> Result<Vec<StoredValue>, ExecutionError> {
    let snapshot = frame
        .arguments_snapshot
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "arguments object was initialized more than once",
        })?;
    let mut duplicate = Vec::new();
    duplicate
        .try_reserve_exact(snapshot.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: snapshot.len(),
        })?;
    duplicate.extend(snapshot.iter().map(StoredValue::duplicate));
    Ok(duplicate)
}

fn is_class_name_capture_write(
    runtime: &Runtime,
    frame: &Frame,
    index: u32,
) -> Result<bool, EngineFault> {
    let code = code(runtime, frame.code)?;
    let function =
        code.authority
            .function(frame.template)
            .ok_or(EngineFault::InvalidClosureEnvironment {
                function: frame.template,
            })?;
    let definition = function.metadata().closures().get(index as usize).ok_or(
        EngineFault::MissingPoolEntry {
            pool: "closure environment",
            index,
        },
    )?;
    Ok(matches!(
        definition.binding(),
        CompilerClosureBinding::Captured(policy)
            if policy.kind() == CompilerBindingKind::ClassName
    ))
}

#[allow(
    clippy::too_many_lines,
    reason = "failure-atomic frame allocation and initialization remain one transaction"
)]
pub(super) fn create_frame(
    runtime: &mut Runtime,
    plan: FramePlan,
    receiver: StoredValue,
    new_target: Option<FunctionId>,
    supplied: FrameArguments<'_>,
    return_to: Option<CallReturn>,
    dynamic_return: Option<DynamicFunctionReturn>,
) -> Result<Frame, ExecutionError> {
    if plan.construction != new_target.is_some() {
        return Err(EngineFault::RuntimeInvariant {
            message: "frame plan construction profile changed before allocation",
        }
        .into());
    }
    let function = runtime
        .functions
        .get(plan.function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "function",
            index: plan.function.index(),
            generation: plan.function.generation(),
        })?;
    let bytecode = function.bytecode()?;
    let environment = copy_environment(&bytecode.environment, RuntimeResource::FrameValues)?;
    let lexical_receiver = bytecode
        .lexical_receiver
        .as_ref()
        .map(StoredValue::duplicate);
    let lexical_new_target = bytecode.lexical_new_target;
    let code = runtime
        .code
        .get(plan.code)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "installed code",
            index: plan.code.index(),
            generation: plan.code.generation(),
        })?;
    let realm = code.realm;

    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(plan.argument_count)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: plan.argument_count,
        })?;
    let mut arguments_snapshot = None;
    let mut constructor_arguments = None;
    match supplied {
        FrameArguments::Public(supplied) if plan.constructor_profile.is_derived_default() => {
            if plan.arguments_snapshot_use.is_needed() {
                return Err(EngineFault::RuntimeInvariant {
                    message: "derived class constructor unexpectedly requested an arguments snapshot",
                }
                .into());
            }
            let mut forwarded = Vec::new();
            forwarded.try_reserve_exact(supplied.len()).map_err(|_| {
                ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: supplied.len(),
                }
            })?;
            for value in supplied {
                forwarded.push(value.stored()?.duplicate());
            }
            for index in 0..plan.argument_count {
                let value = forwarded
                    .get(index)
                    .map_or(StoredValue::Undefined, StoredValue::duplicate);
                arguments.push(FrameBinding::Direct(SlotValue::Value(value)));
            }
            constructor_arguments = Some(forwarded);
        }
        FrameArguments::Owned(supplied) if plan.constructor_profile.is_derived_default() => {
            if plan.arguments_snapshot_use.is_needed() {
                return Err(EngineFault::RuntimeInvariant {
                    message: "derived class constructor unexpectedly requested an arguments snapshot",
                }
                .into());
            }
            let forwarded = supplied.into_remaining_values();
            for index in 0..plan.argument_count {
                let value = forwarded
                    .get(index)
                    .map_or(StoredValue::Undefined, StoredValue::duplicate);
                arguments.push(FrameBinding::Direct(SlotValue::Value(value)));
            }
            constructor_arguments = Some(forwarded);
        }
        FrameArguments::Public(supplied) if plan.arguments_snapshot_use.is_needed() => {
            let mut snapshot = Vec::new();
            snapshot.try_reserve_exact(supplied.len()).map_err(|_| {
                ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: supplied.len(),
                }
            })?;
            for value in supplied {
                snapshot.push(value.stored()?.duplicate());
            }
            for index in 0..plan.argument_count {
                let value = snapshot
                    .get(index)
                    .map_or(StoredValue::Undefined, StoredValue::duplicate);
                arguments.push(FrameBinding::Direct(SlotValue::Value(value)));
            }
            arguments_snapshot = Some(snapshot);
        }
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
        FrameArguments::Owned(supplied) if plan.arguments_snapshot_use.is_needed() => {
            let snapshot = supplied.into_remaining_values();
            for index in 0..plan.argument_count {
                let value = snapshot
                    .get(index)
                    .map_or(StoredValue::Undefined, StoredValue::duplicate);
                arguments.push(FrameBinding::Direct(SlotValue::Value(value)));
            }
            arguments_snapshot = Some(snapshot);
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

    let (receiver, new_target) = if matches!(plan.receiver_access, ReceiverAccess::Lexical) {
        if plan.construction || new_target.is_some() {
            return Err(EngineFault::RuntimeInvariant {
                message: "lexical arrow entered a construction frame",
            }
            .into());
        }
        let receiver = lexical_receiver.ok_or(EngineFault::InvalidClosureEnvironment {
            function: plan.template,
        })?;
        (receiver, lexical_new_target)
    } else {
        if lexical_receiver.is_some() || lexical_new_target.is_some() {
            return Err(EngineFault::InvalidClosureEnvironment {
                function: plan.template,
            }
            .into());
        }
        (
            normalize_receiver(runtime, realm, plan.receiver_access, receiver)?,
            new_target,
        )
    };

    let mut native_returns = Vec::new();
    if plan.asynchronous {
        native_returns
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::Frames,
                additional: 1,
            })?;
        native_returns.push(allocate_async_function_settlement(runtime, realm)?);
    }

    Ok(Frame {
        function: plan.function,
        code: plan.code,
        template: plan.template,
        strict: plan.strict,
        receiver,
        new_target,
        instruction: plan.instruction,
        return_to,
        dynamic_return,
        native_returns,
        transient_cleanup_pending: false,
        constructor_state: if plan.construction && plan.constructor_profile.is_derived() {
            ConstructorState::DerivedUninitialized
        } else {
            ConstructorState::NonConstructor
        },
        native_caller: None,
        generator_resume: None,
        generator_result: None,
        resume_abrupt: None,
        reserved_values: plan.reserved_values,
        arguments_snapshot_use: plan.arguments_snapshot_use,
        arguments_snapshot,
        constructor_arguments,
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
        FinalOpcode::PushBigIntI32 => {
            // The compiler emits this for a `BigInt` literal whose value fits
            // `i32`, mirroring upstream's short-bigint fast path
            // (`quickjs.c:26733-26737`).
            let Operands::I32(value) = operands else {
                return unsupported_dispatch(opcode);
            };
            push(
                frame,
                StoredValue::BigInt(Arc::new(JsBigInt::from_i32(value))),
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
        FinalOpcode::PrivateSymbol => {
            let Operands::Atom(index) = operands else {
                return unsupported_dispatch(opcode);
            };
            let description = {
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
            push(
                frame,
                StoredValue::Symbol(runtime.new_private_name(&description)?),
            );
        }
        FinalOpcode::PushEmptyString => {
            push(frame, StoredValue::String(JsString::empty()));
        }
        FinalOpcode::Undefined => push(frame, StoredValue::Undefined),
        FinalOpcode::Null => push(frame, StoredValue::Null),
        FinalOpcode::PushThis => {
            if frame.constructor_state == ConstructorState::DerivedUninitialized {
                return Ok(Step::Abrupt(derived_this_uninitialized_exception(
                    runtime, frame, source_pc,
                )?));
            }
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
        FinalOpcode::Rest => {
            let Operands::U16(first_argument) = operands else {
                return unsupported_dispatch(opcode);
            };
            let first_argument = usize::from(first_argument);
            let argument_count = frame
                .arguments_snapshot
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "rest parameter was initialized more than once",
                })?
                .len();
            let first_argument = first_argument.min(argument_count);
            let element_count = argument_count - first_argument;
            execution_budget.charge_instructions(usize_to_u64(element_count))?;
            let mut elements =
                frame
                    .arguments_snapshot
                    .take()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "rest parameter was initialized more than once",
                    })?;
            elements.drain(..first_argument);
            let realm = code(runtime, frame.code)?.realm;
            let array = runtime.allocate_array(realm, elements)?;
            push(frame, StoredValue::Object(array));
        }
        FinalOpcode::SpecialObject => {
            let Operands::U8(selector) = operands else {
                return unsupported_dispatch(opcode);
            };
            match selector {
                3 => push(
                    frame,
                    frame
                        .new_target
                        .map_or(StoredValue::Undefined, StoredValue::Function),
                ),
                4 => {
                    if frame.constructor_state != ConstructorState::DerivedUninitialized {
                        return Ok(Step::Abrupt(derived_this_uninitialized_exception(
                            runtime, frame, source_pc,
                        )?));
                    }
                    push(frame, StoredValue::Function(frame.function));
                }
                5 => {
                    let function = runtime.functions.get(frame.function).ok_or(
                        EngineFault::StaleHeapEdge {
                            edge: "active function home object",
                            index: frame.function.index(),
                            generation: frame.function.generation(),
                        },
                    )?;
                    let home_object =
                        function
                            .bytecode()?
                            .home_object
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "verified super property access has no method home object",
                            })?;
                    push(
                        frame,
                        match home_object {
                            HeapReference::Function(function) => StoredValue::Function(function),
                            HeapReference::Object(object) => StoredValue::Object(object),
                        },
                    );
                }
                arguments_kind @ (0 | 1) => {
                    let arguments = if frame.arguments_snapshot_use.has_rest_parameter() {
                        let argument_count = frame
                            .arguments_snapshot
                            .as_ref()
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "arguments object was initialized more than once",
                            })?
                            .len();
                        execution_budget.charge_instructions(usize_to_u64(argument_count))?;
                        duplicate_arguments_snapshot(frame)?
                    } else {
                        frame
                            .arguments_snapshot
                            .take()
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "arguments object was initialized more than once",
                            })?
                    };
                    let realm = code(runtime, frame.code)?.realm;
                    let object = if arguments_kind == 0 {
                        runtime.allocate_unmapped_arguments_object(realm, arguments)?
                    } else {
                        let mapped_arguments = mapped_arguments_authority(runtime, frame)?;
                        let supplied_count = arguments.len();
                        let active_count = mapped_arguments
                            .partition_point(|index| (*index as usize) < supplied_count);
                        let active_arguments = &mapped_arguments[..active_count];
                        preflight_mapped_arguments_frame(frame, active_arguments)?;
                        let object = runtime.allocate_mapped_arguments_object(
                            realm,
                            frame.function,
                            arguments,
                            &mapped_arguments,
                        )?;
                        install_mapped_arguments_cells(runtime, frame, object, active_arguments)?;
                        object
                    };
                    push(frame, StoredValue::Object(object));
                }
                _ => return unsupported_dispatch(opcode),
            }
        }
        FinalOpcode::Object => {
            let realm = code(runtime, frame.code)?.realm;
            let prototype = runtime.realm_object_prototype(realm)?;
            let object = runtime.allocate_ordinary_object(prototype)?;
            push(frame, StoredValue::Object(object));
        }
        FinalOpcode::RegExp => {
            let flags = pop(frame)?;
            let pattern = pop(frame)?;
            let (StoredValue::String(pattern), StoredValue::String(flags)) = (pattern, flags)
            else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "verified regexp operands are strings",
                }
                .into());
            };
            let realm = code(runtime, frame.code)?.realm;
            let origin = instruction_location(runtime, frame, source_pc)?;
            let pattern_len =
                usize::try_from(pattern.len()).map_err(|_| EngineFault::RuntimeInvariant {
                    message: "RegExp literal pattern length exceeded usize",
                })?;
            let flags_len =
                usize::try_from(flags.len()).map_err(|_| EngineFault::RuntimeInvariant {
                    message: "RegExp literal flags length exceeded usize",
                })?;
            let mut pattern_units = Vec::new();
            pattern_units.try_reserve_exact(pattern_len).map_err(|_| {
                ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: pattern_len,
                }
            })?;
            pattern_units.extend(pattern.code_units());
            let mut flags_units = Vec::new();
            flags_units.try_reserve_exact(flags_len).map_err(|_| {
                ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: flags_len,
                }
            })?;
            flags_units.extend(flags.code_units());
            let matcher = match quickjs_regexp::CompiledRegExp::compile_utf16(
                &pattern_units,
                &flags_units,
                quickjs_regexp::CompileLimits::default(),
            ) {
                Ok(matcher) => matcher,
                Err(error) => {
                    return Ok(Step::Abrupt(PendingException {
                        realm,
                        payload: PendingExceptionPayload::EngineError {
                            kind: ExceptionKind::SyntaxError,
                            message: JsString::from_utf8(&error.to_string())?,
                        },
                        origin,
                    }));
                }
            };
            let prototype = runtime.realm_regexp_prototype(realm)?;
            let object = runtime.allocate_regexp_object(
                HeapReference::Object(prototype),
                pattern,
                flags,
                matcher,
            )?;
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
        FinalOpcode::Dup2 => {
            let right = pop(frame)?;
            let left = pop(frame)?;
            push(frame, left.duplicate());
            push(frame, right.duplicate());
            push(frame, left);
            push(frame, right);
        }
        FinalOpcode::Dup3 => {
            let third = pop(frame)?;
            let second = pop(frame)?;
            let first = pop(frame)?;
            push(frame, first.duplicate());
            push(frame, second.duplicate());
            push(frame, third.duplicate());
            push(frame, first);
            push(frame, second);
            push(frame, third);
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
        FinalOpcode::Insert4 => {
            let fourth = pop(frame)?;
            let third = pop(frame)?;
            let second = pop(frame)?;
            let first = pop(frame)?;
            push(frame, fourth.duplicate());
            push(frame, first);
            push(frame, second);
            push(frame, third);
            push(frame, fourth);
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
        FinalOpcode::Perm5 => {
            let fifth = pop(frame)?;
            let fourth = pop(frame)?;
            let third = pop(frame)?;
            let second = pop(frame)?;
            let first = pop(frame)?;
            push(frame, fourth);
            push(frame, first);
            push(frame, second);
            push(frame, third);
            push(frame, fifth);
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
        FinalOpcode::GetSuper => {
            let value = pop(frame)?;
            let Some(reference) = value.heap_reference() else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "verified get_super did not receive a home object",
                }
                .into());
            };
            if matches!(value, StoredValue::Function(function) if function == frame.function)
                && frame.constructor_state != ConstructorState::DerivedUninitialized
            {
                return Err(EngineFault::RuntimeInvariant {
                    message: "derived get_super escaped its uninitialized constructor frame",
                }
                .into());
            }
            let superclass = runtime.object_record(reference)?.prototype();
            push(
                frame,
                superclass.map_or(StoredValue::Null, |reference| match reference {
                    HeapReference::Function(function) => StoredValue::Function(function),
                    HeapReference::Object(object) => StoredValue::Object(object),
                }),
            );
        }
        FinalOpcode::GetSuperValue => {
            let realm = code(runtime, frame.code)?.realm;
            let key = pop(frame)?;
            let base = pop(frame)?;
            let receiver = pop(frame)?;
            let property = computed_property_operand(runtime, &key)?;
            let Some(reference) = base.heap_reference() else {
                let failure = if matches!(base, StoredValue::Null) {
                    PropertyFailure::ReadNull
                } else {
                    PropertyFailure::ReadUndefined
                };
                return Ok(Step::Abrupt(property_exception_at(
                    realm,
                    instruction_location(runtime, frame, source_pc)?,
                    Some(&property.name),
                    failure,
                )?));
            };
            let return_to =
                CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_internal_get(
                    runtime,
                    reference,
                    receiver,
                    property.key,
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::PutSuperValue => {
            let realm = code(runtime, frame.code)?.realm;
            let value = pop(frame)?;
            let key = pop(frame)?;
            let base = pop(frame)?;
            let receiver = pop(frame)?;
            let property = computed_property_operand(runtime, &key)?;
            let Some(reference) = base.heap_reference() else {
                let failure = if matches!(base, StoredValue::Null) {
                    PropertyFailure::WriteNull
                } else {
                    PropertyFailure::WriteUndefined
                };
                return Ok(Step::Abrupt(property_exception_at(
                    realm,
                    instruction_location(runtime, frame, source_pc)?,
                    Some(&property.name),
                    failure,
                )?));
            };
            let return_to =
                CallReturn::discard(verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?);
            let origin = instruction_location(runtime, frame, source_pc)?;
            return native_step(
                begin_internal_set(
                    runtime,
                    reference,
                    property.key,
                    property.name,
                    value,
                    receiver,
                    frame.strict,
                    false,
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::CheckCtorReturn => {
            if frame.constructor_state != ConstructorState::DerivedUninitialized {
                return Err(EngineFault::RuntimeInvariant {
                    message: "derived super completion reached an initialized constructor frame",
                }
                .into());
            }
            let value = peek(frame)?.duplicate();
            if value.heap_reference().is_none() {
                return Err(EngineFault::RuntimeInvariant {
                    message: "verified superclass construction completed without an object receiver",
                }
                .into());
            }
            frame.receiver = value;
            frame.constructor_state = ConstructorState::DerivedInitialized;
            push(frame, StoredValue::Boolean(false));
        }
        FinalOpcode::CheckCtor => {
            if frame.stack.is_empty() {
                if frame.constructor_state != ConstructorState::DerivedUninitialized
                    || frame.new_target.is_none()
                {
                    return Ok(Step::Abrupt(not_constructor_exception(
                        runtime, frame, source_pc,
                    )?));
                }
            } else {
                let StoredValue::Function(function) = peek(frame)? else {
                    return Ok(Step::Abrupt(not_constructor_exception(
                        runtime, frame, source_pc,
                    )?));
                };
                if !function_is_constructor(runtime, *function)? {
                    return Ok(Step::Abrupt(not_constructor_exception(
                        runtime, frame, source_pc,
                    )?));
                }
            }
        }
        FinalOpcode::InitCtor => {
            if frame.constructor_state != ConstructorState::DerivedUninitialized {
                return Err(EngineFault::RuntimeInvariant {
                    message: "init_ctor reached a non-default derived constructor frame",
                }
                .into());
            }
            let new_target = frame.new_target.ok_or(EngineFault::RuntimeInvariant {
                message: "derived constructor has no new.target",
            })?;
            let superclass = runtime
                .object_record(HeapReference::Function(frame.function))?
                .prototype();
            let Some(HeapReference::Function(superclass)) = superclass else {
                return Ok(Step::Abrupt(not_constructor_exception(
                    runtime, frame, source_pc,
                )?));
            };
            let arguments = frame.constructor_arguments.take().ok_or(
                EngineFault::RuntimeInvariant {
                    message: "derived default constructor forwarded its arguments more than once",
                },
            )?;
            let return_to = CallReturn::initialize_derived_this(
                verified_instruction.successors().fallthrough().ok_or(
                    EngineFault::InvalidSuccessor {
                        function: frame.template,
                        pc: source_pc,
                    },
                )?,
            );
            return Ok(Step::Call {
                function: superclass,
                inputs: CallInputSource::Prepared(CallInputs {
                    receiver: StoredValue::Undefined,
                    arguments: CallArguments::from_values(arguments),
                    new_target: Some(new_target),
                }),
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
            let function = match *stack_value_at(frame, callee_index)? {
                StoredValue::Function(function) => function,
                _ if magic == 2 => {
                    return Ok(Step::Abrupt(not_constructor_exception(
                        runtime, frame, source_pc,
                    )?));
                }
                _ => {
                    return Ok(Step::Abrupt(not_callable_exception(
                        runtime, frame, source_pc,
                    )?));
                }
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
        FinalOpcode::SetName => {
            let name = static_property_operand(runtime, frame, operands)?.name;
            let function = match peek(frame)? {
                StoredValue::Function(function) => *function,
                StoredValue::Undefined
                | StoredValue::Null
                | StoredValue::Boolean(_)
                | StoredValue::Number(_)
                | StoredValue::BigInt(_)
                | StoredValue::String(_)
                | StoredValue::Symbol(_)
                | StoredValue::Object(_) => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "verified set_name operand is not a function",
                    }
                    .into());
                }
            };
            set_inferred_function_name(runtime, function, name)?;
        }
        FinalOpcode::SetNameComputed => {
            let function_index =
                frame
                    .stack
                    .len()
                    .checked_sub(1)
                    .ok_or(EngineFault::StackDepthMismatch {
                        function: frame.template,
                        pc: source_pc,
                        expected: 2,
                        actual: frame.stack.len(),
                    })?;
            let key_index =
                function_index
                    .checked_sub(1)
                    .ok_or(EngineFault::StackDepthMismatch {
                        function: frame.template,
                        pc: source_pc,
                        expected: 2,
                        actual: frame.stack.len(),
                    })?;
            let function = match stack_value_at(frame, function_index)? {
                StoredValue::Function(function) => *function,
                StoredValue::Undefined
                | StoredValue::Null
                | StoredValue::Boolean(_)
                | StoredValue::Number(_)
                | StoredValue::BigInt(_)
                | StoredValue::String(_)
                | StoredValue::Symbol(_)
                | StoredValue::Object(_) => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "verified set_name_computed operand is not a function",
                    }
                    .into());
                }
            };
            let name = computed_function_name(stack_value_at(frame, key_index)?)?;
            set_inferred_function_name(runtime, function, name)?;
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
                | StoredValue::BigInt(_)
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
        FinalOpcode::Delete => {
            let realm = code(runtime, frame.code)?.realm;
            let key = pop(frame)?;
            let base = pop(frame)?;
            let origin = instruction_location(runtime, frame, source_pc)?;
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
                    PropertyKeyTarget::Delete {
                        base,
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
        FinalOpcode::DeleteVar => {
            let global = delete_global_reference_operand(runtime, frame, operands)?;
            let deleted = delete_realm_global_binding(runtime, &global)?;
            push(frame, StoredValue::Boolean(deleted));
        }
        FinalOpcode::SetProto => {
            // `OP_set_proto` keeps the target on the stack and consumes the
            // requested prototype. Only an object or `null` takes effect; any
            // other value is ignored (`quickjs.c:19330-19341`).
            let requested = pop(frame)?;
            let target = peek(frame)?.duplicate();
            let prototype = match requested {
                StoredValue::Null => Some(None),
                StoredValue::Function(function) => Some(Some(HeapReference::Function(function))),
                StoredValue::Object(object) => Some(Some(HeapReference::Object(object))),
                StoredValue::Undefined
                | StoredValue::Boolean(_)
                | StoredValue::Number(_)
                | StoredValue::BigInt(_)
                | StoredValue::String(_)
                | StoredValue::Symbol(_) => None,
            };
            if let Some(prototype) = prototype {
                let reference = match target {
                    StoredValue::Function(function) => HeapReference::Function(function),
                    StoredValue::Object(object) => HeapReference::Object(object),
                    StoredValue::Undefined
                    | StoredValue::Null
                    | StoredValue::Boolean(_)
                    | StoredValue::Number(_)
                    | StoredValue::BigInt(_)
                    | StoredValue::String(_)
                    | StoredValue::Symbol(_) => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "set_proto received a non-object literal target",
                        }
                        .into());
                    }
                };
                let realm = code(runtime, frame.code)?.realm;
                let origin = instruction_location(runtime, frame, source_pc)?;
                match runtime.set_prototype_of(reference, prototype)? {
                    SetPrototypeOutcome::Complete => {}
                    SetPrototypeOutcome::NonExtensible => {
                        return Ok(Step::Abrupt(PendingException {
                            realm,
                            payload: PendingExceptionPayload::EngineError {
                                kind: ExceptionKind::TypeError,
                                message: JsString::from_utf8("object is not extensible")?,
                            },
                            origin,
                        }));
                    }
                    SetPrototypeOutcome::CyclicPrototype => {
                        return Ok(Step::Abrupt(PendingException {
                            realm,
                            payload: PendingExceptionPayload::EngineError {
                                kind: ExceptionKind::TypeError,
                                message: JsString::from_utf8("circular prototype chain")?,
                            },
                            origin,
                        }));
                    }
                }
            }
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
                | StoredValue::BigInt(_)
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
            if let Some(reference) = base.heap_reference()
                && runtime.proxy_state(reference)?.is_some()
            {
                let return_to =
                    CallReturn::push(verified_instruction.successors().fallthrough().ok_or(
                        EngineFault::InvalidSuccessor {
                            function: frame.template,
                            pc: source_pc,
                        },
                    )?);
                let origin = instruction_location(runtime, frame, source_pc)?;
                return native_step(
                    begin_internal_get(
                        runtime,
                        reference,
                        base,
                        property.key,
                        realm,
                        Some(return_to),
                        origin,
                        execution_budget,
                    ),
                    return_to,
                );
            }
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
        FinalOpcode::GetPrivateField => {
            let private_name = pop(frame)?;
            let base = pop(frame)?;
            let Some(key) = private_field_key(private_name) else {
                return Ok(Step::Abrupt(private_field_exception(
                    runtime,
                    frame,
                    source_pc,
                    "invalid private field name",
                )?));
            };
            let Some(reference) = base.heap_reference() else {
                return Ok(Step::Abrupt(private_field_exception(
                    runtime,
                    frame,
                    source_pc,
                    "private field access requires an object receiver",
                )?));
            };
            let Some(value) = runtime.private_own_data_property(reference, &key)? else {
                return Ok(Step::Abrupt(private_field_exception(
                    runtime,
                    frame,
                    source_pc,
                    "private field is not present on this object",
                )?));
            };
            push(frame, value);
        }
        FinalOpcode::PutField => {
            let realm = code(runtime, frame.code)?.realm;
            let property = static_property_operand(runtime, frame, operands)?;
            let value = pop(frame)?;
            let base = pop(frame)?;
            if let Some(reference) = base.heap_reference()
                && runtime.proxy_state(reference)?.is_some()
            {
                let return_to =
                    CallReturn::discard(verified_instruction.successors().fallthrough().ok_or(
                        EngineFault::InvalidSuccessor {
                            function: frame.template,
                            pc: source_pc,
                        },
                    )?);
                let origin = instruction_location(runtime, frame, source_pc)?;
                return native_step(
                    begin_internal_set(
                        runtime,
                        reference,
                        property.key,
                        property.name,
                        value,
                        base,
                        frame.strict,
                        false,
                        realm,
                        Some(return_to),
                        origin,
                        execution_budget,
                    ),
                    return_to,
                );
            }
            if is_array_length_target(runtime, &base, &property.key)? {
                let return_to =
                    CallReturn::discard(verified_instruction.successors().fallthrough().ok_or(
                        EngineFault::InvalidSuccessor {
                            function: frame.template,
                            pc: source_pc,
                        },
                    )?);
                let origin = instruction_location(runtime, frame, source_pc)?;
                let target =
                    array_length_write_target(base, property.name, frame.strict, false, &value);
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
        FinalOpcode::PutPrivateField => {
            let value = pop(frame)?;
            let private_name = pop(frame)?;
            let base = pop(frame)?;
            let Some(key) = private_field_key(private_name) else {
                return Ok(Step::Abrupt(private_field_exception(
                    runtime,
                    frame,
                    source_pc,
                    "invalid private field name",
                )?));
            };
            let Some(reference) = base.heap_reference() else {
                return Ok(Step::Abrupt(private_field_exception(
                    runtime,
                    frame,
                    source_pc,
                    "private field access requires an object receiver",
                )?));
            };
            if !runtime.replace_private_own_data_property(reference, &key, value)? {
                return Ok(Step::Abrupt(private_field_exception(
                    runtime,
                    frame,
                    source_pc,
                    "private field is not present on this object",
                )?));
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
        FinalOpcode::DefinePrivateField => {
            let value = pop(frame)?;
            let private_name = pop(frame)?;
            let base = peek(frame)?.duplicate();
            let Some(key) = private_field_key(private_name) else {
                return Ok(Step::Abrupt(private_field_exception(
                    runtime,
                    frame,
                    source_pc,
                    "invalid private field name",
                )?));
            };
            let Some(reference) = base.heap_reference() else {
                return Ok(Step::Abrupt(private_field_exception(
                    runtime,
                    frame,
                    source_pc,
                    "private field initialization requires an object receiver",
                )?));
            };
            if runtime
                .private_own_data_property(reference, &key)?
                .is_some()
            {
                return Ok(Step::Abrupt(private_field_exception(
                    runtime,
                    frame,
                    source_pc,
                    "private field is already initialized on this object",
                )?));
            }
            runtime.append_data_property(
                reference,
                key,
                PropertyLayout::data(true, false, false),
                value,
            )?;
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
        FinalOpcode::DefineClass => {
            let class = define_class_operand(runtime, frame, operands)?;
            let constructor = pop(frame)?;
            let StoredValue::Function(constructor) = constructor else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "verified define_class did not receive a closure",
                }
                .into());
            };
            let (constructor_parent, prototype_parent) = if class.has_heritage {
                let prototype_parent = match pop(frame)? {
                    StoredValue::Function(function) => Some(HeapReference::Function(function)),
                    StoredValue::Object(object) => Some(HeapReference::Object(object)),
                    StoredValue::Null => None,
                    StoredValue::Undefined
                    | StoredValue::Boolean(_)
                    | StoredValue::Number(_)
                    | StoredValue::BigInt(_)
                    | StoredValue::String(_)
                    | StoredValue::Symbol(_) => {
                        let realm = code(runtime, frame.code)?.realm;
                        return Ok(Step::Abrupt(PendingException {
                            realm,
                            payload: PendingExceptionPayload::EngineError {
                                kind: ExceptionKind::TypeError,
                                message: JsString::from_utf8(
                                    "class extends value has a non-object prototype property",
                                )?,
                            },
                            origin: instruction_location(runtime, frame, source_pc)?,
                        }));
                    }
                };
                let superclass = pop(frame)?;
                let constructor_parent = match superclass {
                    StoredValue::Function(function) => HeapReference::Function(function),
                    StoredValue::Null => HeapReference::Function(
                        runtime.realm_function_prototype(code(runtime, frame.code)?.realm)?,
                    ),
                    StoredValue::Undefined
                    | StoredValue::Object(_)
                    | StoredValue::Boolean(_)
                    | StoredValue::Number(_)
                    | StoredValue::BigInt(_)
                    | StoredValue::String(_)
                    | StoredValue::Symbol(_) => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "verified derived define_class did not receive a constructor or null superclass",
                        }
                        .into());
                    }
                };
                (constructor_parent, prototype_parent)
            } else {
                let superclass = pop(frame)?;
                if !matches!(superclass, StoredValue::Undefined) {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "base define_class did not receive undefined superclass",
                    }
                    .into());
                }
                let realm = code(runtime, frame.code)?.realm;
                (
                    HeapReference::Function(runtime.realm_function_prototype(realm)?),
                    Some(HeapReference::Object(
                        runtime.realm_object_prototype(realm)?,
                    )),
                )
            };
            let prototype = define_class(
                runtime,
                constructor,
                class.property.name,
                constructor_parent,
                prototype_parent,
            )?;
            push(frame, StoredValue::Function(constructor));
            push(frame, StoredValue::Object(prototype));
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
                RealmGlobalReadOutcome::Getter { function, receiver } => {
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
                RealmGlobalReadOutcome::Uninitialized => {
                    return Ok(Step::Abrupt(tdz_exception(
                        runtime,
                        frame,
                        BindingName::Closure(index),
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
                RealmGlobalWriteOutcome::Setter {
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
                RealmGlobalWriteOutcome::Missing => {
                    return Ok(Step::Abrupt(global_not_defined_exception(
                        runtime, frame, &name, source_pc,
                    )?));
                }
                RealmGlobalWriteOutcome::Uninitialized => {
                    return Ok(Step::Abrupt(tdz_exception(
                        runtime,
                        frame,
                        BindingName::Closure(index),
                        source_pc,
                    )?));
                }
                RealmGlobalWriteOutcome::Immutable => {
                    return Ok(Step::Abrupt(immutable_binding_exception(
                        runtime,
                        frame,
                        BindingName::Closure(index),
                        source_pc,
                    )?));
                }
                RealmGlobalWriteOutcome::Property(failure) => {
                    return Ok(Step::Abrupt(property_exception(
                        runtime, frame, source_pc, &name, failure,
                    )?));
                }
            }
        }
        FinalOpcode::PutVarInit => {
            let index = closure_index(opcode, operands)?;
            let global = global_reference_operand(runtime, frame, index)?;
            let value = pop(frame)?;
            initialize_realm_global(runtime, &global, value)?;
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
            if is_class_name_capture_write(runtime, frame, index)? {
                return Ok(Step::Abrupt(immutable_binding_exception(
                    runtime,
                    frame,
                    BindingName::Closure(index),
                    source_pc,
                )?));
            }
            let value = pop(frame)?;
            write_environment(runtime, frame, index, SlotValue::Value(value))?;
        }
        FinalOpcode::SetVarRef
        | FinalOpcode::SetVarRef0
        | FinalOpcode::SetVarRef1
        | FinalOpcode::SetVarRef2
        | FinalOpcode::SetVarRef3 => {
            let index = closure_index(opcode, operands)?;
            if is_class_name_capture_write(runtime, frame, index)? {
                return Ok(Step::Abrupt(immutable_binding_exception(
                    runtime,
                    frame,
                    BindingName::Closure(index),
                    source_pc,
                )?));
            }
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
            if is_class_name_capture_write(runtime, frame, index)? {
                return Ok(Step::Abrupt(immutable_binding_exception(
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
            let value = pop(frame)?;
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
                begin_for_in_start(
                    runtime,
                    value,
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
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
                        | StoredValue::BigInt(_)
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
                begin_for_in_next(
                    runtime,
                    iterator,
                    realm,
                    Some(return_to),
                    &origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::ForOfStart | FinalOpcode::ForAwaitOfStart => {
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
            let dispatch = if opcode == FinalOpcode::ForAwaitOfStart {
                begin_for_await_of_start(
                    runtime,
                    iterable,
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                )
            } else {
                begin_for_of_start(
                    runtime,
                    iterable,
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                )
            };
            return native_step(dispatch, return_to);
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
        FinalOpcode::IteratorNext => {
            let input = pop(frame)?;
            let base = frame
                .stack
                .len()
                .checked_sub(3)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "verified yield-star next has an incomplete iterator record",
                })?;
            let iterator = stack_value_at(frame, base)?.duplicate();
            let next = stack_value_at(frame, base + 1)?.duplicate();
            let StoredValue::Function(function) = next else {
                let realm = code(runtime, frame.code)?.realm;
                return Ok(Step::Abrupt(PendingException {
                    realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::TypeError,
                        message: JsString::from_utf8("not a function")?,
                    },
                    origin: instruction_location(runtime, frame, source_pc)?,
                }));
            };
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
                    receiver: iterator,
                    arguments: CallArguments::from_values(vec![input]),
                    new_target: None,
                }),
                return_to,
                source_pc,
            });
        }
        FinalOpcode::IteratorCall => {
            let Operands::U8(flags) = operands else {
                return unsupported_dispatch(opcode);
            };
            let input = pop(frame)?;
            let base = frame
                .stack
                .len()
                .checked_sub(3)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "verified yield-star method call has an incomplete iterator record",
                })?;
            let iterator = stack_value_at(frame, base)?.duplicate();
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
                begin_yield_star_iterator_call(
                    runtime,
                    iterator,
                    input,
                    flags,
                    realm,
                    Some(return_to),
                    origin,
                    execution_budget,
                ),
                return_to,
            );
        }
        FinalOpcode::IteratorCheckObject => {
            if !matches!(
                peek(frame)?,
                StoredValue::Function(_) | StoredValue::Object(_)
            ) {
                let realm = code(runtime, frame.code)?.realm;
                return Ok(Step::Abrupt(PendingException {
                    realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::TypeError,
                        message: JsString::from_utf8("iterator must return an object")?,
                    },
                    origin: instruction_location(runtime, frame, source_pc)?,
                }));
            }
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
        FinalOpcode::In => {
            let realm = code(runtime, frame.code)?.realm;
            let right = pop(frame)?;
            if right.heap_reference().is_none() {
                let origin = instruction_location(runtime, frame, source_pc)?;
                return Ok(Step::Abrupt(PendingException {
                    realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::TypeError,
                        message: JsString::from_utf8("right operand of 'in' is not an object")?,
                    },
                    origin,
                }));
            }
            let left = pop(frame)?;
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
                    left,
                    PropertyKeyTarget::In {
                        target: right,
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
                StoredValue::BigInt(_) => "bigint",
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
        FinalOpcode::IsNull => {
            let value = pop(frame)?;
            push(
                frame,
                StoredValue::Boolean(matches!(value, StoredValue::Null)),
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
        FinalOpcode::ThrowError => {
            let Operands::AtomU8 { value: 4, .. } = operands else {
                return unsupported_dispatch(opcode);
            };
            let realm = code(runtime, frame.code)?.realm;
            return Ok(Step::Abrupt(PendingException {
                realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::TypeError,
                    message: JsString::from_utf8("iterator does not have a throw method")?,
                },
                origin: instruction_location(runtime, frame, source_pc)?,
            }));
        }
        FinalOpcode::InitialYield => {
            return Ok(Step::InitialYield);
        }
        FinalOpcode::Yield => {
            frame.instruction = verified_instruction.successors().fallthrough().ok_or(
                EngineFault::InvalidSuccessor {
                    function: frame.template,
                    pc: source_pc,
                },
            )?;
            return Ok(Step::Yield(pop(frame)?));
        }
        FinalOpcode::YieldStar => {
            frame.instruction = verified_instruction.successors().fallthrough().ok_or(
                EngineFault::InvalidSuccessor {
                    function: frame.template,
                    pc: source_pc,
                },
            )?;
            return Ok(Step::YieldStar(pop(frame)?));
        }
        FinalOpcode::AsyncYieldStar => {
            frame.instruction = verified_instruction.successors().fallthrough().ok_or(
                EngineFault::InvalidSuccessor {
                    function: frame.template,
                    pc: source_pc,
                },
            )?;
            return Ok(Step::AsyncYieldStar(pop(frame)?));
        }
        FinalOpcode::Await => {
            frame.instruction = verified_instruction.successors().fallthrough().ok_or(
                EngineFault::InvalidSuccessor {
                    function: frame.template,
                    pc: source_pc,
                },
            )?;
            return Ok(Step::Await {
                value: pop(frame)?,
                source_pc,
            });
        }
        FinalOpcode::Return | FinalOpcode::ReturnAsync => {
            return Ok(Step::Return {
                value: pop(frame)?,
                source_pc,
            });
        }
        FinalOpcode::ReturnUndef => {
            return Ok(Step::Return {
                value: StoredValue::Undefined,
                source_pc,
            });
        }
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

/// Converts the VM-only private-symbol value into the backing key used by the
/// object's internal slots. Public `ToPropertyKey` never reaches this path.
fn private_field_key(value: StoredValue) -> Option<PropertyKey> {
    let StoredValue::Symbol(atom) = value else {
        return None;
    };
    (atom.kind() == AtomKind::Private).then(|| PropertyKey::from_private_atom(atom))
}

fn mapped_arguments_authority(runtime: &Runtime, frame: &Frame) -> Result<Arc<[u32]>, EngineFault> {
    let template = usize::try_from(frame.template.get()).map_err(|_| {
        EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        }
    })?;
    code(runtime, frame.code)?
        .templates
        .get(template)
        .and_then(|template| template.mapped_arguments.as_ref())
        .cloned()
        .ok_or(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        })
}

fn preflight_mapped_arguments_frame(
    frame: &Frame,
    active_arguments: &[u32],
) -> Result<(), EngineFault> {
    for &index in active_arguments {
        if !matches!(
            frame.arguments.get(index as usize),
            Some(FrameBinding::Direct(SlotValue::Value(_)))
        ) {
            return Err(EngineFault::RuntimeInvariant {
                message: "mapped arguments are promoted before executable body entry",
            });
        }
    }
    for (own_index, address) in frame.own_cell_bindings.iter().enumerate() {
        if matches!(address, FrameBindingAddress::Argument(index) if active_arguments.binary_search(index).is_ok())
            && frame.own_cells.get(own_index).copied().flatten().is_some()
        {
            return Err(EngineFault::RuntimeInvariant {
                message: "mapped parameter has not already been captured",
            });
        }
    }
    Ok(())
}

fn install_mapped_arguments_cells(
    runtime: &Runtime,
    frame: &mut Frame,
    object: ObjectId,
    active_arguments: &[u32],
) -> Result<(), EngineFault> {
    for &index in active_arguments {
        if runtime
            .mapped_arguments_cell(
                object,
                &PropertyKey::from_index(ArrayIndex::new(index).ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "mapped parameter index is an array index",
                    },
                )?),
            )?
            .is_none()
        {
            return Err(EngineFault::RuntimeInvariant {
                message: "mapped arguments object contains every mapped parameter",
            });
        }
    }

    for &index in active_arguments {
        let key = PropertyKey::from_index(ArrayIndex::new(index).ok_or(
            EngineFault::RuntimeInvariant {
                message: "mapped parameter index is an array index",
            },
        )?);
        let cell =
            runtime
                .mapped_arguments_cell(object, &key)?
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "mapped arguments object contains every mapped parameter",
                })?;
        frame.arguments[index as usize] = FrameBinding::Captured(cell);
    }
    for (own_index, address) in frame.own_cell_bindings.iter().enumerate() {
        let FrameBindingAddress::Argument(index) = *address else {
            continue;
        };
        if active_arguments.binary_search(&index).is_err() {
            continue;
        }
        let key = PropertyKey::from_index(ArrayIndex::new(index).ok_or(
            EngineFault::RuntimeInvariant {
                message: "mapped captured parameter index is an array index",
            },
        )?);
        let cell =
            runtime
                .mapped_arguments_cell(object, &key)?
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "captured mapped parameter has a binding cell",
                })?;
        frame.own_cells[own_index] = Some(cell);
    }
    Ok(())
}
