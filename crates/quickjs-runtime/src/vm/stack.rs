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

//! Operand-stack, call-input, and compact operand decoding helpers.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) fn frame_argument(frame: &Frame, index: u32) -> Result<&FrameBinding, EngineFault> {
    frame
        .arguments
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "argument",
            index,
        })
}

pub(super) fn frame_argument_mut(
    frame: &mut Frame,
    index: u32,
) -> Result<&mut FrameBinding, EngineFault> {
    frame
        .arguments
        .get_mut(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "argument",
            index,
        })
}

pub(super) fn frame_local(frame: &Frame, index: u32) -> Result<&FrameBinding, EngineFault> {
    frame
        .locals
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "local",
            index,
        })
}

pub(super) fn frame_local_mut(
    frame: &mut Frame,
    index: u32,
) -> Result<&mut FrameBinding, EngineFault> {
    frame
        .locals
        .get_mut(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "local",
            index,
        })
}

pub(super) struct CallInputs {
    pub(super) receiver: StoredValue,
    pub(super) arguments: CallArguments,
    pub(super) new_target: Option<FunctionId>,
}

pub(super) fn take_call_inputs(
    frame: &mut Frame,
    expected_function: FunctionId,
    source: CallInputSource,
) -> Result<CallInputs, ExecutionError> {
    let (argument_count, kind, preserve_receiver) = match source {
        CallInputSource::Frame {
            argument_count,
            kind,
        } => (argument_count, kind, false),
        CallInputSource::EvalReferenceFrame { argument_count } => {
            (argument_count, CallKind::Method, true)
        }
        CallInputSource::Prepared(inputs) => return Ok(inputs),
    };
    let required = argument_count.saturating_add(match kind {
        CallKind::Direct => 1,
        CallKind::Method | CallKind::Constructor => 2,
    });
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
    let new_target = if matches!(kind, CallKind::Constructor) {
        match pop(frame)? {
            StoredValue::Function(function) => Some(function),
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "validated constructor new target changed value kind",
                }
                .into());
            }
        }
    } else {
        None
    };
    match pop(frame)? {
        StoredValue::Function(actual) if actual == expected_function => {}
        StoredValue::Function(_) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "parked ordinary-call callee changed before frame creation",
            }
            .into());
        }
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Object(_) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "validated ordinary-call callee changed value kind",
            }
            .into());
        }
    }
    let receiver = match kind {
        CallKind::Method => pop(frame)?,
        CallKind::Direct | CallKind::Constructor => StoredValue::Undefined,
    };
    if preserve_receiver {
        push(frame, receiver.duplicate());
    }
    Ok(CallInputs {
        receiver,
        arguments: CallArguments::from_values(arguments),
        new_target,
    })
}

pub(super) fn push(frame: &mut Frame, value: StoredValue) {
    frame.stack.push(OperandStackEntry::JavaScript(value));
}

pub(super) fn pop(frame: &mut Frame) -> Result<StoredValue, EngineFault> {
    match frame.stack.pop() {
        Some(OperandStackEntry::JavaScript(value)) => Ok(value),
        Some(OperandStackEntry::Catch { .. } | OperandStackEntry::ForOfCatch { .. }) => {
            Err(EngineFault::RuntimeInvariant {
                message: "verified JavaScript value operation consumed an internal catch marker",
            })
        }
        Some(OperandStackEntry::FinallyReturn { .. }) => Err(EngineFault::RuntimeInvariant {
            message: "verified JavaScript value operation consumed an internal finally return address",
        }),
        None => Err(EngineFault::StackDepthMismatch {
            function: frame.template,
            pc: BytecodePc::ZERO,
            expected: 1,
            actual: 0,
        }),
    }
}

pub(super) fn peek(frame: &Frame) -> Result<&StoredValue, EngineFault> {
    match frame.stack.last() {
        Some(OperandStackEntry::JavaScript(value)) => Ok(value),
        Some(OperandStackEntry::Catch { .. } | OperandStackEntry::ForOfCatch { .. }) => {
            Err(EngineFault::RuntimeInvariant {
                message: "verified JavaScript value operation inspected an internal catch marker",
            })
        }
        Some(OperandStackEntry::FinallyReturn { .. }) => Err(EngineFault::RuntimeInvariant {
            message: "verified JavaScript value operation inspected an internal finally return address",
        }),
        None => Err(EngineFault::StackDepthMismatch {
            function: frame.template,
            pc: BytecodePc::ZERO,
            expected: 1,
            actual: 0,
        }),
    }
}

