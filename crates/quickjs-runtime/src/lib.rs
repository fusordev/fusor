//! Safe runtime primitives for the pure-Rust `QuickJS` port.
//!
//! The crate is under active construction. It exposes a first fail-closed
//! interpreter profile for runtime-installed [`quickjs_bytecode::VerifiedBytecode`]
//! together with tested JavaScript value, closure, binding-cell, and atom
//! invariants. It is not yet the complete object heap or language runtime.
//!
//! Recoverable backing-buffer growth failures are returned as structured
//! errors. Allocation of immutable reference-counted string nodes currently
//! follows Rust's global allocator policy; the runtime memory-budget layer
//! remains a separate, unfinished milestone.

#![forbid(unsafe_code)]

mod arena;
mod array_index;
mod atom;
mod error;
mod ids;
mod number;
mod predefined_atoms;
mod property;
mod runtime;
mod string;
mod value;
mod vm;

pub use array_index::{ArrayIndex, MAX_ARRAY_INDEX};
pub use atom::{
    Atom, AtomAllocationTarget, AtomError, AtomKind, AtomLimits, AtomTable, AtomUsage,
    MAX_ATOM_ENTRIES, PREDEFINED_ATOM_COUNT, PREDEFINED_DESCRIPTION_CODE_UNITS,
    PREDEFINED_INTERNER_SLOTS, PropertyKey,
};
pub use error::{
    EngineFault, ExceptionKind, ExecutionError, HandleError, HandleKind, InstallError, JsException,
    JsStackFrame, RuntimeError, RuntimeResource, ValueKind,
};
pub use number::JsNumber;
pub use predefined_atoms::PredefinedAtom;
pub use property::{
    CompletedPropertyDescriptor, DescriptorFields, PropertyDescriptor, PropertyDescriptorError,
    PropertyDescriptorKind, PropertyLayout, PropertyLayoutKind,
};
pub use runtime::{CollectionReport, Context, Realm, Runtime, RuntimeLimits, RuntimeUsage};
pub use string::{CodeUnits, JsString, JsStringError, MAX_STRING_CODE_UNITS};
pub use value::{Function, JsValue};
pub use vm::ExecutionLimits;
