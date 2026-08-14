//! Host adapter for Project Fusor.
//!
//! This crate layers the host-side machinery over the engine core
//! (`fusor-runtime`): op binding, the ECMA-262 host event loop, process
//! lifecycle, overlay assembly, and module loading. The engine core never
//! depends on this crate, and `serde`/`Tokio` types never cross the engine
//! boundary.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

// The `#[op]` macro expansions reference this crate through absolute
// `::fusor_host::` paths (they must also work from integration tests);
// this binding makes those paths resolve inside the crate itself.
extern crate self as fusor_host;

pub mod r#loop;
pub mod ops;
pub mod process;
