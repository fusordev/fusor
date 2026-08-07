//! Compiler planning and staged bytecode lowering for the pure-Rust `QuickJS`
//! port.
//!
//! [`CompilationContext`] borrows the Oxc model retained by
//! `quickjs-frontend` while keeping Oxc identities private. Storage plans and
//! compiled artifacts are owned and never copy or export Oxc's semantic graph.

#![forbid(unsafe_code)]

mod lowering;
mod storage;

pub(crate) const fn is_supported_dynamic_function_goal(
    goal: quickjs_frontend::CompilationGoal<'_>,
) -> bool {
    matches!(
        goal,
        quickjs_frontend::CompilationGoal::DynamicFunction(
            quickjs_frontend::DynamicFunctionKind::Function
                | quickjs_frontend::DynamicFunctionKind::GeneratorFunction
                | quickjs_frontend::DynamicFunctionKind::AsyncFunction
                | quickjs_frontend::DynamicFunctionKind::AsyncGeneratorFunction
        )
    )
}

pub(crate) const fn is_supported_global_script_goal(
    goal: quickjs_frontend::CompilationGoal<'_>,
) -> bool {
    matches!(
        goal,
        quickjs_frontend::CompilationGoal::GlobalScript(script)
            if !script.allows_top_level_await()
    )
}

pub(crate) const fn is_supported_script_root_goal(
    goal: quickjs_frontend::CompilationGoal<'_>,
) -> bool {
    is_supported_global_script_goal(goal) || is_supported_dynamic_function_goal(goal)
}

pub use lowering::{
    CompilationContext, CompilationExecutable, CompiledClosureSource, CompiledClosureVariable,
    CompiledConstant, CompiledFunction, CompiledFunctionConstant, CompiledFunctionTree,
    CompiledLeafFunction, CompiledRealmGlobal, CompiledRealmGlobalSource, LeafCompilationError,
    LocalSlot, LoweredLocal, RealmGlobalId, SourceInstruction, UnsupportedLeafFeature,
};
pub use storage::{
    BindingId, BindingStorage, CaptureSlot, CaptureSource, CompilationUnitKind, CompilerError,
    DeclarationKind, DeclarationPolicy, Executable, ExecutableId, ExecutableKind, FrameCapture,
    InitializationPolicy, ReferenceAccess, ResolvedReference, ResolvedReferenceId,
    StoragePlacement, StoragePlan, UnresolvedGlobal, UnresolvedGlobalId, UnsupportedFeature,
    WritePolicy, build_storage_plan,
};
