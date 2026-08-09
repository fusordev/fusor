use crate::{
    DecodeError, DecodedInstruction, FinalOpcode,
    function::{FunctionKind, FunctionKindRequirement},
};

use super::{
    CompilerConstantLayout, FunctionIndexDomains, InstructionIndex, VerificationError,
    VerificationErrorKind, VerificationResource, VerifiedInstruction, VerifiedSuccessors,
    VerifiedSuccessorsRepr,
    layouts::ValidatedCompilerCaptureLayout,
    opcode_semantics::{OpcodeSemantics, SuccessorShape, opcode_semantics},
    operands::{
        validate_compiler_close_loc, validate_compiler_constant_kind, validate_operand_indices,
        validate_secondary_operands,
    },
    targets::{
        require_encoded_target, resolve_fallthrough, resolve_relative_target,
        validate_gosub_continuation,
    },
    usize_to_u64,
};

/// Structurally checked successors before ordinary stack dataflow.
pub(super) struct StructurallyVerifiedControlFlow {
    instructions: Vec<VerifiedInstruction>,
    function_kind: FunctionKind,
}

impl StructurallyVerifiedControlFlow {
    pub(super) fn into_parts(self) -> (Vec<VerifiedInstruction>, FunctionKind) {
        (self.instructions, self.function_kind)
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "decoded control-flow domains and compiler-only metadata form one static-semantics boundary"
)]
pub(super) fn validate_static_semantics(
    decoded: &[DecodedInstruction],
    instruction_start_bitmap: &[u64],
    bytecode_len: usize,
    domains: FunctionIndexDomains,
    function_kind: FunctionKind,
    compiler_capture_layout: Option<&ValidatedCompilerCaptureLayout>,
    compiler_constant_layout: Option<&CompilerConstantLayout>,
    compiler_generated: bool,
) -> Result<StructurallyVerifiedControlFlow, VerificationError> {
    let mut verified = Vec::new();
    verified.try_reserve_exact(decoded.len()).map_err(|_| {
        VerificationError::root(VerificationErrorKind::AllocationFailed {
            resource: VerificationResource::Instructions,
            requested: usize_to_u64(decoded.len()),
        })
    })?;

    for &current in decoded {
        validate_operand_indices(current, domains)?;
        validate_secondary_operands(current, domains)?;

        let target =
            resolve_relative_target(current, decoded, instruction_start_bitmap, bytecode_len)?;
        validate_gosub_continuation(current, decoded, instruction_start_bitmap, bytecode_len)?;

        let semantics = opcode_semantics(current.instruction().opcode());
        let successors = resolve_static_successors(
            current,
            semantics,
            target,
            decoded,
            instruction_start_bitmap,
            bytecode_len,
        )?;

        verified.push(VerifiedInstruction {
            decoded: current,
            entry_stack_depth: None,
            successors,
        });
    }

    for instruction in &verified {
        let decoded = instruction.decoded;
        if let OpcodeSemantics::Unsupported(feature, _) =
            opcode_semantics(decoded.instruction().opcode())
        {
            if decoded.instruction().opcode() == FinalOpcode::CloseLoc {
                if let Some(capture_layout) = compiler_capture_layout {
                    validate_compiler_close_loc(decoded, capture_layout)?;
                } else {
                    return Err(VerificationError::at_instruction(
                        decoded,
                        VerificationErrorKind::UnsupportedOpcodeSemantics { feature },
                    ));
                }
            } else if matches!(
                decoded.instruction().opcode(),
                FinalOpcode::PushConst
                    | FinalOpcode::FClosure
                    | FinalOpcode::PushConst8
                    | FinalOpcode::FClosure8
            ) {
                if let Some(constant_layout) = compiler_constant_layout {
                    validate_compiler_constant_kind(decoded, constant_layout)?;
                } else {
                    return Err(VerificationError::at_instruction(
                        decoded,
                        VerificationErrorKind::UnsupportedOpcodeSemantics { feature },
                    ));
                }
            } else if compiler_generated
                && matches!(
                    decoded.instruction().opcode(),
                    FinalOpcode::ForOfStart
                        | FinalOpcode::ForAwaitOfStart
                        | FinalOpcode::ForOfNext
                        | FinalOpcode::IteratorClose
                        | FinalOpcode::IteratorNext
                        | FinalOpcode::IteratorCall
                        | FinalOpcode::CopyDataProperties
                        | FinalOpcode::Eval
                        | FinalOpcode::ApplyEval
                )
            {
                // Compiler-owned control flow is still non-executable. The
                // whole-function verifier proves the exact synchronous
                // iterator record and eval scope metadata before granting
                // authority. `copy_data_properties`' packed stack offsets keep
                // their net-zero effect (three operand slots popped and
                // re-pushed).
            } else {
                return Err(VerificationError::at_instruction(
                    decoded,
                    VerificationErrorKind::UnsupportedOpcodeSemantics { feature },
                ));
            }
        }
        validate_function_kind_opcode(decoded, function_kind)?;
    }

    Ok(StructurallyVerifiedControlFlow {
        instructions: verified,
        function_kind,
    })
}

