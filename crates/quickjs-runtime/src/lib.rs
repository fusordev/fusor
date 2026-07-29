//! Safe runtime primitives for the pure-Rust `QuickJS` port.
//!
//! The crate is under active construction. Each exposed primitive preserves a
//! tested JavaScript semantic invariant even while the VM and object heap are
//! still being built.
//!
//! Recoverable backing-buffer growth failures are returned as structured
//! errors. Allocation of immutable reference-counted string nodes currently
//! follows Rust's global allocator policy; the runtime memory-budget layer
//! remains a separate, unfinished milestone.

#![forbid(unsafe_code)]

mod array_index;
mod atom;
mod number;
mod predefined_atoms;
mod string;

pub use array_index::{ArrayIndex, MAX_ARRAY_INDEX};
pub use atom::{
    Atom, AtomAllocationTarget, AtomError, AtomKind, AtomLimits, AtomTable, AtomUsage,
    MAX_ATOM_ENTRIES, PREDEFINED_ATOM_COUNT, PREDEFINED_DESCRIPTION_CODE_UNITS,
    PREDEFINED_INTERNER_SLOTS, PropertyKey,
};
pub use number::JsNumber;
pub use predefined_atoms::PredefinedAtom;
pub use string::{CodeUnits, JsString, JsStringError, MAX_STRING_CODE_UNITS};