pub(super) fn stack_value_at(frame: &Frame, index: usize) -> Result<&StoredValue, EngineFault> {
    match frame.stack.get(index) {
        Some(OperandStackEntry::JavaScript(value)) => Ok(value),
        Some(OperandStackEntry::Catch { .. } | OperandStackEntry::ForOfCatch { .. }) => {
            Err(EngineFault::RuntimeInvariant {
                message: "verified JavaScript value operation indexed an internal catch marker",
            })
        }
        Some(OperandStackEntry::FinallyReturn { .. }) => Err(EngineFault::RuntimeInvariant {
            message: "verified JavaScript value operation indexed an internal finally return address",
        }),
        None => Err(EngineFault::StackDepthMismatch {
            function: frame.template,
            pc: BytecodePc::ZERO,
            expected: u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX),
            actual: frame.stack.len(),
        }),
    }
}

pub(super) fn pop_finally_continuation(frame: &mut Frame) -> Result<InstructionIndex, EngineFault> {
    match frame.stack.pop() {
        Some(OperandStackEntry::FinallyReturn { continuation }) => Ok(continuation),
        Some(
            OperandStackEntry::JavaScript(_)
            | OperandStackEntry::Catch { .. }
            | OperandStackEntry::ForOfCatch { .. },
        ) => Err(EngineFault::RuntimeInvariant {
            message: "verified ret operand is not an internal finally return address",
        }),
        None => Err(EngineFault::StackDepthMismatch {
            function: frame.template,
            pc: BytecodePc::ZERO,
            expected: 1,
            actual: 0,
        }),
    }
}

pub(super) fn enter_finally_subroutine(
    instruction: quickjs_bytecode::VerifiedInstruction,
    frame: &mut Frame,
) -> Result<(), EngineFault> {
    let target = branch_successor(instruction, true, frame)?;
    let continuation = branch_successor(instruction, false, frame)?;
    frame
        .stack
        .push(OperandStackEntry::FinallyReturn { continuation });
    frame.instruction = target;
    Ok(())
}

pub(super) fn normalize_stack_depth_correction(
    runtime: &Runtime,
    frame: &mut Frame,
    instruction: InstructionIndex,
) -> Result<(), EngineFault> {
    let verified = code(runtime, frame.code)?
        .authority
        .function(frame.template)
        .and_then(|function| function.function().control_flow().instruction(instruction))
        .copied()
        .ok_or(EngineFault::MissingInstruction {
            function: frame.template,
            instruction: instruction.get(),
        })?;
    let structural_depth =
        verified
            .entry_stack_depth()
            .ok_or(EngineFault::UnreachableInstruction {
                function: frame.template,
                pc: verified.decoded().pc(),
            })?;
    let actual_depth =
        u32::try_from(frame.stack.len()).map_err(|_| EngineFault::StackDepthMismatch {
            function: frame.template,
            pc: verified.decoded().pc(),
            expected: structural_depth,
            actual: frame.stack.len(),
        })?;
    frame.stack_depth_correction =
        structural_depth
            .checked_sub(actual_depth)
            .ok_or(EngineFault::StackDepthMismatch {
                function: frame.template,
                pc: verified.decoded().pc(),
                expected: structural_depth,
                actual: frame.stack.len(),
            })?;
    Ok(())
}

pub(super) fn nip_catch(
    frame: &mut Frame,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), ExecutionError> {
    execution_budget.charge_instructions(usize_to_u64(frame.stack.len()))?;

    peek(frame)?;
    let top = frame.stack.len().saturating_sub(1);
    let marker = frame.stack[..top]
        .iter()
        .rposition(|entry| {
            matches!(
                entry,
                OperandStackEntry::Catch { .. } | OperandStackEntry::ForOfCatch { .. }
            )
        })
        .ok_or(EngineFault::RuntimeInvariant {
            message: "verified nip_catch operand is not a catch marker",
        })?;

    let top = pop(frame)?;
    frame.stack.truncate(marker);
    push(frame, top);
    Ok(())
}

pub(super) fn push_for_of_record(
    frame: &mut Frame,
    iterator: StoredValue,
    next: StoredValue,
    asynchronous: bool,
    return_to: CallReturn,
) -> Result<(), ExecutionError> {
    if !matches!(return_to.disposition, ReturnDisposition::Push) {
        return Err(EngineFault::RuntimeInvariant {
            message: "for-of start reached a discarding continuation",
        }
        .into());
    }
    if frame.stack.capacity().saturating_sub(frame.stack.len()) < 3 {
        return Err(EngineFault::RuntimeInvariant {
            message: "verified for-of record exceeds frame stack capacity",
        }
        .into());
    }
    push(frame, iterator);
    push(frame, next);
    frame.stack.push(OperandStackEntry::ForOfCatch {
        active: true,
        asynchronous,
    });
    frame.instruction = return_to.instruction;
    Ok(())
}

