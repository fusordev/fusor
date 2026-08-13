use crate::{DecodedInstruction, FinalOpcode, OperandFormat, Operands};

use super::{
    CompilerConstantKind, CompilerConstantLayout, FunctionIndexDomains, OperandIndexDomain,
    SecondaryOperandField, UnsupportedVerifierFeature, VerificationError, VerificationErrorKind,
    layouts::ValidatedCompilerCaptureLayout,
};

pub(super) fn validate_compiler_constant_kind(
    decoded: DecodedInstruction,
    constant_layout: &CompilerConstantLayout,
) -> Result<(), VerificationError> {
    let instruction = decoded.instruction();
    let (index, expected) = match (instruction.opcode(), instruction.operands()) {
        (FinalOpcode::PushConst, Operands::Const(index)) => (index, CompilerConstantKind::Value),
        (FinalOpcode::PushConst8, Operands::Const8(index)) => {
            (u32::from(index), CompilerConstantKind::Value)
        }
        (FinalOpcode::FClosure, Operands::Const(index)) => (index, CompilerConstantKind::Function),
        (FinalOpcode::FClosure8, Operands::Const8(index)) => {
            (u32::from(index), CompilerConstantKind::Function)
        }
        _ => {
            return Err(VerificationError::at_instruction(
                decoded,
                VerificationErrorKind::MissingControlFlowOperand {
                    expected: instruction.opcode().metadata().operand_format(),
                },
            ));
        }
    };
    let actual = constant_layout.kind(index).ok_or_else(|| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::IndexOutOfBounds {
                domain: OperandIndexDomain::ConstantPool,
                index,
                len: u32::try_from(constant_layout.kinds.len()).unwrap_or(u32::MAX),
            },
        )
    })?;
    if expected == CompilerConstantKind::Value && actual == CompilerConstantKind::Function {
        return Err(VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::UnsupportedOpcodeSemantics {
                feature: UnsupportedVerifierFeature::RawFunctionStack,
            },
        ));
    }
    if actual != expected {
        return Err(VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::CompilerConstantKindMismatch {
                index,
                expected,
                actual,
            },
        ));
    }
    Ok(())
}

