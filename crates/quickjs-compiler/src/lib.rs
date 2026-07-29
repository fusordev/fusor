//! Compiler planning and staged bytecode lowering for the pure-Rust `QuickJS`
//! port.
//!
//! [`CompilationContext`] borrows the Oxc model retained by
//! `quickjs-frontend` while keeping Oxc identities private. Storage plans and
//! compiled artifacts are owned and never copy or export Oxc's semantic graph.

#![forbid(unsafe_code)]

mod lowering;
mod storage;

pub use lowering::{
    CompilationContext, CompilationExecutable, CompiledLeafFunction, LeafCompilationError,
    LocalSlot, LoweredLocal, SourceInstruction, UnsupportedLeafFeature,
};
pub use storage::{
    BindingId, BindingStorage, CaptureSlot, CaptureSource, CompilationUnitKind, CompilerError,
    DeclarationKind, DeclarationPolicy, Executable, ExecutableId, ExecutableKind, FrameCapture,
    InitializationPolicy, ReferenceAccess, ResolvedReference, ResolvedReferenceId,
    StoragePlacement, StoragePlan, UnresolvedGlobal, UnresolvedGlobalId, UnsupportedFeature,
    WritePolicy, build_storage_plan,
};
