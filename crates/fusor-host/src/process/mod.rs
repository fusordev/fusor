//! Process lifecycle and diagnostics (subproject 4, §7): signal handling,
//! exit codes, uncaught exceptions, the shutdown sequence, and unified
//! diagnostic rendering.

mod exit_codes;
mod signals;
mod state;

pub use exit_codes::ExitCode;
pub use signals::{Signal, SignalForwardError, SignalForwarder, SignalState, spawn_signal_forwarder};
pub(crate) use signals::{install_signal_state, with_signal_state};
pub(crate) use state::{
    ProcessState, has_pending_rejections, install_process_state, push_rejection_event,
    take_rejection_events, with_process_state,
};
