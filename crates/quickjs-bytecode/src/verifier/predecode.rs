use crate::{BytecodePc, DecodeError, DecodedInstruction, InstructionDecoder};

use super::{VerificationError, VerificationErrorKind, VerificationResource, usize_to_u64};

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(test)]
static FAIL_BOUNDARY_BITMAP_RESERVATION: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static FAIL_INSTRUCTION_RESERVATION: AtomicBool = AtomicBool::new(false);

/// Completely decoded instructions and their exact byte-boundary map.
///
/// This private stage is structural evidence only. It cannot construct a
/// public control-flow certificate or authorize execution.
pub(super) struct PredecodedBody {
    pub(super) instructions: Vec<DecodedInstruction>,
    pub(super) instruction_start_bitmap: Vec<u64>,
}

/// Decodes the complete body and records every instruction boundary.
///
/// The result is all-or-nothing: neither the decoded vector nor its boundary
/// map escapes when decoding, limits, or allocation fail.
pub(super) fn predecode_complete(
    bytecode: &[u8],
    max_instructions: u32,
) -> Result<PredecodedBody, VerificationError> {
    let mut instruction_start_bitmap = allocate_boundary_bitmap(bytecode.len())?;
    let mut instructions = Vec::new();
    let instruction_stream = InstructionDecoder::new(bytecode);

    for item in instruction_stream {
        let decoded = item.map_err(VerificationError::from_decode)?;
        let observed = usize_to_u64(instructions.len()).saturating_add(1);
        if observed > u64::from(max_instructions) {
            return Err(VerificationError::at_instruction(
                decoded,
                VerificationErrorKind::LimitExceeded {
                    resource: VerificationResource::Instructions,
                    limit: u64::from(max_instructions),
                    observed,
                },
            ));
        }
        mark_instruction_start(&mut instruction_start_bitmap, bytecode.len(), decoded.pc())?;
        if instructions.len() == instructions.capacity() {
            try_reserve_instructions(&mut instructions, 1).map_err(|_| {
                VerificationError::at_instruction(
                    decoded,
                    VerificationErrorKind::AllocationFailed {
                        resource: VerificationResource::Instructions,
                        requested: 1,
                    },
                )
            })?;
        }
        instructions.push(decoded);
    }

    Ok(PredecodedBody {
        instructions,
        instruction_start_bitmap,
    })
}

fn allocate_boundary_bitmap(bytecode_len: usize) -> Result<Vec<u64>, VerificationError> {
    let word_count = bytecode_len.checked_add(63).ok_or_else(|| {
        VerificationError::root(VerificationErrorKind::AllocationFailed {
            resource: VerificationResource::InstructionBoundaryWords,
            requested: u64::MAX,
        })
    })? / 64;
    let mut bitmap = Vec::new();
    try_reserve_boundary_words(&mut bitmap, word_count).map_err(|_| {
        VerificationError::root(VerificationErrorKind::AllocationFailed {
            resource: VerificationResource::InstructionBoundaryWords,
            requested: usize_to_u64(word_count),
        })
    })?;
    bitmap.resize(word_count, 0);
    Ok(bitmap)
}

fn try_reserve_boundary_words(
    bitmap: &mut Vec<u64>,
    additional: usize,
) -> Result<(), std::collections::TryReserveError> {
    #[cfg(test)]
    if FAIL_BOUNDARY_BITMAP_RESERVATION.swap(false, Ordering::Relaxed) {
        return Vec::<u64>::new().try_reserve(usize::MAX);
    }
    bitmap.try_reserve_exact(additional)
}

fn try_reserve_instructions(
    instructions: &mut Vec<DecodedInstruction>,
    additional: usize,
) -> Result<(), std::collections::TryReserveError> {
    #[cfg(test)]
    if FAIL_INSTRUCTION_RESERVATION.swap(false, Ordering::Relaxed) {
        return Vec::<DecodedInstruction>::new().try_reserve(usize::MAX);
    }
    instructions.try_reserve(additional)
}

fn mark_instruction_start(
    bitmap: &mut [u64],
    bytecode_len: usize,
    pc: BytecodePc,
) -> Result<(), VerificationError> {
    let offset = usize::try_from(pc.get())
        .map_err(|_| VerificationError::from_decode(DecodeError::PcNotRepresentable { pc }))?;
    let word = bitmap.get_mut(offset / 64).ok_or_else(|| {
        VerificationError::from_decode(DecodeError::PcOutOfBounds { pc, bytecode_len })
    })?;
    *word |= 1_u64 << (offset % 64);
    Ok(())
}

pub(super) fn is_instruction_start(bitmap: &[u64], bytecode_len: usize, pc: BytecodePc) -> bool {
    let Ok(offset) = usize::try_from(pc.get()) else {
        return false;
    };
    if offset >= bytecode_len {
        return false;
    }
    bitmap
        .get(offset / 64)
        .is_some_and(|word| word & (1_u64 << (offset % 64)) != 0)
}

#[cfg(test)]
mod tests {
    use crate::{BytecodeBuilder, BytecodePc, FinalOpcode, Operands};

    use super::{
        FAIL_BOUNDARY_BITMAP_RESERVATION, FAIL_INSTRUCTION_RESERVATION, Ordering,
        VerificationErrorKind, VerificationResource, allocate_boundary_bitmap, predecode_complete,
    };

    #[test]
    fn allocation_failures_preserve_exact_resource_and_location() {
        let overflow = allocate_boundary_bitmap(usize::MAX)
            .expect_err("bitmap word-count overflow must fail closed");
        assert_eq!(overflow.pc(), None);
        assert_eq!(overflow.opcode(), None);
        assert_eq!(
            overflow.kind(),
            &VerificationErrorKind::AllocationFailed {
                resource: VerificationResource::InstructionBoundaryWords,
                requested: u64::MAX,
            }
        );

        FAIL_BOUNDARY_BITMAP_RESERVATION.store(true, Ordering::Relaxed);
        let Err(bitmap) = predecode_complete(&[FinalOpcode::ReturnUndef as u8], 1) else {
            panic!("injected bitmap reservation failure must fail closed");
        };
        assert_eq!(bitmap.pc(), None);
        assert_eq!(bitmap.opcode(), None);
        assert_eq!(
            bitmap.kind(),
            &VerificationErrorKind::AllocationFailed {
                resource: VerificationResource::InstructionBoundaryWords,
                requested: 1,
            }
        );

        let mut builder = BytecodeBuilder::new();
        builder
            .push(FinalOpcode::ReturnUndef, Operands::None)
            .expect("test instruction must encode");
        FAIL_INSTRUCTION_RESERVATION.store(true, Ordering::Relaxed);
        let Err(instructions) = predecode_complete(&builder.into_bytes(), 1) else {
            panic!("injected instruction reservation failure must fail closed");
        };
        assert_eq!(instructions.pc(), Some(BytecodePc::ZERO));
        assert_eq!(instructions.opcode(), Some(FinalOpcode::ReturnUndef));
        assert_eq!(
            instructions.kind(),
            &VerificationErrorKind::AllocationFailed {
                resource: VerificationResource::Instructions,
                requested: 1,
            }
        );
    }
}