pub(super) fn deactivate_for_of_record(
    frame: &mut Frame,
    allow_return_dummy: bool,
    offset: u8,
) -> Result<(StoredValue, StoredValue, bool), EngineFault> {
    // The record marker sits `offset` slots below the stack top: the
    // array-destructuring rest collector keeps its fresh array and cursor
    // above the record while stepping the shared iterator.
    let marker = frame
        .stack
        .len()
        .checked_sub(1_usize.saturating_add(usize::from(offset)))
        .ok_or(EngineFault::RuntimeInvariant {
            message: "verified for-of operation has no record marker",
        })?;
    match frame.stack.get_mut(marker) {
        Some(OperandStackEntry::ForOfCatch { active, .. }) if allow_return_dummy || *active => {
            *active = false;
        }
        Some(OperandStackEntry::JavaScript(StoredValue::Undefined)) if allow_return_dummy => {}
        Some(
            OperandStackEntry::JavaScript(_)
            | OperandStackEntry::Catch { .. }
            | OperandStackEntry::ForOfCatch { .. }
            | OperandStackEntry::FinallyReturn { .. },
        )
        | None => {
            return Err(EngineFault::RuntimeInvariant {
                message: "verified for-of operation has the wrong record marker",
            });
        }
    }
    let iterator_index = marker.checked_sub(2).ok_or(EngineFault::RuntimeInvariant {
        message: "verified for-of operation has an incomplete record",
    })?;
    let next_index = iterator_index.saturating_add(1);
    let iterator = stack_value_at(frame, iterator_index)?.duplicate();
    let next = stack_value_at(frame, next_index)?.duplicate();
    let asynchronous = match frame.stack.get(marker) {
        Some(OperandStackEntry::ForOfCatch { asynchronous, .. }) => *asynchronous,
        Some(OperandStackEntry::JavaScript(StoredValue::Undefined)) if allow_return_dummy => false,
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "verified for-of operation lost its record marker",
            });
        }
    };
    Ok((iterator, next, asynchronous))
}

pub(super) fn finish_for_of_step(
    frame: &mut Frame,
    value: StoredValue,
    done: bool,
    return_to: CallReturn,
    offset: u8,
) -> Result<(), ExecutionError> {
    if !matches!(return_to.disposition, ReturnDisposition::Push) {
        return Err(EngineFault::RuntimeInvariant {
            message: "for-of step reached a discarding continuation",
        }
        .into());
    }
    // The record marker is `offset` slots below the stack top, exactly as
    // the preceding `deactivate_for_of_record` located it; the native
    // `next()` call runs in child frames and never moves the parent stack.
    let marker = frame
        .stack
        .len()
        .checked_sub(1_usize.saturating_add(usize::from(offset)))
        .ok_or(EngineFault::RuntimeInvariant {
            message: "for-of step completed without its record marker",
        })?;
    match frame.stack.get_mut(marker) {
        Some(OperandStackEntry::ForOfCatch { active, .. }) if !*active => {
            *active = !done;
        }
        Some(
            OperandStackEntry::JavaScript(_)
            | OperandStackEntry::Catch { .. }
            | OperandStackEntry::ForOfCatch { .. }
            | OperandStackEntry::FinallyReturn { .. },
        )
        | None => {
            return Err(EngineFault::RuntimeInvariant {
                message: "for-of step completed with an active or invalid record marker",
            }
            .into());
        }
    }
    if done {
        let iterator = marker.checked_sub(2).ok_or(EngineFault::RuntimeInvariant {
            message: "for-of step completed with an incomplete record",
        })?;
        frame.stack[iterator] = OperandStackEntry::JavaScript(StoredValue::Undefined);
    }
    if frame.stack.capacity().saturating_sub(frame.stack.len()) < 2 {
        return Err(EngineFault::RuntimeInvariant {
            message: "verified for-of step exceeds frame stack capacity",
        }
        .into());
    }
    push(frame, value);
    push(frame, StoredValue::Boolean(done));
    frame.instruction = return_to.instruction;
    Ok(())
}

pub(super) fn finish_for_of_close(
    frame: &mut Frame,
    return_to: CallReturn,
) -> Result<(), ExecutionError> {
    if !matches!(return_to.disposition, ReturnDisposition::Discard) {
        return Err(EngineFault::RuntimeInvariant {
            message: "for-of close reached a value-producing continuation",
        }
        .into());
    }
    if matches!(
        frame.pending_async_iterator_close,
        Some(PendingAsyncIteratorClose::Normal)
    ) {
        frame.pending_async_iterator_close = None;
    }
    finish_for_of_close_record(frame)?;
    frame.instruction = return_to.instruction;
    Ok(())
}

