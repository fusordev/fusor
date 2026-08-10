use std::collections::VecDeque;

use quickjs_bytecode::{
    InstructionIndex, VerificationLimits, VerifiedControlFlow, VerifiedSuccessorKind,
};

use super::{
    super::LeafCompilationError,
    ConstantInputs,
    abstract_value::{
        AbstractValue, constant_branch_outcome, is_truthiness_branch, transfer_stack,
    },
};

pub(super) fn analyze_constant_branches(
    control_flow: &VerifiedControlFlow,
    inputs: &ConstantInputs<'_>,
    limits: VerificationLimits,
) -> Result<Vec<Option<bool>>, LeafCompilationError> {
    let instruction_count = control_flow.instructions().len();
    let mut entry_stacks = reserved_vec(instruction_count, "CFG constant entry states")?;
    entry_stacks.resize_with(instruction_count, || None);
    let Some(entry) = entry_stacks.first_mut() else {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "verified control flow has an optimizer entry instruction",
            span: None,
        });
    };
    *entry = Some(Vec::new());

    let mut worklist = VecDeque::new();
    reserve_worklist(&mut worklist)?;
    worklist.push_back(0_usize);
    let mut evaluations = 0_u64;

    while let Some(position) = worklist.pop_front() {
        evaluations = evaluations
            .checked_add(1)
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "CFG constant propagation evaluations",
            })?;
        if evaluations > limits.max_transfer_evaluations() {
            return Err(LeafCompilationError::CapacityExceeded {
                domain: "CFG constant propagation evaluations",
            });
        }

        let Some(verified) = control_flow.instructions().get(position).copied() else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "CFG constant work item resolves to a verified instruction",
                span: None,
            });
        };
        let Some(entry_stack) = entry_stacks.get(position).and_then(Option::as_deref) else {
            continue;
        };
        let mut output = clone_values(entry_stack, "CFG constant transfer stack")?;
        let expected_depth =
            verified
                .entry_stack_depth()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "CFG constant work item is verifier-reachable",
                    span: None,
                })?;
        if output.len() != usize_from_u32(expected_depth, "verified optimizer stack depth")? {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "CFG constant entry stack matches verified depth",
                span: None,
            });
        }

        let instruction = verified.decoded().instruction();
        let branch_outcome = constant_branch_outcome(instruction.opcode(), &output);
        transfer_stack(&mut output, instruction, inputs)?;

        let successors = verified.successors();
        match successors.kind() {
            VerifiedSuccessorKind::Fallthrough => {
                merge_entry_stack(
                    &mut entry_stacks,
                    &mut worklist,
                    required_successor(successors.fallthrough())?,
                    &output,
                )?;
            }
            VerifiedSuccessorKind::Jump => {
                merge_entry_stack(
                    &mut entry_stacks,
                    &mut worklist,
                    required_successor(successors.jump_target())?,
                    &output,
                )?;
            }
            VerifiedSuccessorKind::Branch => {
                propagate_branch(
                    control_flow,
                    &mut entry_stacks,
                    &mut worklist,
                    instruction.opcode(),
                    branch_outcome,
                    required_successor(successors.branch_target())?,
                    required_successor(successors.fallthrough())?,
                    &output,
                )?;
            }
            VerifiedSuccessorKind::Terminate => {}
        }
    }

    let mut outcomes = reserved_vec(instruction_count, "CFG constant branch outcomes")?;
    for (position, verified) in control_flow.instructions().iter().copied().enumerate() {
        let outcome = entry_stacks
            .get(position)
            .and_then(Option::as_deref)
            .and_then(|stack| {
                constant_branch_outcome(verified.decoded().instruction().opcode(), stack)
            });
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

#[allow(clippy::too_many_arguments)]
fn propagate_branch(
    control_flow: &VerifiedControlFlow,
    entry_stacks: &mut [Option<Vec<AbstractValue>>],
    worklist: &mut VecDeque<usize>,
    opcode: quickjs_bytecode::FinalOpcode,
    outcome: Option<bool>,
    taken: InstructionIndex,
    not_taken: InstructionIndex,
    output: &[AbstractValue],
) -> Result<(), LeafCompilationError> {
    if is_truthiness_branch(opcode) {
        match outcome {
            Some(true) => merge_entry_stack(entry_stacks, worklist, taken, output),
            Some(false) => merge_entry_stack(entry_stacks, worklist, not_taken, output),
            None => {
                merge_entry_stack(entry_stacks, worklist, taken, output)?;
                merge_entry_stack(entry_stacks, worklist, not_taken, output)
            }
        }
    } else {
        let opaque = opaque_target_stack(control_flow, taken)?;
        merge_entry_stack(entry_stacks, worklist, taken, &opaque)?;
        merge_entry_stack(entry_stacks, worklist, not_taken, output)
    }
}

fn merge_entry_stack(
    entry_stacks: &mut [Option<Vec<AbstractValue>>],
    worklist: &mut VecDeque<usize>,
    target: InstructionIndex,
    incoming: &[AbstractValue],
) -> Result<(), LeafCompilationError> {
    let target = usize_from_u32(target.get(), "optimizer successor index")?;
    let Some(entry) = entry_stacks.get_mut(target) else {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "verified optimizer successor resolves to an instruction",
            span: None,
        });
    };
    let changed = match entry {
        None => {
            *entry = Some(clone_values(incoming, "CFG constant entry stack")?);
            true
        }
        Some(established) => join_entry_stack(established, incoming)?,
    };
    if changed {
        reserve_worklist(worklist)?;
        worklist.push_back(target);
    }
    Ok(())
}

