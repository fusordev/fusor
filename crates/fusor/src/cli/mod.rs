//! The `fusor` binary's CLI modules (node-like module runner, ESM REPL, and
//! DevTools entry point).
//!
//! These modules belong to the *binary* target of the `fusor` package
//! (`src/main.rs`), not to the facade library (`src/lib.rs`): the module
//! loader, its `node:` builtin table, and the REPL are CLI concerns
//! (2026-08-14 decision — no module loading lives in `fusor-host`).

pub(crate) mod builtins;
pub(crate) mod imports;
pub(crate) mod loader;
pub(crate) mod repl;
pub(crate) mod resolver;
