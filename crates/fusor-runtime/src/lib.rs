//! Safe runtime primitives for the Experimental JavaScript Engine.
//!
//! The crate is under active construction. It exposes a first fail-closed
//! interpreter profile for runtime-installed [`fusor_bytecode::VerifiedBytecode`]
//! together with tested JavaScript value, closure, binding-cell, atom, and
//! ordinary-object invariants. It is not yet the complete object model or
//! language runtime.
//!
//! Recoverable backing-buffer growth failures are returned as structured
//! errors. Allocation of reference-counted headers and immutable backing nodes,
//! including public roots, object-shape owners, and strings, currently follows
//! Rust's global allocator policy; the runtime memory-budget layer remains a
//! separate, unfinished milestone.

#![forbid(unsafe_code)]

mod arena;
mod array_index;
mod atom;
mod bigint;
mod conversion;
mod debug;
mod define_property;
mod diagnostic;
mod error;
mod host;
mod ids;
mod interrupt;
mod number;
mod object;
mod predefined_atoms;
mod promise_rejection;
mod property;
mod runtime;
mod shared_array_buffer;
mod snapshot;
mod string;
mod value;
mod vm;

pub use array_index::{ArrayIndex, MAX_ARRAY_INDEX};
pub use atom::{
    Atom, AtomAllocationTarget, AtomError, AtomKind, AtomLimits, AtomTable, AtomUsage,
    MAX_ATOM_ENTRIES, PREDEFINED_ATOM_COUNT, PREDEFINED_DESCRIPTION_CODE_UNITS,
    PREDEFINED_INTERNER_SLOTS, PropertyKey,
};
pub use bigint::{BigIntError, JsBigInt};
pub use debug::{DebugExecutionSnapshot, DebugLocation, DebuggerHook};
pub use diagnostic::RuntimeDiagnosticError;
pub use error::{
    CallError, DynamicFunctionCompileFailure, DynamicFunctionScriptError, EngineFault,
    ErrorObjectKind, ExceptionKind, ExecutionError, GlobalDeclarationRejectionKind,
    GlobalScriptError, HandleError, HandleKind, InstallError, JsException, JsStackFrame,
    RuntimeError, RuntimeResource, ValueKind,
};
pub use host::{
    DirectEvalCallerBinding, DirectEvalCallerBindingLocation, DirectEvalCallerBindingScope,
    DirectEvalCompileRequest, DirectEvalVariableEnvironment, DynamicFunctionCompileRequest,
    DynamicFunctionCompiler, DynamicFunctionFamily, IndirectEvalCompileRequest,
    OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource,
};
pub use interrupt::{INTERRUPT_POLL_INTERVAL, InterruptHandler};
pub use number::JsNumber;
pub use predefined_atoms::PredefinedAtom;
pub use promise_rejection::{
    OwnedPromiseRejectionEvent, PromiseRejectionEvent, PromiseRejectionOperation,
    PromiseRejectionTracker, PromiseRejectionValue,
};
pub use property::{
    CompletedPropertyDescriptor, DescriptorFields, PropertyDescriptor, PropertyDescriptorError,
    PropertyDescriptorKind, PropertyLayout, PropertyLayoutKind,
};
pub use runtime::{
    CollectionReport, Context, HostFunctionId, ModuleError, ModuleErrorPhase,
    ModuleEvaluationError, ModuleKey, ModuleLinkError, ModuleLoader, ModuleResolveError,
    PendingDynamicImport, Realm, Runtime, RuntimeLimits, RuntimeUsage,
};
pub use shared_array_buffer::SharedArrayBufferHandle;
pub use snapshot::{SNAPSHOT_FORMAT_STAMP, SNAPSHOT_MAGIC, SnapshotError};
pub use string::{CodeUnits, JsString, JsStringError, MAX_STRING_CODE_UNITS};
pub use value::{Function, HostCall, HostCallback, JsValue, Object, Promise, PromiseResolver};
pub use vm::ExecutionLimits;
