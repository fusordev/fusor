//! Chrome DevTools Protocol server for the Experimental JavaScript Engine.
//!
//! The engine is runtime-local and synchronous, so network I/O lives on OS
//! threads and protocol requests that need live engine values are forwarded
//! to the runtime-owning task through [`cdp::EngineRequest`].

pub mod cdp;
pub mod format;
pub mod inspector;

pub use cdp::{DebugSession, EngineRequest, start};
