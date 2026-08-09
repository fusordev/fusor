//! Whole-graph certification for lexical function-environment capabilities.

use super::{
    BytecodeGraphResource, BytecodeVerificationError, BytecodeVerificationErrorKind,
    ClosureVariableDefinition, CompilerBindingKind, CompilerExecutableKind, FinalOpcode,
    FunctionTemplateId, Operands, VariableDefinition, VerifiedCompilerFunctionGraph,
    VerifiedControlFlow, VerifiedFunctionMetadata, VerifiedInstruction, function_id,
    try_filled_vec,
};

fn lexical_arrow_boundary(
    id: FunctionTemplateId,
    metadata: &[VerifiedFunctionMetadata],
    parents: &[Option<FunctionTemplateId>],
) -> Option<(FunctionTemplateId, CompilerExecutableKind)> {
    let mut ancestor = id;
    loop {
        let parent = usize::try_from(ancestor.get())
            .ok()
            .and_then(|ancestor_index| parents.get(ancestor_index))
            .copied()
            .flatten()?;
        let parent_metadata = usize::try_from(parent.get())
            .ok()
            .and_then(|parent_index| metadata.get(parent_index))?;
        match parent_metadata.executable_kind {
            CompilerExecutableKind::OrdinaryArrow => ancestor = parent,
            kind => return Some((parent, kind)),
        }
    }
}

fn lexical_arrow_uses(
    flow: &VerifiedControlFlow,
    static_field_super: bool,
) -> (Option<VerifiedInstruction>, Option<VerifiedInstruction>) {
    let home_object = flow.instructions().iter().copied().find(|verified| {
        let instruction = verified.decoded().instruction();
        matches!(
            (instruction.opcode(), instruction.operands()),
            (FinalOpcode::SpecialObject, Operands::U8(5))
        ) || (!static_field_super
            && matches!(
                instruction.opcode(),
                FinalOpcode::GetSuper | FinalOpcode::GetSuperValue | FinalOpcode::PutSuperValue
            ))
    });
    let derived_super = flow.instructions().iter().copied().find(|verified| {
        matches!(
            (
                verified.decoded().instruction().opcode(),
                verified.decoded().instruction().operands(),
            ),
            (FinalOpcode::SpecialObject, Operands::U8(4))
        )
    });
    (home_object, derived_super)
}

/// Certifies the function-environment capabilities inherited by arrows.
/// Home objects cross only arrow boundaries beneath methods or constructors;
/// the mutable derived-`this` binding crosses only arrow boundaries beneath a
/// derived constructor. Static field arrows instead carry the separately
/// certified class-receiver cell.
pub(super) fn verify_lexical_arrow_environments(
    graph: &VerifiedCompilerFunctionGraph,
    metadata: &[VerifiedFunctionMetadata],
    parents: &[Option<FunctionTemplateId>],
) -> Result<Vec<bool>, BytecodeVerificationError> {
    let mut lexical_derived_this = try_filled_vec(
        graph.root_id(),
        metadata.len(),
        false,
        BytecodeGraphResource::VerifiedMetadata,
    )?;
    for (index, record) in metadata.iter().enumerate() {
        if record.executable_kind != CompilerExecutableKind::OrdinaryArrow {
            continue;
        }
        let id = function_id(index)?;
        let flow = graph
            .function(id)
            .ok_or_else(|| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::FunctionTemplateOwnershipMismatch {
                        child: id,
                        incoming: 0,
                    },
                )
            })?
            .control_flow();
        let static_field_super = record
            .variables
            .iter()
            .map(VariableDefinition::policy)
            .chain(
                record
                    .closures
                    .iter()
                    .map(ClosureVariableDefinition::policy),
            )
            .any(|policy| policy.kind() == CompilerBindingKind::ClassStaticReceiver);
        let (home_object_use, derived_super_call) = lexical_arrow_uses(flow, static_field_super);
        let boundary = lexical_arrow_boundary(id, metadata, parents);

        let derived_constructor = boundary.is_some_and(|(boundary, kind)| {
            graph.function(boundary).is_some_and(|function| {
                let flags = function.control_flow().function_header().flags();
                (kind == CompilerExecutableKind::ClassConstructor
                    && flags.is_derived_class_constructor())
                    || (kind == CompilerExecutableKind::DirectEvalScript
                        && flags.super_call_allowed())
            })
        });
        if let Some(offending) = derived_super_call
            && !derived_constructor
        {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                    pc: offending.decoded().pc(),
                    opcode: offending.decoded().instruction().opcode(),
                },
            ));
        }

        let home_object_authorized = boundary.is_some_and(|(boundary, kind)| {
            matches!(
                kind,
                CompilerExecutableKind::OrdinaryMethod
                    | CompilerExecutableKind::ClassInstanceInitializer
                    | CompilerExecutableKind::GeneratorMethod
                    | CompilerExecutableKind::AsyncMethod
                    | CompilerExecutableKind::AsyncGeneratorMethod
                    | CompilerExecutableKind::ClassConstructor
            ) || (kind == CompilerExecutableKind::DirectEvalScript
                && graph.function(boundary).is_some_and(|function| {
                    function
                        .control_flow()
                        .function_header()
                        .flags()
                        .super_allowed()
                }))
        });
        if let Some(offending) = home_object_use
            && !home_object_authorized
        {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                    pc: offending.decoded().pc(),
                    opcode: offending.decoded().instruction().opcode(),
                },
            ));
        }
        lexical_derived_this[index] = derived_constructor;
    }
    Ok(lexical_derived_this)
}
