//! Chrome DevTools Protocol server for the Experimental JavaScript Engine.
//!
//! A bin-side module of the `fusor` package (2026-08-14: the former
//! `fusor-cdp` crate merged into the CLI — the CDP server's only consumer
//! is the CLI, and the facade library must stay free of the CLI's
//! dependencies). The engine is runtime-local and synchronous, so network
//! I/O lives on OS threads and protocol requests that need live engine
//! values are forwarded to the runtime-owning task through
//! [`cdp::EngineRequest`].

mod cdp;
pub(crate) mod format;
pub(crate) mod inspector;

pub(crate) use cdp::{DebugSession, EngineRequest, start};
