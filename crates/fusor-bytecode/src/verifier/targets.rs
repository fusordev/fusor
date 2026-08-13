use crate::{BytecodePc, DecodedInstruction, FinalOpcode, OperandFormat, Operands};

use super::{
    ControlFlowEdge, InstructionIndex, InvalidControlFlowTargetReason, VerificationError,
    VerificationErrorKind, VerificationResource, VerifiedInstruction,
    predecode::is_instruction_start, usize_to_u64,
};

pub(super) fn require_encoded_target(
    decoded: DecodedInstruction,
    target: Option<InstructionIndex>,
) -> Result<InstructionIndex, VerificationError> {
    target.ok_or_else(|| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::MissingControlFlowOperand {
                expected: decoded.instruction().opcode().metadata().operand_format(),
            },
        )
    })
}

fn decoded_instruction_index_at(
    instructions: &[DecodedInstruction],
    bitmap: &[u64],
    bytecode_len: usize,
    pc: BytecodePc,
) -> Option<InstructionIndex> {
    if !is_instruction_start(bitmap, bytecode_len, pc) {
        return None;
    }
    let index = instructions
        .binary_search_by_key(&pc, |instruction| instruction.pc())
        .ok()?;
    Some(InstructionIndex(u32::try_from(index).ok()?))
}

pub(super) fn instruction_index_at(
    instructions: &[VerifiedInstruction],
    bitmap: &[u64],
    bytecode_len: usize,
    pc: BytecodePc,
) -> Option<InstructionIndex> {
    if !is_instruction_start(bitmap, bytecode_len, pc) {
        return None;
    }
    let index = instructions
        .binary_search_by_key(&pc, |instruction| instruction.decoded.pc())
        .ok()?;
    Some(InstructionIndex(u32::try_from(index).ok()?))
}

pub(super) fn resolve_relative_target(
    decoded: DecodedInstruction,
    instructions: &[DecodedInstruction],
    bitmap: &[u64],
    bytecode_len: usize,
) -> Result<Option<InstructionIndex>, VerificationError> {
    let Some((edge, base_delta, displacement)) = relative_target_spec(decoded)? else {
        return Ok(None);
    };
    let pc = i64::from(decoded.pc().get());
    let base = pc.checked_add(base_delta).ok_or_else(|| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::ControlFlowTargetOverflow {
                edge,
                base: pc,
                displacement: base_delta,
            },
        )
    })?;
    let target = base.checked_add(displacement).ok_or_else(|| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::ControlFlowTargetOverflow {
                edge,
                base,
                displacement,
            },
        )
    })?;
    resolve_target(decoded, edge, target, instructions, bitmap, bytecode_len).map(Some)
}

fn relative_target_spec(
    decoded: DecodedInstruction,
) -> Result<Option<(ControlFlowEdge, i64, i64)>, VerificationError> {
    let instruction = decoded.instruction();
    let opcode = instruction.opcode();
    let operands = instruction.operands();

    let missing = |expected| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::MissingControlFlowOperand { expected },
        )
    };

    match opcode {
        FinalOpcode::IfFalse8 | FinalOpcode::IfTrue8 => match operands {
            Operands::Label8(displacement) => {
                Ok(Some((ControlFlowEdge::Branch, 1, i64::from(displacement))))
            }
            _ => Err(missing(OperandFormat::Label8)),
        },
        FinalOpcode::IfFalse | FinalOpcode::IfTrue => match operands {
            Operands::Label(displacement) => {
                Ok(Some((ControlFlowEdge::Branch, 1, i64::from(displacement))))
            }
            _ => Err(missing(OperandFormat::Label)),
        },
        FinalOpcode::Goto8 => match operands {
            Operands::Label8(displacement) => {
                Ok(Some((ControlFlowEdge::Jump, 1, i64::from(displacement))))
            }
            _ => Err(missing(OperandFormat::Label8)),
        },
        FinalOpcode::Goto16 => match operands {
            Operands::Label16(displacement) => {
                Ok(Some((ControlFlowEdge::Jump, 1, i64::from(displacement))))
            }
            _ => Err(missing(OperandFormat::Label16)),
        },
        FinalOpcode::Goto => match operands {
            Operands::Label(displacement) => {
                Ok(Some((ControlFlowEdge::Jump, 1, i64::from(displacement))))
            }
            _ => Err(missing(OperandFormat::Label)),
        },
        FinalOpcode::Catch => match operands {
            Operands::Label(displacement) => Ok(Some((
                ControlFlowEdge::CatchHandler,
                1,
                i64::from(displacement),
            ))),
            _ => Err(missing(OperandFormat::Label)),
        },
        FinalOpcode::Gosub => match operands {
            Operands::Label(displacement) => Ok(Some((
                ControlFlowEdge::FinallySubroutine,
                1,
                i64::from(displacement),
            ))),
            _ => Err(missing(OperandFormat::Label)),
        },
        FinalOpcode::WithGetVar
        | FinalOpcode::WithPutVar
        | FinalOpcode::WithDeleteVar
        | FinalOpcode::WithMakeRef
        | FinalOpcode::WithGetRef => match operands {
            Operands::AtomLabelU8 { label, .. } => {
                Ok(Some((ControlFlowEdge::WithBinding, 5, i64::from(label))))
            }
            _ => Err(missing(OperandFormat::AtomLabelU8)),
        },
        _ => Ok(None),
    }
}

