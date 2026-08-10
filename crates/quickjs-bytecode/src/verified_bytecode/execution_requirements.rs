#[allow(clippy::too_many_lines)]
fn collect_requirements(
    function: &VerifiedCompilerFunction,
    metadata: &VerifiedFunctionMetadata,
    requirements: &mut Vec<ExecutionRequirement>,
) {
    if !function.atoms().is_empty()
        || function.constants().iter().any(|constant| {
            matches!(
                constant,
                crate::CompilerConstant::Value(
                    crate::CompilerConstantValue::String(_)
                        | crate::CompilerConstantValue::TemplateObject(_)
                )
            )
        })
    {
        push_requirement(requirements, ExecutionRequirement::Strings);
    }
    if function.constants().iter().any(|constant| {
        matches!(
            constant,
            crate::CompilerConstant::Value(crate::CompilerConstantValue::Number(_))
        )
    }) {
        push_requirement(requirements, ExecutionRequirement::Numbers);
    }
    if function.constants().iter().any(|constant| {
        matches!(
            constant,
            crate::CompilerConstant::Value(crate::CompilerConstantValue::BigInt(_))
        )
    }) {
        push_requirement(requirements, ExecutionRequirement::BigInts);
    }
    if function.constants().iter().any(|constant| {
        matches!(
            constant,
            crate::CompilerConstant::Value(crate::CompilerConstantValue::TemplateObject(_))
        )
    }) {
        push_requirement(requirements, ExecutionRequirement::Arrays);
        push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
    }
    if !metadata.closures.is_empty()
        || function
            .control_flow()
            .function_header()
            .variable_reference_count()
            != 0
        || function
            .constants()
            .iter()
            .any(|constant| matches!(constant, crate::CompilerConstant::Function(_)))
    {
        push_requirement(requirements, ExecutionRequirement::Closures);
    }
    if metadata
        .variables
        .iter()
        .any(|definition| definition.has_scope || definition.policy.temporal_dead_zone)
        || metadata
            .closures
            .iter()
            .any(|definition| definition.policy().temporal_dead_zone)
    {
        push_requirement(requirements, ExecutionRequirement::LexicalBindings);
    }
    if metadata
        .closures
        .iter()
        .any(|definition| definition.binding().is_realm_global())
    {
        push_requirement(requirements, ExecutionRequirement::RealmGlobalBindings);
    }
    for instruction in function.control_flow().instructions() {
        let instruction = instruction.decoded().instruction();
        match instruction.opcode() {
            FinalOpcode::CallConstructor
            | FinalOpcode::Call
            | FinalOpcode::Call0
            | FinalOpcode::Call1
            | FinalOpcode::Call2
            | FinalOpcode::Call3
            | FinalOpcode::CallMethod
            | FinalOpcode::Apply
            | FinalOpcode::TailCall
            | FinalOpcode::TailCallMethod
            | FinalOpcode::TailApply
            | FinalOpcode::TailEval
            | FinalOpcode::TailApplyEval
            | FinalOpcode::Eval
            | FinalOpcode::ApplyEval
            | FinalOpcode::InitCtor
            | FinalOpcode::GetSuper
            | FinalOpcode::GetSuperValue
            | FinalOpcode::PutSuperValue
            | FinalOpcode::PushThis => {
                push_requirement(requirements, ExecutionRequirement::Calls);
            }
            FinalOpcode::ArrayFrom | FinalOpcode::Rest => {
                push_requirement(requirements, ExecutionRequirement::Arrays);
            }
            FinalOpcode::Append => {
                push_requirement(requirements, ExecutionRequirement::Arrays);
                push_requirement(requirements, ExecutionRequirement::Iterators);
            }
            FinalOpcode::ForOfStart
            | FinalOpcode::ForAwaitOfStart
            | FinalOpcode::ForOfNext
            | FinalOpcode::IteratorClose => {
                push_requirement(requirements, ExecutionRequirement::Iterators);
            }
            FinalOpcode::Object
            | FinalOpcode::SetName
            | FinalOpcode::GetField
            | FinalOpcode::GetField2
            | FinalOpcode::PutField
            | FinalOpcode::DefineField
            | FinalOpcode::DefineClass
            | FinalOpcode::DefineMethod
            | FinalOpcode::ForInStart => {
                push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
            }
            FinalOpcode::WithGetVar
            | FinalOpcode::WithDeleteVar
            | FinalOpcode::WithMakeRef
            | FinalOpcode::WithGetRef
            | FinalOpcode::PutRefValue => {
                push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
                push_requirement(requirements, ExecutionRequirement::Calls);
            }
            FinalOpcode::SpecialObject => match instruction.operands() {
                Operands::U8(3..=6) => {
                    push_requirement(requirements, ExecutionRequirement::Calls);
                }
                Operands::U8(0 | 1) => {
                    push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
                }
                _ => unreachable!("verified compiler special-object selector"),
            },
            FinalOpcode::ForInNext => {
                push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
                push_requirement(requirements, ExecutionRequirement::Strings);
            }
            FinalOpcode::GetArrayEl
            | FinalOpcode::GetArrayEl2
            | FinalOpcode::GetArrayEl3
            | FinalOpcode::PutArrayEl
            | FinalOpcode::ToPropKey
            | FinalOpcode::DefineArrayEl
            | FinalOpcode::DefineMethodComputed
            | FinalOpcode::SetNameComputed => {
                push_requirement(requirements, ExecutionRequirement::OrdinaryObjects);
                push_requirement(requirements, ExecutionRequirement::DynamicPropertyKeys);
            }
            FinalOpcode::Throw
            | FinalOpcode::Catch
            | FinalOpcode::NipCatch
            | FinalOpcode::Gosub
            | FinalOpcode::Ret => {
                push_requirement(requirements, ExecutionRequirement::AbruptCompletions);
            }
            FinalOpcode::PushBigIntI32 => {
                push_requirement(requirements, ExecutionRequirement::BigInts);
            }
            FinalOpcode::InstanceOf | FinalOpcode::In => {
                push_requirement(requirements, ExecutionRequirement::ObjectOperators);
            }
            FinalOpcode::PushI32
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
            | FinalOpcode::PushI16 => {
                push_requirement(requirements, ExecutionRequirement::Numbers);
            }
            FinalOpcode::Neg
            | FinalOpcode::Plus
            | FinalOpcode::Dec
            | FinalOpcode::Inc
            | FinalOpcode::PostDec
            | FinalOpcode::PostInc
            | FinalOpcode::Not
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
            | FinalOpcode::Eq
            | FinalOpcode::Neq
            | FinalOpcode::StrictEq
            | FinalOpcode::StrictNeq
            | FinalOpcode::And
            | FinalOpcode::Xor
            | FinalOpcode::Or => {
                push_requirement(requirements, ExecutionRequirement::DynamicOperators);
            }
            FinalOpcode::PushEmptyString | FinalOpcode::Typeof => {
                push_requirement(requirements, ExecutionRequirement::Strings);
            }
            FinalOpcode::CloseLoc => {
                push_requirement(requirements, ExecutionRequirement::LexicalBindings);
                push_requirement(requirements, ExecutionRequirement::Closures);
            }
            _ => {}
        }
    }
}

fn push_requirement(
    requirements: &mut Vec<ExecutionRequirement>,
    requirement: ExecutionRequirement,
) {
    if !requirements.contains(&requirement) {
        requirements.push(requirement);
    }
}

fn function_id(index: usize) -> Result<FunctionTemplateId, BytecodeVerificationError> {
    let index = u32::try_from(index).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::LimitExceeded {
            resource: BytecodeGraphResource::VerifiedMetadata,
            limit: u64::from(u32::MAX),
            observed: usize_to_u64(index),
        })
    })?;
    Ok(FunctionTemplateId::new(index))
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn usize_to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}
