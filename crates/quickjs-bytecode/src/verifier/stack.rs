use std::collections::VecDeque;

use crate::{DecodedInstruction, FinalOpcode, FunctionKind};

use super::{
    InstructionIndex, VerificationError, VerificationErrorKind, VerificationLimits,
    VerificationResource, VerifiedInstruction, model::VerifiedSuccessorsRepr,
    static_control_flow::StructurallyVerifiedControlFlow, usize_to_u64,
};

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(test)]
static FAIL_WORKLIST_RESERVATION: AtomicBool = AtomicBool::new(false);

/// Ordinary-stack dataflow certificate for structurally checked control flow.
pub(super) struct StackCertificate {
    pub(super) instructions: Vec<VerifiedInstruction>,
    pub(super) computed_stack_size: u32,
    pub(super) transfer_evaluations: u64,
}

#[allow(clippy::too_many_lines)]
pub(super) fn analyze_ordinary_stack(
    structurally_verified: StructurallyVerifiedControlFlow,
    limits: VerificationLimits,
    require_empty_exits: bool,
) -> Result<StackCertificate, VerificationError> {
    let (mut instructions, function_kind) = structurally_verified.into_parts();
    let Some(entry) = instructions.first_mut() else {
        return Err(VerificationError::root(
            VerificationErrorKind::EmptyBytecode,
        ));
    };
    entry.entry_stack_depth = Some(0);

    let mut worklist = VecDeque::new();
    reserve_worklist_entry(&mut worklist, entry.decoded)?;
    worklist.push_back(InstructionIndex(0));

    let mut computed_max = 0_u32;
    let mut evaluations = 0_u64;
    let has_catch_marker = instructions.iter().any(|instruction| {
        matches!(
            instruction.decoded.instruction().opcode(),
            FinalOpcode::Catch | FinalOpcode::ForOfStart | FinalOpcode::ForAwaitOfStart
        )
    });
    let has_gosub = instructions
        .iter()
        .any(|instruction| instruction.decoded.instruction().opcode() == FinalOpcode::Gosub);

    while let Some(index) = worklist.pop_front() {
        let position = usize::try_from(index.get()).map_err(|_| {
            VerificationError::root(VerificationErrorKind::LimitExceeded {
                resource: VerificationResource::Instructions,
                limit: u64::from(u32::MAX),
                observed: u64::from(index.get()),
            })
        })?;
        let Some(current) = instructions.get(position).copied() else {
            return Err(VerificationError::root(
                VerificationErrorKind::LimitExceeded {
                    resource: VerificationResource::Instructions,
                    limit: usize_to_u64(instructions.len()),
                    observed: u64::from(index.get()) + 1,
                },
            ));
        };
        let Some(entry_depth) = current.entry_stack_depth else {
            continue;
        };

        evaluations = evaluations.checked_add(1).ok_or_else(|| {
            VerificationError::at_instruction(
                current.decoded,
                VerificationErrorKind::LimitExceeded {
                    resource: VerificationResource::TransferEvaluations,
                    limit: limits.max_transfer_evaluations,
                    observed: u64::MAX,
                },
            )
        })?;
        if evaluations > limits.max_transfer_evaluations {
            return Err(VerificationError::at_instruction(
                current.decoded,
                VerificationErrorKind::LimitExceeded {
                    resource: VerificationResource::TransferEvaluations,
                    limit: limits.max_transfer_evaluations,
                    observed: evaluations,
                },
            ));
        }

        let effect = current
            .decoded
            .instruction()
            .stack_effect()
            .map_err(|source| {
                VerificationError::at_instruction(
                    current.decoded,
                    VerificationErrorKind::StackEffect(source),
                )
            })?;
        if entry_depth < effect.pops() {
            return Err(VerificationError::at_instruction(
                current.decoded,
                VerificationErrorKind::StackUnderflow {
                    required: effect.pops(),
                    available: entry_depth,
                },
            ));
        }
        let output_depth = u64::from(entry_depth - effect.pops()) + u64::from(effect.pushes());
        if output_depth > u64::from(limits.max_stack_depth) {
            return Err(VerificationError::at_instruction(
                current.decoded,
                VerificationErrorKind::StackLimitExceeded {
                    depth: output_depth,
                    limit: limits.max_stack_depth,
                },
            ));
        }
        let output_depth = u32::try_from(output_depth).map_err(|_| {
            VerificationError::at_instruction(
                current.decoded,
                VerificationErrorKind::StackLimitExceeded {
                    depth: output_depth,
                    limit: limits.max_stack_depth,
                },
            )
        })?;
        computed_max = computed_max.max(output_depth);
        let finally_subroutine_depth =
            if current.decoded.instruction().opcode() == FinalOpcode::Gosub {
                let depth = u64::from(output_depth).checked_add(1).ok_or_else(|| {
                    VerificationError::at_instruction(
                        current.decoded,
                        VerificationErrorKind::StackLimitExceeded {
                            depth: u64::MAX,
                            limit: limits.max_stack_depth,
                        },
                    )
                })?;
                if depth > u64::from(limits.max_stack_depth) {
                    return Err(VerificationError::at_instruction(
                        current.decoded,
                        VerificationErrorKind::StackLimitExceeded {
                            depth,
                            limit: limits.max_stack_depth,
                        },
                    ));
                }
                let depth = u32::try_from(depth).map_err(|_| {
                    VerificationError::at_instruction(
                        current.decoded,
                        VerificationErrorKind::StackLimitExceeded {
                            depth,
                            limit: limits.max_stack_depth,
                        },
                    )
                })?;
                computed_max = computed_max.max(depth);
                Some(depth)
            } else {
                None
            };
        let with_binding_depth =
            if current.decoded.instruction().opcode() == FinalOpcode::WithGetVar {
                let depth = u64::from(output_depth).checked_add(1).ok_or_else(|| {
                    VerificationError::at_instruction(
                        current.decoded,
                        VerificationErrorKind::StackLimitExceeded {
                            depth: u64::MAX,
                            limit: limits.max_stack_depth,
                        },
                    )
                })?;
                if depth > u64::from(limits.max_stack_depth) {
                    return Err(VerificationError::at_instruction(
                        current.decoded,
                        VerificationErrorKind::StackLimitExceeded {
                            depth,
                            limit: limits.max_stack_depth,
                        },
                    ));
                }
                let depth = u32::try_from(depth).map_err(|_| {
                    VerificationError::at_instruction(
                        current.decoded,
                        VerificationErrorKind::StackLimitExceeded {
                            depth,
                            limit: limits.max_stack_depth,
                        },
                    )
                })?;
                computed_max = computed_max.max(depth);
                Some(depth)
            } else {
                None
            };

        match current.successors.0 {
            VerifiedSuccessorsRepr::Fallthrough(successor)
            | VerifiedSuccessorsRepr::Jump(successor) => propagate_stack_depth(
                &mut instructions,
                &mut worklist,
                successor,
                output_depth,
                current.decoded,
            )?,
            VerifiedSuccessorsRepr::Branch { taken, not_taken } => {
                propagate_stack_depth(
                    &mut instructions,
                    &mut worklist,
                    taken,
                    finally_subroutine_depth
                        .or(with_binding_depth)
                        .unwrap_or(output_depth),
                    current.decoded,
                )?;
                propagate_stack_depth(
                    &mut instructions,
                    &mut worklist,
                    not_taken,
                    output_depth,
                    current.decoded,
                )?;
            }
            VerifiedSuccessorsRepr::Terminate => {
                let protected_throw = has_catch_marker
                    && matches!(
                        current.decoded.instruction().opcode(),
                        FinalOpcode::Throw | FinalOpcode::ThrowError
                    );
                let returns_from_finally =
                    current.decoded.instruction().opcode() == FinalOpcode::Ret;
                // Resuming a suspended `yield` with `generator.return(value)`
                // abandons the surrounding expression evaluation. The VM
                // retains that expression stack in the suspended frame, then
                // discards the frame after `return_async` consumes `value`.
                // Async functions have no corresponding suspended expression
                // context and retain the ordinary empty-exit requirement.
                let abandons_generator_expression = matches!(
                    function_kind,
                    FunctionKind::Generator | FunctionKind::AsyncGenerator
                ) && current.decoded.instruction().opcode()
                    == FinalOpcode::ReturnAsync;
                // This structural pass cannot distinguish the pending
                // completion and return-address slots introduced by `gosub`.
                // A body containing `gosub` may therefore defer non-empty
                // abrupt-exit proof to the whole-bytecode typed stack pass.
                // `VerifiedControlFlow` alone remains non-executable.
                let defers_typed_finally_exit = has_gosub
                    && matches!(
                        current.decoded.instruction().opcode(),
                        FinalOpcode::Return | FinalOpcode::ReturnUndef | FinalOpcode::Throw
                    );
                if require_empty_exits
                    && output_depth != 0
                    && !protected_throw
                    && !returns_from_finally
                    && !abandons_generator_expression
                    && !defers_typed_finally_exit
                {
                    return Err(VerificationError::at_instruction(
                        current.decoded,
                        VerificationErrorKind::NonEmptyCompilerExitStack {
                            remaining: output_depth,
                        },
                    ));
                }
            }
        }
    }

    Ok(StackCertificate {
        instructions,
        computed_stack_size: computed_max,
        transfer_evaluations: evaluations,
    })
}