fn join_entry_stack(
    established: &mut [AbstractValue],
    incoming: &[AbstractValue],
) -> Result<bool, LeafCompilationError> {
    if established.len() != incoming.len() {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "verified optimizer joins have equal stack depth",
            span: None,
        });
    }
    let mut changed = false;
    for (current, incoming) in established.iter_mut().zip(incoming.iter().copied()) {
        let joined = current.join(incoming);
        if joined != *current {
            *current = joined;
            changed = true;
        }
    }
    Ok(changed)
}

fn opaque_target_stack(
    control_flow: &VerifiedControlFlow,
    target: InstructionIndex,
) -> Result<Vec<AbstractValue>, LeafCompilationError> {
    let verified =
        control_flow
            .instruction(target)
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "opaque structural target resolves to a verified instruction",
                span: None,
            })?;
    let depth = verified
        .entry_stack_depth()
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "executable structural target is verifier-reachable",
            span: None,
        })?;
    let depth = usize_from_u32(depth, "opaque structural target depth")?;
    let mut stack = reserved_vec(depth, "opaque structural target stack")?;
    stack.resize(depth, AbstractValue::Overdefined);
    Ok(stack)
}

fn required_successor(
    successor: Option<InstructionIndex>,
) -> Result<InstructionIndex, LeafCompilationError> {
    successor.ok_or(LeafCompilationError::SemanticInvariant {
        invariant: "verified successor shape retains its target",
        span: None,
    })
}

fn reserve_worklist(worklist: &mut VecDeque<usize>) -> Result<(), LeafCompilationError> {
    worklist
        .try_reserve(1)
        .map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "CFG constant propagation worklist",
        })
}

fn clone_values(
    values: &[AbstractValue],
    domain: &'static str,
) -> Result<Vec<AbstractValue>, LeafCompilationError> {
    let mut clone = reserved_vec(values.len(), domain)?;
    clone.extend_from_slice(values);
    Ok(clone)
}

fn reserved_vec<T>(capacity: usize, domain: &'static str) -> Result<Vec<T>, LeafCompilationError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| LeafCompilationError::CapacityExceeded { domain })?;
    Ok(values)
}

fn usize_from_u32(value: u32, domain: &'static str) -> Result<usize, LeafCompilationError> {
    usize::try_from(value).map_err(|_| LeafCompilationError::CapacityExceeded { domain })
}
