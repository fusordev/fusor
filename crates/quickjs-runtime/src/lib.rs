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

mod number;
mod string;

pub use number::JsNumber;
pub use string::{CodeUnits, JsString, JsStringError, MAX_STRING_CODE_UNITS};
