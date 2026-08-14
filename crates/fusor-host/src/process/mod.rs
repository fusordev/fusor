//! Process lifecycle and diagnostics (subproject 4, §7): signal handling,
//! exit codes, uncaught exceptions, the shutdown sequence, and unified
//! diagnostic rendering.

mod signals;

pub use signals::{Signal, SignalForwardError, SignalForwarder, SignalState, spawn_signal_forwarder};
