//! Arena-independent compiler metadata for the pure-Rust `QuickJS` port.
//!
//! This crate consumes the Oxc semantic model retained by
//! `quickjs-frontend`, but never copies or exports Oxc's semantic graph.

#![forbid(unsafe_code)]

mod storage;

pub use storage::{
    BindingId, BindingStorage, CompilationUnitKind, CompilerError, DeclarationKind,
    DeclarationPolicy, Executable, ExecutableId, ExecutableKind, InitializationPolicy,
    ReferenceAccess, ResolvedReference, ResolvedReferenceId, StoragePlacement, StoragePlan,
    UnresolvedGlobal, UnresolvedGlobalId, UnsupportedFeature, WritePolicy, build_storage_plan,
};