fn propagate_stack_depth(
    instructions: &mut [VerifiedInstruction],
    worklist: &mut VecDeque<InstructionIndex>,
    target: InstructionIndex,
    incoming_depth: u32,
    source: DecodedInstruction,
) -> Result<(), VerificationError> {
    let position = usize::try_from(target.get()).map_err(|_| {
        VerificationError::at_instruction(
            source,
            VerificationErrorKind::LimitExceeded {
                resource: VerificationResource::Instructions,
                limit: u64::from(u32::MAX),
                observed: u64::from(target.get()),
            },
        )
    })?;
    let Some(target_instruction) = instructions.get_mut(position) else {
        return Err(VerificationError::at_instruction(
            source,
            VerificationErrorKind::LimitExceeded {
                resource: VerificationResource::Instructions,
                limit: usize_to_u64(instructions.len()),
                observed: u64::from(target.get()) + 1,
            },
        ));
    };

    match target_instruction.entry_stack_depth {
        None => {
            reserve_worklist_entry(worklist, source)?;
            target_instruction.entry_stack_depth = Some(incoming_depth);
            worklist.push_back(target);
        }
        Some(established_depth) if established_depth == incoming_depth => {}
        Some(established_depth) => {
            return Err(VerificationError::at_instruction(
                source,
                VerificationErrorKind::InconsistentStackAtJoin {
                    target: target_instruction.decoded.pc(),
                    established_depth,
                    incoming_depth,
                    incoming_from: source.pc(),
                },
            ));
        }
    }
    Ok(())
}

