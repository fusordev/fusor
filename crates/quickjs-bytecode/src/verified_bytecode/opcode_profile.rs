#[allow(
    clippy::too_many_lines,
    reason = "opcode admission and function-kind restrictions share one auditable pass"
)]
fn verify_supported_opcodes(
    id: FunctionTemplateId,
    flow: &VerifiedControlFlow,
    metadata: &UnverifiedFunctionMetadata,
) -> Result<(), BytecodeVerificationError> {
    let executable_kind = metadata.executable_kind;
    let mut arguments_object_count = 0_u8;
    let mut arguments_object_initializer = None;
    let mut rest_parameter_count = 0_u8;
    let generator = matches!(
        executable_kind,
        CompilerExecutableKind::GeneratorFunction
            | CompilerExecutableKind::GeneratorMethod
            | CompilerExecutableKind::AsyncGeneratorFunction
            | CompilerExecutableKind::AsyncGeneratorMethod
    );
    let asynchronous = matches!(
        executable_kind,
        CompilerExecutableKind::AsyncArrow
            | CompilerExecutableKind::AsyncFunction
            | CompilerExecutableKind::AsyncMethod
            | CompilerExecutableKind::AsyncGeneratorFunction
            | CompilerExecutableKind::AsyncGeneratorMethod
    );
    let async_generator = matches!(
        executable_kind,
        CompilerExecutableKind::AsyncGeneratorFunction
            | CompilerExecutableKind::AsyncGeneratorMethod
    );
    let static_field_super = metadata
        .variables
        .iter()
        .map(VariableDefinition::policy)
        .chain(
            metadata
                .closures
                .iter()
                .map(ClosureVariableDefinition::policy),
        )
        .any(|policy| policy.kind() == CompilerBindingKind::ClassStaticReceiver);
    let super_property_authorized = (executable_kind == CompilerExecutableKind::DirectEvalScript
        && (flow.function_header().flags().super_allowed()
            || flow.function_header().flags().super_call_allowed()))
        || matches!(
            executable_kind,
            CompilerExecutableKind::OrdinaryArrow
                | CompilerExecutableKind::AsyncArrow
                | CompilerExecutableKind::OrdinaryMethod
                | CompilerExecutableKind::ClassInstanceInitializer
                | CompilerExecutableKind::GeneratorMethod
                | CompilerExecutableKind::AsyncMethod
                | CompilerExecutableKind::AsyncGeneratorMethod
                | CompilerExecutableKind::ClassConstructor
        )
        || static_field_super;
    let mut initial_yield = None;
    let mapped_arguments_authority = flow
        .compiler_capture_layout()
        .and_then(CompilerCaptureLayout::mapped_arguments)
        .is_some();
    let simple_parameter_list = flow.function_header().flags().has_simple_parameter_list();
    for (instruction_index, instruction) in flow.instructions().iter().enumerate() {
        let decoded = instruction.decoded();
        let instruction = decoded.instruction();
        let opcode = instruction.opcode();
        if matches!(
            (opcode, instruction.operands()),
            (FinalOpcode::SpecialObject, Operands::U8(0 | 1))
        ) {
            arguments_object_count = arguments_object_count.saturating_add(1);
            arguments_object_initializer = Some((instruction_index, decoded.pc()));
        } else if opcode == FinalOpcode::Rest {
            rest_parameter_count = rest_parameter_count.saturating_add(1);
        } else if opcode == FinalOpcode::InitialYield
            && initial_yield.replace(decoded.pc()).is_some()
        {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                    pc: decoded.pc(),
                    opcode,
                },
            ));
        }
        let throw_error_authorized = match instruction.operands() {
            Operands::AtomU8 { value: 3, .. } => super_property_authorized,
            Operands::AtomU8 { value: 4, .. } => generator,
            _ => false,
        };
        if !supported_compiler_opcode(opcode)
            || (matches!(
                opcode,
                FinalOpcode::InitialYield
                    | FinalOpcode::Yield
                    | FinalOpcode::YieldStar
                    | FinalOpcode::AsyncYieldStar
            ) && !generator)
            || (opcode == FinalOpcode::Await && !asynchronous)
            || (matches!(
                opcode,
                FinalOpcode::ForAwaitOfStart
                    | FinalOpcode::ForAwaitOfNext
                    | FinalOpcode::IteratorGetValueDone
            ) && !asynchronous)
            || (opcode == FinalOpcode::YieldStar && async_generator)
            || (opcode == FinalOpcode::AsyncYieldStar && !async_generator)
            || (opcode == FinalOpcode::Yield && async_generator && {
                let target = usize_to_u32(instruction_index);
                let immediately_awaited = instruction_index.checked_sub(1).is_some_and(|prior| {
                    let prior = &flow.instructions()[prior];
                    prior.decoded().instruction().opcode() == FinalOpcode::Await
                        && prior.successors().kind() == crate::VerifiedSuccessorKind::Fallthrough
                        && prior.successors().fallthrough().map(InstructionIndex::get)
                            == Some(target)
                });
                let predecessor_count = flow
                    .instructions()
                    .iter()
                    .filter(|candidate| {
                        let successors = candidate.successors();
                        successors.fallthrough().map(InstructionIndex::get) == Some(target)
                            || successors.branch_target().map(InstructionIndex::get) == Some(target)
                            || successors.jump_target().map(InstructionIndex::get) == Some(target)
                    })
                    .count();
                !immediately_awaited || predecessor_count != 1
            })
            || (opcode == FinalOpcode::ReturnAsync && !generator && !asynchronous)
            || (matches!(
                opcode,
                FinalOpcode::IteratorNext
                    | FinalOpcode::IteratorCall
                    | FinalOpcode::IteratorCheckObject
            ) && !generator)
            || (matches!(opcode, FinalOpcode::Return | FinalOpcode::ReturnUndef)
                && (generator || asynchronous))
            || (matches!(
                opcode,
                FinalOpcode::TailCall
                    | FinalOpcode::TailCallMethod
                    | FinalOpcode::TailApply
                    | FinalOpcode::TailEval
                    | FinalOpcode::TailApplyEval
            ) && (!flow.function_header().mode().is_strict()
                || generator
                || asynchronous
                || !matches!(
                    executable_kind,
                    CompilerExecutableKind::OrdinaryFunction
                        | CompilerExecutableKind::OrdinaryArrow
                        | CompilerExecutableKind::OrdinaryMethod
                        | CompilerExecutableKind::ClassConstructor
                )))
            || (opcode == FinalOpcode::TailApply
                && !matches!(instruction.operands(), Operands::U16(0)))
            || (opcode == FinalOpcode::CheckCtorReturn
                && !(matches!(
                    executable_kind,
                    CompilerExecutableKind::OrdinaryArrow | CompilerExecutableKind::AsyncArrow
                ) || (executable_kind == CompilerExecutableKind::DirectEvalScript
                    && flow.function_header().flags().super_call_allowed())
                    || (executable_kind == CompilerExecutableKind::ClassConstructor
                        && flow
                            .function_header()
                            .flags()
                            .is_derived_class_constructor())))
            || (opcode == FinalOpcode::CheckCtorReturn
                && executable_kind == CompilerExecutableKind::DirectEvalScript
                && flow
                    .function_header()
                    .flags()
                    .direct_eval_has_instance_elements()
                && !contextual_instance_initializer_sequence(flow, instruction_index))
            || (matches!(
                opcode,
                FinalOpcode::GetSuper | FinalOpcode::GetSuperValue | FinalOpcode::PutSuperValue
            ) && !super_property_authorized)
            || matches!(
                (opcode, instruction.operands()),
                (FinalOpcode::SpecialObject, operands)
                    if !compiler_special_object_is_authorized(
                        operands,
                        flow,
                        executable_kind,
                        arguments_object_count,
                        rest_parameter_count,
                        mapped_arguments_authority,
                        simple_parameter_list,
                    )
            )
            || (matches!(instruction.operands(), Operands::U8(6))
                && opcode == FinalOpcode::SpecialObject
                && !instruction_index.checked_sub(1).is_some_and(|check_index| {
                    contextual_instance_initializer_sequence(flow, check_index)
                }))
            || matches!(
                (opcode, instruction.operands()),
                (FinalOpcode::Rest, Operands::U16(first_argument))
                    if u32::from(first_argument) != flow.domains().argument_count()
                        || simple_parameter_list
                        || !matches!(
                            executable_kind,
                            CompilerExecutableKind::OrdinaryFunction
                                | CompilerExecutableKind::OrdinaryArrow
                                | CompilerExecutableKind::AsyncArrow
                                | CompilerExecutableKind::OrdinaryMethod
                                | CompilerExecutableKind::ClassConstructor
                                | CompilerExecutableKind::GeneratorFunction
                                | CompilerExecutableKind::GeneratorMethod
                                | CompilerExecutableKind::AsyncFunction
                                | CompilerExecutableKind::AsyncMethod
                                | CompilerExecutableKind::AsyncGeneratorFunction
                                | CompilerExecutableKind::AsyncGeneratorMethod
                        )
                        || rest_parameter_count != 1
            )
            || matches!(
                (opcode, instruction.operands()),
                (FinalOpcode::DefineMethod, Operands::AtomU8 { value, .. })
                    | (FinalOpcode::DefineMethodComputed, Operands::U8(value))
                    if !matches!(value, 0..=2 | 4..=6)
            )
            || (opcode == FinalOpcode::ThrowError && !throw_error_authorized)
        {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                    pc: decoded.pc(),
                    opcode,
                },
            ));
        }
    }
    let arguments_object_definition = metadata
        .variables
        .iter()
        .position(VariableDefinition::is_arguments_object)
        .map(usize_to_u32);
    let initialized_definition = arguments_object_initializer.and_then(|(index, _)| {
        flow.instructions()
            .get(index.checked_add(1)?)
            .and_then(|put| {
                let put = put.decoded().instruction();
                initializer_put_definition(
                    put.opcode(),
                    put.operands(),
                    flow.domains().argument_count() as usize,
                )
                .map(usize_to_u32)
            })
    });
    if arguments_object_definition != initialized_definition {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::ArgumentsObjectMetadataMismatch {
                definition: arguments_object_definition,
                pc: arguments_object_initializer.map(|(_, pc)| pc),
            },
        ));
    }
    if arguments_object_count == 0 && mapped_arguments_authority {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                pc: BytecodePc::ZERO,
                opcode: FinalOpcode::SpecialObject,
            },
        ));
    }
    if generator && initial_yield.is_none() {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                pc: BytecodePc::ZERO,
                opcode: FinalOpcode::InitialYield,
            },
        ));
    }
    Ok(())
}