fn validate_function_kind_opcode(
    decoded: DecodedInstruction,
    function_kind: FunctionKind,
) -> Result<(), VerificationError> {
    let requirement = match decoded.instruction().opcode() {
        FinalOpcode::TailCall
        | FinalOpcode::TailCallMethod
        | FinalOpcode::Return
        | FinalOpcode::ReturnUndef => FunctionKindRequirement::Normal,
        FinalOpcode::InitialYield | FinalOpcode::Yield => FunctionKindRequirement::Generator,
        FinalOpcode::YieldStar => FunctionKindRequirement::SynchronousGenerator,
        FinalOpcode::AsyncYieldStar => FunctionKindRequirement::AsyncGenerator,
        FinalOpcode::Await => FunctionKindRequirement::Async,
        FinalOpcode::ReturnAsync => FunctionKindRequirement::NonNormal,
        _ => return Ok(()),
    };

    if !requirement.accepts(function_kind) {
        return Err(VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::OpcodeNotAllowedForFunctionKind {
                kind: function_kind,
                requirement,
            },
        ));
    }

    Ok(())
}

fn resolve_static_successors(
    current: DecodedInstruction,
    semantics: OpcodeSemantics,
    target: Option<InstructionIndex>,
    instructions: &[DecodedInstruction],
    bitmap: &[u64],
    bytecode_len: usize,
) -> Result<VerifiedSuccessors, VerificationError> {
    let Some(shape) = semantics.successor_shape() else {
        return Err(VerificationError::at_instruction(
            current,
            VerificationErrorKind::Decode(DecodeError::InvalidOpcode {
                pc: current.pc(),
                opcode_byte: FinalOpcode::Invalid.encoded_byte(),
                source: crate::FinalOpcodeDecodeError::ReservedInvalid,
            }),
        ));
    };

    match shape {
        SuccessorShape::Fallthrough => Ok(VerifiedSuccessors(VerifiedSuccessorsRepr::Fallthrough(
            resolve_fallthrough(current, instructions, bitmap, bytecode_len)?,
        ))),
        SuccessorShape::Branch => {
            let taken = require_encoded_target(current, target)?;
            let not_taken = resolve_fallthrough(current, instructions, bitmap, bytecode_len)?;
            Ok(VerifiedSuccessors(VerifiedSuccessorsRepr::Branch {
                taken,
                not_taken,
            }))
        }
        SuccessorShape::Jump => Ok(VerifiedSuccessors(VerifiedSuccessorsRepr::Jump(
            require_encoded_target(current, target)?,
        ))),
        SuccessorShape::Terminate => Ok(VerifiedSuccessors(VerifiedSuccessorsRepr::Terminate)),
    }
}