fn reserve_worklist_entry(
    worklist: &mut VecDeque<InstructionIndex>,
    decoded: DecodedInstruction,
) -> Result<(), VerificationError> {
    if worklist.len() == worklist.capacity() {
        try_reserve_worklist(worklist, 1).map_err(|_| {
            VerificationError::at_instruction(
                decoded,
                VerificationErrorKind::AllocationFailed {
                    resource: VerificationResource::WorklistEntries,
                    requested: 1,
                },
            )
        })?;
    }
    Ok(())
}

fn try_reserve_worklist(
    worklist: &mut VecDeque<InstructionIndex>,
    additional: usize,
) -> Result<(), std::collections::TryReserveError> {
    #[cfg(test)]
    if FAIL_WORKLIST_RESERVATION.swap(false, Ordering::Relaxed) {
        return VecDeque::<InstructionIndex>::new().try_reserve(usize::MAX);
    }
    worklist.try_reserve(additional)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use crate::{BytecodeBuilder, BytecodePc, FinalOpcode, InstructionDecoder, Operands};

    use super::{
        FAIL_WORKLIST_RESERVATION, Ordering, VerificationErrorKind, VerificationResource,
        reserve_worklist_entry,
    };

    #[test]
    fn worklist_allocation_failure_preserves_exact_resource_and_location() {
        let mut builder = BytecodeBuilder::new();
        builder
            .push(FinalOpcode::ReturnUndef, Operands::None)
            .expect("test instruction must encode");
        let bytecode = builder.into_bytes();
        let decoded = InstructionDecoder::new(&bytecode)
            .next()
            .expect("test body must contain an instruction")
            .expect("test instruction must decode");

        FAIL_WORKLIST_RESERVATION.store(true, Ordering::Relaxed);
        let error = reserve_worklist_entry(&mut VecDeque::new(), decoded)
            .expect_err("injected worklist reservation failure must fail closed");
        assert_eq!(error.pc(), Some(BytecodePc::ZERO));
        assert_eq!(error.opcode(), Some(FinalOpcode::ReturnUndef));
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::AllocationFailed {
                resource: VerificationResource::WorklistEntries,
                requested: 1,
            }
        );
    }
}