pub(super) fn validate_compiler_close_loc(
    decoded: DecodedInstruction,
    capture_layout: &ValidatedCompilerCaptureLayout,
) -> Result<(), VerificationError> {
    let Operands::Loc(local) = decoded.instruction().operands() else {
        return Err(VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::MissingControlFlowOperand {
                expected: OperandFormat::Loc,
            },
        ));
    };
    let local = u32::from(local);
    if !capture_layout.is_scoped_local(local) {
        return Err(VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::CloseLocRequiresScopedCapture { local },
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(super) fn validate_operand_indices(
    decoded: DecodedInstruction,
    domains: FunctionIndexDomains,
) -> Result<(), VerificationError> {
    let instruction = decoded.instruction();
    let operands = instruction.operands();

    if let Some(index) = operands.atom_pool_index() {
        validate_index(
            decoded,
            OperandIndexDomain::AtomPool,
            index.get(),
            domains.atom_pool_len,
        )?;
    }

    match operands {
        Operands::Const(index) => validate_index(
            decoded,
            OperandIndexDomain::ConstantPool,
            index,
            domains.constant_pool_len,
        )?,
        Operands::Const8(index) => validate_index(
            decoded,
            OperandIndexDomain::ConstantPool,
            u32::from(index),
            domains.constant_pool_len,
        )?,
        Operands::Loc(index) => validate_index(
            decoded,
            OperandIndexDomain::Local,
            u32::from(index),
            domains.local_count,
        )?,
        Operands::Loc8(index) => validate_index(
            decoded,
            OperandIndexDomain::Local,
            u32::from(index),
            domains.local_count,
        )?,
        Operands::Arg(index) => validate_index(
            decoded,
            OperandIndexDomain::Argument,
            u32::from(index),
            domains.argument_count,
        )?,
        Operands::VarRef(index) => validate_index(
            decoded,
            OperandIndexDomain::ClosureVariable,
            u32::from(index),
            domains.closure_var_count,
        )?,
        Operands::AtomU16 { value, .. } => match instruction.opcode() {
            FinalOpcode::MakeLocRef => validate_index(
                decoded,
                OperandIndexDomain::Local,
                u32::from(value),
                domains.local_count,
            )?,
            FinalOpcode::MakeArgRef => validate_index(
                decoded,
                OperandIndexDomain::Argument,
                u32::from(value),
                domains.argument_count,
            )?,
            FinalOpcode::MakeVarRefRef => validate_index(
                decoded,
                OperandIndexDomain::ClosureVariable,
                u32::from(value),
                domains.closure_var_count,
            )?,
            _ => {}
        },
        Operands::NoneLoc => {
            let index = implied_local_index(instruction.opcode()).ok_or_else(|| {
                VerificationError::at_instruction(
                    decoded,
                    VerificationErrorKind::MissingControlFlowOperand {
                        expected: OperandFormat::NoneLoc,
                    },
                )
            })?;
            validate_index(
                decoded,
                OperandIndexDomain::Local,
                index,
                domains.local_count,
            )?;
        }
        Operands::NoneArg => {
            let index = implied_argument_index(instruction.opcode()).ok_or_else(|| {
                VerificationError::at_instruction(
                    decoded,
                    VerificationErrorKind::MissingControlFlowOperand {
                        expected: OperandFormat::NoneArg,
                    },
                )
            })?;
            validate_index(
                decoded,
                OperandIndexDomain::Argument,
                index,
                domains.argument_count,
            )?;
        }
        Operands::NoneVarRef => {
            let index = implied_closure_variable_index(instruction.opcode()).ok_or_else(|| {
                VerificationError::at_instruction(
                    decoded,
                    VerificationErrorKind::MissingControlFlowOperand {
                        expected: OperandFormat::NoneVarRef,
                    },
                )
            })?;
            validate_index(
                decoded,
                OperandIndexDomain::ClosureVariable,
                index,
                domains.closure_var_count,
            )?;
        }
        Operands::None
        | Operands::NoneInt
        | Operands::U8(_)
        | Operands::I8(_)
        | Operands::Label8(_)
        | Operands::U16(_)
        | Operands::I16(_)
        | Operands::Label16(_)
        | Operands::NPop { .. }
        | Operands::NPopX
        | Operands::NPopU16 { .. }
        | Operands::U32(_)
        | Operands::I32(_)
        | Operands::Label(_)
        | Operands::Atom(_)
        | Operands::AtomU8 { .. }
        | Operands::AtomLabelU8 { .. }
        | Operands::AtomLabelU16 { .. }
        | Operands::LabelU16 { .. } => {}
    }

    Ok(())
}

fn validate_index(
    decoded: DecodedInstruction,
    domain: OperandIndexDomain,
    index: u32,
    len: u32,
) -> Result<(), VerificationError> {
    if index >= len {
        return Err(VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::IndexOutOfBounds { domain, index, len },
        ));
    }
    Ok(())
}

fn implied_local_index(opcode: FinalOpcode) -> Option<u32> {
    match opcode {
        FinalOpcode::GetLoc0 | FinalOpcode::PutLoc0 | FinalOpcode::SetLoc0 => Some(0),
        FinalOpcode::GetLoc1 | FinalOpcode::PutLoc1 | FinalOpcode::SetLoc1 => Some(1),
        FinalOpcode::GetLoc2 | FinalOpcode::PutLoc2 | FinalOpcode::SetLoc2 => Some(2),
        FinalOpcode::GetLoc3 | FinalOpcode::PutLoc3 | FinalOpcode::SetLoc3 => Some(3),
        _ => None,
    }
}

fn implied_argument_index(opcode: FinalOpcode) -> Option<u32> {
    match opcode {
        FinalOpcode::GetArg0 | FinalOpcode::PutArg0 | FinalOpcode::SetArg0 => Some(0),
        FinalOpcode::GetArg1 | FinalOpcode::PutArg1 | FinalOpcode::SetArg1 => Some(1),
        FinalOpcode::GetArg2 | FinalOpcode::PutArg2 | FinalOpcode::SetArg2 => Some(2),
        FinalOpcode::GetArg3 | FinalOpcode::PutArg3 | FinalOpcode::SetArg3 => Some(3),
        _ => None,
    }
}

fn implied_closure_variable_index(opcode: FinalOpcode) -> Option<u32> {
    match opcode {
        FinalOpcode::GetVarRef0 | FinalOpcode::PutVarRef0 | FinalOpcode::SetVarRef0 => Some(0),
        FinalOpcode::GetVarRef1 | FinalOpcode::PutVarRef1 | FinalOpcode::SetVarRef1 => Some(1),
        FinalOpcode::GetVarRef2 | FinalOpcode::PutVarRef2 | FinalOpcode::SetVarRef2 => Some(2),
        FinalOpcode::GetVarRef3 | FinalOpcode::PutVarRef3 | FinalOpcode::SetVarRef3 => Some(3),
        _ => None,
    }
}

pub(super) fn validate_secondary_operands(
    decoded: DecodedInstruction,
    domains: FunctionIndexDomains,
) -> Result<(), VerificationError> {
    let instruction = decoded.instruction();
    let invalid = |field, value| {
        VerificationError::at_instruction(
            decoded,
            VerificationErrorKind::InvalidSecondaryOperand { field, value },
        )
    };

    match (instruction.opcode(), instruction.operands()) {
        (FinalOpcode::SpecialObject, Operands::U8(value)) if value > 6 => Err(invalid(
            SecondaryOperandField::SpecialObjectKind,
            u32::from(value),
        )),
        (FinalOpcode::Rest, Operands::U16(value)) if u32::from(value) > domains.argument_count => {
            Err(invalid(
                SecondaryOperandField::RestFirstArgument,
                u32::from(value),
            ))
        }
        (FinalOpcode::Apply, Operands::U16(value)) if value > 2 => {
            Err(invalid(SecondaryOperandField::ApplyMagic, u32::from(value)))
        }
        (FinalOpcode::ThrowError, Operands::AtomU8 { value, .. }) if value > 4 => Err(invalid(
            SecondaryOperandField::ThrowErrorKind,
            u32::from(value),
        )),
        (FinalOpcode::DefineMethod, Operands::AtomU8 { value, .. })
        | (FinalOpcode::DefineMethodComputed, Operands::U8(value))
            if value & !0b111 != 0 || value & 0b11 == 0b11 =>
        {
            Err(invalid(
                SecondaryOperandField::DefineMethodFlags,
                u32::from(value),
            ))
        }
        (FinalOpcode::DefinePrivateField, Operands::U8(value)) if value > 3 => Err(invalid(
            SecondaryOperandField::DefinePrivateFieldKind,
            u32::from(value),
        )),
        (
            FinalOpcode::DefineClass | FinalOpcode::DefineClassComputed,
            Operands::AtomU8 { value, .. },
        ) if value > 3 => Err(invalid(
            SecondaryOperandField::DefineClassFlags,
            u32::from(value),
        )),
        (
            FinalOpcode::WithGetVar
            | FinalOpcode::WithPutVar
            | FinalOpcode::WithDeleteVar
            | FinalOpcode::WithMakeRef
            | FinalOpcode::WithGetRef,
            Operands::AtomLabelU8 { value, .. },
        ) if value > 1 => Err(invalid(SecondaryOperandField::IsWith, u32::from(value))),
        (FinalOpcode::IteratorCall, Operands::U8(value)) if value > 2 => Err(invalid(
            SecondaryOperandField::IteratorCallFlags,
            u32::from(value),
        )),
        _ => Ok(()),
    }
}