fn compiler_special_object_is_authorized(
    operands: Operands,
    flow: &VerifiedControlFlow,
    executable_kind: CompilerExecutableKind,
    arguments_object_count: u8,
    rest_parameter_count: u8,
    mapped_arguments_authority: bool,
    simple_parameter_list: bool,
) -> bool {
    if !matches!(
        executable_kind,
        CompilerExecutableKind::DirectEvalScript
            | CompilerExecutableKind::OrdinaryFunction
            | CompilerExecutableKind::OrdinaryArrow
            | CompilerExecutableKind::AsyncArrow
            | CompilerExecutableKind::OrdinaryMethod
            | CompilerExecutableKind::ClassInstanceInitializer
            | CompilerExecutableKind::ClassConstructor
            | CompilerExecutableKind::GeneratorFunction
            | CompilerExecutableKind::GeneratorMethod
            | CompilerExecutableKind::AsyncFunction
            | CompilerExecutableKind::AsyncMethod
            | CompilerExecutableKind::AsyncGeneratorFunction
            | CompilerExecutableKind::AsyncGeneratorMethod
    ) {
        return false;
    }
    match operands {
        Operands::U8(0) => {
            !matches!(
                executable_kind,
                CompilerExecutableKind::OrdinaryArrow | CompilerExecutableKind::AsyncArrow
            ) && (flow.function_header().mode().is_strict() || !simple_parameter_list)
                && arguments_object_count == 1
                && rest_parameter_count == 0
                && !mapped_arguments_authority
        }
        Operands::U8(1) => {
            !matches!(
                executable_kind,
                CompilerExecutableKind::OrdinaryArrow | CompilerExecutableKind::AsyncArrow
            ) && !flow.function_header().mode().is_strict()
                && simple_parameter_list
                && arguments_object_count == 1
                && rest_parameter_count == 0
                && mapped_arguments_authority
        }
        Operands::U8(3) => flow.function_header().flags().new_target_allowed(),
        Operands::U8(4) => {
            matches!(
                executable_kind,
                CompilerExecutableKind::OrdinaryArrow | CompilerExecutableKind::AsyncArrow
            ) || (executable_kind == CompilerExecutableKind::DirectEvalScript
                && flow.function_header().flags().super_call_allowed())
                || (executable_kind == CompilerExecutableKind::ClassConstructor
                    && flow
                        .function_header()
                        .flags()
                        .is_derived_class_constructor())
        }
        Operands::U8(5) => {
            (executable_kind == CompilerExecutableKind::DirectEvalScript
                && flow.function_header().flags().super_allowed())
                || matches!(
                    executable_kind,
                    CompilerExecutableKind::OrdinaryArrow
                        | CompilerExecutableKind::AsyncArrow
                        | CompilerExecutableKind::OrdinaryMethod
                        | CompilerExecutableKind::ClassInstanceInitializer
                        | CompilerExecutableKind::GeneratorMethod
                        | CompilerExecutableKind::AsyncMethod
                        | CompilerExecutableKind::AsyncGeneratorMethod
                        | CompilerExecutableKind::ClassConstructor
                )
        }
        Operands::U8(6) => {
            (executable_kind == CompilerExecutableKind::DirectEvalScript
                && flow
                    .function_header()
                    .flags()
                    .direct_eval_has_instance_elements())
                || matches!(
                    executable_kind,
                    CompilerExecutableKind::OrdinaryArrow | CompilerExecutableKind::AsyncArrow
                )
        }
        _ => false,
    }
}