pub(super) fn validate_gosub_continuation(
    decoded: DecodedInstruction,
    instructions: &[DecodedInstruction],
    bitmap: &[u64],
    bytecode_len: usize,
) -> Result<(), VerificationError> {
    if decoded.instruction().opcode() != FinalOpcode::Gosub {
        return Ok(());
    }
    resolve_target(
        decoded,
        ControlFlowEdge::FinallyContinuation,
        i64::from(decoded.next_pc().get()),
        instructions,
        bitmap,
        bytecode_len,
    )?;
    Ok(())
}

pub(super) fn resolve_fallthrough(
    decoded: DecodedInstruction,
    instructions: &[DecodedInstruction],
    bitmap: &[u64],
    bytecode_len: usize,
) -> Result<InstructionIndex, VerificationError> {
    resolve_target(
        decoded,
        ControlFlowEdge::Fallthrough,
        i64::from(decoded.next_pc().get()),
        instructions,
        bitmap,
        bytecode_len,
    )
}

fn resolve_target(
    decoded: DecodedInstruction,
    edge: ControlFlowEdge,
    target: i64,
    instructions: &[DecodedInstruction],
    bitmap: &[u64],
    bytecode_len: usize,
) -> Result<InstructionIndex, VerificationError> {
    let bytecode_len_u32 = u32::try_from(bytecode_len).map_err(|_| {
        VerificationError::root(VerificationErrorKind::LimitExceeded {
            resource: VerificationResource::BytecodeBytes,
            limit: u64::from(u32::MAX),
            observed: usize_to_u64(bytecode_len),
        })
    })?;

    if target < 0 || target >= i64::from(bytecode_len_u32) {
        return Err(VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::InvalidControlFlowTarget {
                edge,
                target,
                bytecode_len: bytecode_len_u32,
                reason: InvalidControlFlowTargetReason::OutsideBytecode,
            },
        ));
    }
    if decoded.instruction().opcode() == FinalOpcode::Catch && target == 0 {
        return Err(VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::InvalidControlFlowTarget {
                edge,
                target,
                bytecode_len: bytecode_len_u32,
                reason: InvalidControlFlowTargetReason::CatchTargetZero,
            },
        ));
    }

    let target_pc = BytecodePc::new(u32::try_from(target).map_err(|_| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::InvalidControlFlowTarget {
                edge,
                target,
                bytecode_len: bytecode_len_u32,
                reason: InvalidControlFlowTargetReason::OutsideBytecode,
            },
        )
    })?);
    decoded_instruction_index_at(instructions, bitmap, bytecode_len, target_pc).ok_or_else(|| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::InvalidControlFlowTarget {
                edge,
                target,
                bytecode_len: bytecode_len_u32,
                reason: InvalidControlFlowTargetReason::NotInstructionBoundary,
            },
        )
    })
}
