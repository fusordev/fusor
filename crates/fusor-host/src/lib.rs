//! Host adapter for Project Fusor.
//!
//! This crate layers the host-side machinery over the engine core
//! (`fusor-runtime`): op binding, the ECMA-262 host event loop, process
//! lifecycle, overlay assembly, and module loading. The engine core never
//! depends on this crate, and `serde`/`Tokio` types never cross the engine
//! boundary.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ops;
