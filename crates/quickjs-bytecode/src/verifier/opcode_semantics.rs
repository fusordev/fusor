use crate::FinalOpcode;

use super::UnsupportedVerifierFeature;

/// Capability policy for the semantics this control-flow verifier can prove.
///
/// This is deliberately separate from immutable opcode encoding metadata.
/// Every final opcode must remain in exactly one explicit policy partition.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OpcodeSemantics {
    Invalid,
    Ordinary,
    Conditional,
    Jump,
    Terminate,
    Unsupported(UnsupportedVerifierFeature, SuccessorShape),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SuccessorShape {
    Fallthrough,
    Branch,
    Jump,
    Terminate,
}

impl OpcodeSemantics {
    pub(super) const fn successor_shape(self) -> Option<SuccessorShape> {
        match self {
            Self::Invalid => None,
            Self::Ordinary => Some(SuccessorShape::Fallthrough),
            Self::Conditional => Some(SuccessorShape::Branch),
            Self::Jump => Some(SuccessorShape::Jump),
            Self::Terminate => Some(SuccessorShape::Terminate),
            Self::Unsupported(_, shape) => Some(shape),
        }
    }
}

#[allow(clippy::too_many_lines)]
pub(super) const fn opcode_semantics(opcode: FinalOpcode) -> OpcodeSemantics {
    match opcode {
        FinalOpcode::Invalid => OpcodeSemantics::Invalid,

        FinalOpcode::IfFalse
        | FinalOpcode::IfTrue
        | FinalOpcode::IfFalse8
        | FinalOpcode::IfTrue8
        | FinalOpcode::Catch
        | FinalOpcode::Gosub
        | FinalOpcode::WithGetVar
        | FinalOpcode::WithDeleteVar
        | FinalOpcode::WithMakeRef
        | FinalOpcode::WithGetRef => OpcodeSemantics::Conditional,

        FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16 => OpcodeSemantics::Jump,

        FinalOpcode::TailCall
        | FinalOpcode::TailCallMethod
        | FinalOpcode::Return
        | FinalOpcode::ReturnUndef
        | FinalOpcode::ReturnAsync
        | FinalOpcode::Throw
        | FinalOpcode::ThrowError
        | FinalOpcode::Ret => OpcodeSemantics::Terminate,

        FinalOpcode::PushConst
        | FinalOpcode::FClosure
        | FinalOpcode::PushConst8
        | FinalOpcode::FClosure8 => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::ConstantPoolTyping,
            SuccessorShape::Fallthrough,
        ),

        FinalOpcode::DefineClassComputed => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::RawFunctionStack,
            SuccessorShape::Fallthrough,
        ),

        FinalOpcode::Eval | FinalOpcode::ApplyEval => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::EvalScopeMetadata,
            SuccessorShape::Fallthrough,
        ),

        FinalOpcode::CloseLoc
        | FinalOpcode::MakeLocRef
        | FinalOpcode::MakeArgRef
        | FinalOpcode::MakeVarRefRef
        | FinalOpcode::MakeVarRef => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::CapturedBindingMetadata,
            SuccessorShape::Fallthrough,
        ),

        FinalOpcode::WithPutVar => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::WithEnvironmentBranches,
            SuccessorShape::Branch,
        ),

        FinalOpcode::ForOfStart
        | FinalOpcode::ForAwaitOfStart
        | FinalOpcode::ForOfNext
        | FinalOpcode::ForAwaitOfNext
        | FinalOpcode::IteratorGetValueDone
        | FinalOpcode::IteratorClose
        | FinalOpcode::IteratorNext
        | FinalOpcode::IteratorCall => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::IteratorMarkers,
            SuccessorShape::Fallthrough,
        ),

        FinalOpcode::CopyDataProperties => OpcodeSemantics::Unsupported(
            UnsupportedVerifierFeature::PackedStackOffsets,
            SuccessorShape::Fallthrough,
        ),

        FinalOpcode::DefineClass
        | FinalOpcode::NipCatch
        | FinalOpcode::PushI32
        | FinalOpcode::InitialYield
        | FinalOpcode::Yield
        | FinalOpcode::YieldStar
        | FinalOpcode::AsyncYieldStar
        | FinalOpcode::Await
        | FinalOpcode::PushAtomValue
        | FinalOpcode::PrivateSymbol
        | FinalOpcode::Undefined
        | FinalOpcode::Null
        | FinalOpcode::PushThis
        | FinalOpcode::PushFalse
        | FinalOpcode::PushTrue
        | FinalOpcode::Object
        | FinalOpcode::SpecialObject
        | FinalOpcode::Rest
        | FinalOpcode::Drop
        | FinalOpcode::Nip
        | FinalOpcode::Nip1
        | FinalOpcode::Dup
        | FinalOpcode::Dup1
        | FinalOpcode::Dup2
        | FinalOpcode::Dup3
        | FinalOpcode::Insert2
        | FinalOpcode::Insert3
        | FinalOpcode::Insert4
        | FinalOpcode::Perm3
        | FinalOpcode::Perm4
        | FinalOpcode::Perm5
        | FinalOpcode::Swap
        | FinalOpcode::Swap2
        | FinalOpcode::Rot3l
        | FinalOpcode::Rot3r
        | FinalOpcode::Rot4l
        | FinalOpcode::Rot5l
        | FinalOpcode::CallConstructor
        | FinalOpcode::Call
        | FinalOpcode::CallMethod
        | FinalOpcode::ArrayFrom
        | FinalOpcode::Apply
        | FinalOpcode::CheckCtorReturn
        | FinalOpcode::CheckCtor
        | FinalOpcode::InitCtor
        | FinalOpcode::CheckBrand
        | FinalOpcode::AddBrand
        | FinalOpcode::RegExp
        | FinalOpcode::GetSuper
        | FinalOpcode::Import
        | FinalOpcode::GetVarUndef
        | FinalOpcode::GetVar
        | FinalOpcode::PutVar
        | FinalOpcode::PutVarInit
        | FinalOpcode::GetRefValue
        | FinalOpcode::PutRefValue
        | FinalOpcode::GetField
        | FinalOpcode::GetField2
        | FinalOpcode::PutField
        | FinalOpcode::GetPrivateField
        | FinalOpcode::PutPrivateField
        | FinalOpcode::DefinePrivateField
        | FinalOpcode::GetArrayEl
        | FinalOpcode::GetArrayEl2
        | FinalOpcode::GetArrayEl3
        | FinalOpcode::PutArrayEl
        | FinalOpcode::GetSuperValue
        | FinalOpcode::PutSuperValue
        | FinalOpcode::DefineField
        | FinalOpcode::SetName
        | FinalOpcode::SetNameComputed
        | FinalOpcode::SetProto
        | FinalOpcode::SetHomeObject
        | FinalOpcode::DefineArrayEl
        | FinalOpcode::Append
        | FinalOpcode::DefineMethod
        | FinalOpcode::DefineMethodComputed
        | FinalOpcode::GetLoc
        | FinalOpcode::PutLoc
        | FinalOpcode::SetLoc
        | FinalOpcode::GetArg
        | FinalOpcode::PutArg
        | FinalOpcode::SetArg
        | FinalOpcode::GetVarRef
        | FinalOpcode::PutVarRef
        | FinalOpcode::SetVarRef
        | FinalOpcode::SetLocUninitialized
        | FinalOpcode::GetLocCheck
        | FinalOpcode::PutLocCheck
        | FinalOpcode::SetLocCheck
        | FinalOpcode::PutLocCheckInit
        | FinalOpcode::GetLocCheckThis
        | FinalOpcode::GetVarRefCheck
        | FinalOpcode::PutVarRefCheck
        | FinalOpcode::PutVarRefCheckInit
        | FinalOpcode::ToObject
        | FinalOpcode::ToPropKey
        | FinalOpcode::ForInStart
        | FinalOpcode::ForInNext
        | FinalOpcode::IteratorCheckObject
        | FinalOpcode::Neg
        | FinalOpcode::Plus
        | FinalOpcode::Dec
        | FinalOpcode::Inc
        | FinalOpcode::PostDec
        | FinalOpcode::PostInc
        | FinalOpcode::DecLoc
        | FinalOpcode::IncLoc
        | FinalOpcode::AddLoc
        | FinalOpcode::Not
        | FinalOpcode::Lnot
        | FinalOpcode::Typeof
        | FinalOpcode::Delete
        | FinalOpcode::DeleteVar
        | FinalOpcode::Mul
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
        | FinalOpcode::InstanceOf
        | FinalOpcode::In
        | FinalOpcode::Eq
        | FinalOpcode::Neq
        | FinalOpcode::StrictEq
        | FinalOpcode::StrictNeq
        | FinalOpcode::And
        | FinalOpcode::Xor
        | FinalOpcode::Or
        | FinalOpcode::IsUndefinedOrNull
        | FinalOpcode::PrivateIn
        | FinalOpcode::PushBigIntI32
        | FinalOpcode::Nop
        | FinalOpcode::PushMinus1
        | FinalOpcode::Push0
        | FinalOpcode::Push1
        | FinalOpcode::Push2
        | FinalOpcode::Push3
        | FinalOpcode::Push4
        | FinalOpcode::Push5
        | FinalOpcode::Push6
        | FinalOpcode::Push7
        | FinalOpcode::PushI8
        | FinalOpcode::PushI16
        | FinalOpcode::PushEmptyString
        | FinalOpcode::GetLoc8
        | FinalOpcode::PutLoc8
        | FinalOpcode::SetLoc8
        | FinalOpcode::GetLoc0
        | FinalOpcode::GetLoc1
        | FinalOpcode::GetLoc2
        | FinalOpcode::GetLoc3
        | FinalOpcode::PutLoc0
        | FinalOpcode::PutLoc1
        | FinalOpcode::PutLoc2
        | FinalOpcode::PutLoc3
        | FinalOpcode::SetLoc0
        | FinalOpcode::SetLoc1
        | FinalOpcode::SetLoc2
        | FinalOpcode::SetLoc3
        | FinalOpcode::GetArg0
        | FinalOpcode::GetArg1
        | FinalOpcode::GetArg2
        | FinalOpcode::GetArg3
        | FinalOpcode::PutArg0
        | FinalOpcode::PutArg1
        | FinalOpcode::PutArg2
        | FinalOpcode::PutArg3
        | FinalOpcode::SetArg0
        | FinalOpcode::SetArg1
        | FinalOpcode::SetArg2
        | FinalOpcode::SetArg3
        | FinalOpcode::GetVarRef0
        | FinalOpcode::GetVarRef1
        | FinalOpcode::GetVarRef2
        | FinalOpcode::GetVarRef3
        | FinalOpcode::PutVarRef0
        | FinalOpcode::PutVarRef1
        | FinalOpcode::PutVarRef2
        | FinalOpcode::PutVarRef3
        | FinalOpcode::SetVarRef0
        | FinalOpcode::SetVarRef1
        | FinalOpcode::SetVarRef2
        | FinalOpcode::SetVarRef3
        | FinalOpcode::GetLength
        | FinalOpcode::Call0
        | FinalOpcode::Call1
        | FinalOpcode::Call2
        | FinalOpcode::Call3
        | FinalOpcode::IsUndefined
        | FinalOpcode::IsNull
        | FinalOpcode::TypeofIsUndefined
        | FinalOpcode::TypeofIsFunction => OpcodeSemantics::Ordinary,
    }
}

#[cfg(test)]
mod tests {
    use super::{OpcodeSemantics, opcode_semantics};
    use crate::{ALL_FINAL_OPCODES, FinalOpcode};

    #[test]
    fn final_opcode_capability_partition_is_exhaustive_and_counted_from_the_table() {
        let mut invalid = 0;
        let mut supported = 0;
        let mut unsupported = 0;

        for &opcode in ALL_FINAL_OPCODES {
            match opcode_semantics(opcode) {
                OpcodeSemantics::Invalid => invalid += 1,
                OpcodeSemantics::Unsupported(_, _) => unsupported += 1,
                OpcodeSemantics::Ordinary
                | OpcodeSemantics::Conditional
                | OpcodeSemantics::Jump
                | OpcodeSemantics::Terminate => supported += 1,
            }
        }

        assert_eq!(invalid, 1);
        assert_eq!(supported, 221);
        assert_eq!(unsupported, 22);
        assert_eq!(invalid + supported + unsupported, ALL_FINAL_OPCODES.len());
        assert_eq!(
            opcode_semantics(FinalOpcode::Invalid),
            OpcodeSemantics::Invalid
        );
    }
}
