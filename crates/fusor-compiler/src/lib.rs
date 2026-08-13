//! Compiler planning and staged bytecode lowering for the Experimental
//! JavaScript Engine.
//!
//! [`CompilationContext`] borrows the Oxc model retained by
//! `fusor-frontend` while keeping Oxc identities private. Storage plans and
//! compiled artifacts are owned and never copy or export Oxc's semantic graph.

#![forbid(unsafe_code)]

mod lowering;
mod storage;

pub(crate) const fn is_supported_dynamic_function_goal(
    goal: fusor_frontend::CompilationGoal<'_>,
) -> bool {
    matches!(
        goal,
        fusor_frontend::CompilationGoal::DynamicFunction(
            fusor_frontend::DynamicFunctionKind::Function
                | fusor_frontend::DynamicFunctionKind::GeneratorFunction
                | fusor_frontend::DynamicFunctionKind::AsyncFunction
                | fusor_frontend::DynamicFunctionKind::AsyncGeneratorFunction
        )
    )
}

pub(crate) const fn is_supported_global_script_goal(
    goal: fusor_frontend::CompilationGoal<'_>,
) -> bool {
    matches!(
        goal,
        fusor_frontend::CompilationGoal::GlobalScript(script)
            if !script.allows_top_level_await()
    )
}

pub(crate) const fn is_supported_module_goal(goal: fusor_frontend::CompilationGoal<'_>) -> bool {
    matches!(goal, fusor_frontend::CompilationGoal::Module)
}

pub(crate) const fn is_supported_indirect_eval_goal(
    goal: fusor_frontend::CompilationGoal<'_>,
) -> bool {
    matches!(
        goal,
        fusor_frontend::CompilationGoal::IndirectEval(eval) if !eval.forces_strict()
    )
}

pub(crate) const fn is_supported_direct_eval_goal(
    goal: fusor_frontend::CompilationGoal<'_>,
) -> bool {
    matches!(goal, fusor_frontend::CompilationGoal::DirectEval(_))
}

pub(crate) const fn is_supported_script_root_goal(
    goal: fusor_frontend::CompilationGoal<'_>,
) -> bool {
    is_supported_global_script_goal(goal)
        || is_supported_indirect_eval_goal(goal)
        || is_supported_dynamic_function_goal(goal)
}

pub(crate) const fn is_supported_script_compilation_goal(
    goal: fusor_frontend::CompilationGoal<'_>,
) -> bool {
    is_supported_script_root_goal(goal) || is_supported_direct_eval_goal(goal)
}

pub(crate) const fn is_supported_realm_global_binding_goal(
    goal: fusor_frontend::CompilationGoal<'_>,
) -> bool {
    is_supported_script_root_goal(goal)
        || matches!(
            goal,
            fusor_frontend::CompilationGoal::DirectEval(context)
                if matches!(
                    context.variable_environment(),
                    fusor_frontend::DirectEvalVariableEnvironment::Global
                        | fusor_frontend::DirectEvalVariableEnvironment::Function
                        | fusor_frontend::DirectEvalVariableEnvironment::FunctionParameterInitializer
                )
        )
}

pub use lowering::{
    CompilationContext, CompilationExecutable, CompiledClosureSource, CompiledClosureVariable,
    CompiledConstant, CompiledFunction, CompiledFunctionConstant, CompiledFunctionTree,
    CompiledLeafFunction, CompiledRealmGlobal, CompiledRealmGlobalSource, LeafCompilationError,
    LocalSlot, LoweredLocal, RealmGlobalId, SourceInstruction, SourceTextSubstitution,
    UnsupportedLeafFeature,
};
pub use storage::{
    BindingId, BindingStorage, CaptureSlot, CaptureSource, CompilationUnitKind, CompilerError,
    DeclarationKind, DeclarationPolicy, Executable, ExecutableId, ExecutableKind, FrameCapture,
    InitializationPolicy, ReferenceAccess, ResolvedReference, ResolvedReferenceId,
    StoragePlacement, StoragePlan, UnresolvedGlobal, UnresolvedGlobalId, UnsupportedFeature,
    WritePolicy, build_storage_plan,
};