#[allow(clippy::too_many_lines)]
const fn supported_compiler_opcode(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::PushI32
            | FinalOpcode::PushConst
            | FinalOpcode::FClosure
            | FinalOpcode::SetName
            | FinalOpcode::SetNameComputed
            | FinalOpcode::SetHomeObject
            | FinalOpcode::PushAtomValue
            | FinalOpcode::Undefined
            | FinalOpcode::Null
            | FinalOpcode::PushThis
            | FinalOpcode::PushFalse
            | FinalOpcode::PushTrue
            | FinalOpcode::Object
            | FinalOpcode::RegExp
            | FinalOpcode::SpecialObject
            | FinalOpcode::Rest
            | FinalOpcode::Drop
            | FinalOpcode::Nip
            | FinalOpcode::Dup
            | FinalOpcode::Dup1
            | FinalOpcode::Dup2
            | FinalOpcode::Dup3
            | FinalOpcode::Insert2
            | FinalOpcode::Insert3
            | FinalOpcode::Insert4
            | FinalOpcode::Swap
            | FinalOpcode::Rot3l
            | FinalOpcode::Rot3r
            | FinalOpcode::Rot4l
            | FinalOpcode::CallConstructor
            | FinalOpcode::Call
            | FinalOpcode::CallMethod
            | FinalOpcode::Apply
            | FinalOpcode::TailCall
            | FinalOpcode::TailCallMethod
            | FinalOpcode::TailApply
            | FinalOpcode::TailEval
            | FinalOpcode::TailApplyEval
            | FinalOpcode::Eval
            | FinalOpcode::ApplyEval
            | FinalOpcode::Import
            | FinalOpcode::WithGetVar
            | FinalOpcode::WithDeleteVar
            | FinalOpcode::WithMakeRef
            | FinalOpcode::WithGetRef
            | FinalOpcode::PutRefValue
            | FinalOpcode::ArrayFrom
            | FinalOpcode::CheckCtorReturn
            | FinalOpcode::CheckCtor
            | FinalOpcode::InitCtor
            | FinalOpcode::GetSuper
            | FinalOpcode::GetSuperValue
            | FinalOpcode::PutSuperValue
            | FinalOpcode::Perm3
            | FinalOpcode::Perm4
            | FinalOpcode::Perm5
            | FinalOpcode::Return
            | FinalOpcode::ReturnUndef
            | FinalOpcode::ReturnAsync
            | FinalOpcode::Await
            | FinalOpcode::InitialYield
            | FinalOpcode::Yield
            | FinalOpcode::YieldStar
            | FinalOpcode::AsyncYieldStar
            | FinalOpcode::IteratorNext
            | FinalOpcode::IteratorCall
            | FinalOpcode::IteratorCheckObject
            | FinalOpcode::ThrowError
            | FinalOpcode::Throw
            | FinalOpcode::Catch
            | FinalOpcode::NipCatch
            | FinalOpcode::Gosub
            | FinalOpcode::Ret
            | FinalOpcode::GetVarUndef
            | FinalOpcode::GetVar
            | FinalOpcode::PutVar
            | FinalOpcode::PutVarInit
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
            | FinalOpcode::PutLocCheckInit
            | FinalOpcode::SetLocCheck
            | FinalOpcode::GetVarRefCheck
            | FinalOpcode::PutVarRefCheck
            | FinalOpcode::CloseLoc
            | FinalOpcode::PrivateSymbol
            | FinalOpcode::GetField
            | FinalOpcode::GetField2
            | FinalOpcode::GetPrivateField
            | FinalOpcode::PrivateIn
            | FinalOpcode::GetArrayEl
            | FinalOpcode::GetArrayEl2
            | FinalOpcode::GetArrayEl3
            | FinalOpcode::PutField
            | FinalOpcode::PutPrivateField
            | FinalOpcode::PutArrayEl
            | FinalOpcode::Delete
            | FinalOpcode::SetProto
            | FinalOpcode::ToObject
            | FinalOpcode::ToPropKey
            | FinalOpcode::CopyDataProperties
            | FinalOpcode::DefineField
            | FinalOpcode::DefinePrivateField
            | FinalOpcode::DefineArrayEl
            | FinalOpcode::Append
            | FinalOpcode::DefineClass
            | FinalOpcode::DefineMethod
            | FinalOpcode::DefineMethodComputed
            | FinalOpcode::ForInStart
            | FinalOpcode::ForInNext
            | FinalOpcode::ForOfStart
            | FinalOpcode::ForAwaitOfStart
            | FinalOpcode::ForOfNext
            | FinalOpcode::ForAwaitOfNext
            | FinalOpcode::IteratorGetValueDone
            | FinalOpcode::IteratorClose
            | FinalOpcode::IfFalse
            | FinalOpcode::IfTrue
            | FinalOpcode::Goto
            | FinalOpcode::Neg
            | FinalOpcode::Plus
            | FinalOpcode::Dec
            | FinalOpcode::Inc
            | FinalOpcode::PostDec
            | FinalOpcode::PostInc
            | FinalOpcode::Not
            | FinalOpcode::Lnot
            | FinalOpcode::Typeof
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
            | FinalOpcode::IsNull
            | FinalOpcode::IsUndefinedOrNull
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
            | FinalOpcode::PushConst8
            | FinalOpcode::FClosure8
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
            | FinalOpcode::Call0
            | FinalOpcode::Call1
            | FinalOpcode::Call2
            | FinalOpcode::Call3
            | FinalOpcode::IfFalse8
            | FinalOpcode::IfTrue8
            | FinalOpcode::Goto8
            | FinalOpcode::Goto16
    )
}