pub(super) fn finish_for_of_close_record(frame: &mut Frame) -> Result<(), ExecutionError> {
    let marker = frame
        .stack
        .len()
        .checked_sub(1)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "for-of close completed without its record marker",
        })?;
    if !matches!(
        frame.stack.get(marker),
        Some(
            OperandStackEntry::ForOfCatch { active: false, .. }
                | OperandStackEntry::JavaScript(StoredValue::Undefined)
        )
    ) || marker < 2
    {
        return Err(EngineFault::RuntimeInvariant {
            message: "for-of close completed with an active or incomplete record",
        }
        .into());
    }
    frame.stack.truncate(marker - 2);
    Ok(())
}

pub(super) fn take_for_of_record_at(
    frame: &mut Frame,
    marker: usize,
) -> Result<(StoredValue, StoredValue, bool, bool), EngineFault> {
    if marker < 2 || marker >= frame.stack.len() {
        return Err(EngineFault::RuntimeInvariant {
            message: "exception unwinder found an incomplete for-of record",
        });
    }
    frame.stack.truncate(marker.saturating_add(1));
    let (active, asynchronous) = match frame.stack.pop() {
        Some(OperandStackEntry::ForOfCatch {
            active,
            asynchronous,
        }) => (active, asynchronous),
        Some(
            OperandStackEntry::JavaScript(_)
            | OperandStackEntry::Catch { .. }
            | OperandStackEntry::FinallyReturn { .. },
        )
        | None => {
            return Err(EngineFault::RuntimeInvariant {
                message: "exception unwinder selected a non-for-of marker",
            });
        }
    };
    let next = pop(frame)?;
    let iterator = pop(frame)?;
    Ok((iterator, next, active, asynchronous))
}

pub(super) fn drop_stack_entry(frame: &mut Frame) -> Result<(), EngineFault> {
    frame
        .stack
        .pop()
        .map(|_| ())
        .ok_or(EngineFault::StackDepthMismatch {
            function: frame.template,
            pc: BytecodePc::ZERO,
            expected: 1,
            actual: 0,
        })
}

pub(super) fn branch_successor(
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

pub(super) fn constant_index(operands: Operands) -> Option<u32> {
    match operands {
        Operands::Const(index) => Some(index),
        Operands::Const8(index) => Some(u32::from(index)),
        _ => None,
    }
}

pub(super) fn direct_call_argument_count(
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

pub(super) fn argument_index(opcode: FinalOpcode, operands: Operands) -> Result<u32, EngineFault> {
    match operands {
        Operands::Arg(index) => Ok(u32::from(index)),
        Operands::NoneArg => implied_index(opcode).ok_or(EngineFault::MissingPoolEntry {
            pool: "implied argument",
            index: u32::MAX,
        }),
        _ => Err(EngineFault::UnsupportedDispatch { opcode }),
    }
}

pub(super) fn local_index(opcode: FinalOpcode, operands: Operands) -> Result<u32, EngineFault> {
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

pub(super) fn closure_index(opcode: FinalOpcode, operands: Operands) -> Result<u32, EngineFault> {
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

pub(super) const fn implied_integer(opcode: FinalOpcode) -> Option<i32> {
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

pub(super) fn copy_environment(
    values: &[EnvironmentBinding],
    resource: RuntimeResource,
) -> Result<Vec<EnvironmentBinding>, ExecutionError> {
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

pub(super) fn copy_eval_binding_shadows(
    values: &[Option<EvalBindingShadow>],
) -> Result<Vec<Option<EvalBindingShadow>>, ExecutionError> {
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(values.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: values.len(),
        })?;
    copied.extend(values.iter().cloned());
    Ok(copied)
}

pub(super) fn copy_addresses(
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

pub(super) fn for_in_key_value(key: &PropertyKey) -> Result<StoredValue, ExecutionError> {
    if let Some(index) = key.as_index() {
        return Ok(StoredValue::String(index.to_js_string()?));
    }
    let atom = key.as_atom().ok_or(EngineFault::RuntimeInvariant {
        message: "for-in candidate is neither an array index nor an atom",
    })?;
    if atom.kind() != crate::AtomKind::String {
        return Err(EngineFault::RuntimeInvariant {
            message: "for-in candidate exposed a non-string atom",
        }
        .into());
    }
    let name = atom
        .description()
        .cloned()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "for-in string atom has no description",
        })?;
    Ok(StoredValue::String(name))
}

pub(super) fn unsupported_dispatch<T>(opcode: FinalOpcode) -> Result<T, ExecutionError> {
    Err(EngineFault::UnsupportedDispatch { opcode }.into())
}
