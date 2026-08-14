//! The documented error-code system (§7.5, §12.1): plain five-digit
//! numbers organized by classification range.
//!
//! | range | layer (§12.1) | codes |
//! | --- | --- | --- |
//! | 10000–10099 | handle misuse | 10001 orphaned, 10002 foreign runtime, 10003 stale, 10004 wrong value kind |
//! | 11000–11099 | engine execution | 11001 uncaught exception, 11002 interrupted, 11003 instruction limit, 11004 resource limit, 11005 engine fault, 11006 other engine failure |
//! | 12000–12099 | host calls | 12001 thrown, 12002 execution |
//! | 13000–13099 | modules | reserved for the module adapters (subproject 6/7) |
//! | 14000–14099 | op layer | 14001 op failure; an op's own numeric code passes through verbatim |
//! | 15000–15099 | snapshot | reserved for `SnapshotError` (subproject 5) |
//! | 16000–16099 | parse/compile | reserved for the frontend adapters |
//!
//! Codes are stable within the alpha line (documented; no version
//! compatibility promise beyond that, §8.1).

use std::fmt;

use fusor_runtime::{CallError, ExecutionError};

/// One documented error code (§12.1 classification).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCode {
    /// `HandleError::Orphaned`: a public handle outlived its runtime.
    HandleOrphaned,
    /// `HandleError::ForeignRuntime`: a handle crossed runtimes.
    HandleForeign,
    /// `HandleError::Stale`: a handle named a collected arena slot.
    HandleStale,
    /// `HandleError::WrongValueKind`: a handle had the wrong value kind.
    HandleWrongKind,
    /// An uncaught JavaScript exception (`ExecutionError::Exception`).
    UncaughtException,
    /// The host interrupt handler cancelled execution.
    Interrupted,
    /// Per-call instruction fuel was exhausted.
    InstructionLimit,
    /// A runtime execution resource limit was exceeded.
    LimitExceeded,
    /// The engine's internal invariants were violated.
    EngineFault,
    /// Another engine-class failure (strings, atoms, dynamic functions).
    EngineOther,
    /// A host-invoked function threw (`CallError::Thrown`).
    CallThrown,
    /// A host-invoked function failed with an execution error.
    CallExecution,
    /// An op failed with its own error (`OpError`); an op's own numeric
    /// code passes through verbatim instead.
    OpFailure,
}

impl ErrorCode {
    /// The five-digit numeric code (§12.1 table).
    #[must_use]
    pub const fn number(self) -> u16 {
        match self {
            Self::HandleOrphaned => 10001,
            Self::HandleForeign => 10002,
            Self::HandleStale => 10003,
            Self::HandleWrongKind => 10004,
            Self::UncaughtException => 11001,
            Self::Interrupted => 11002,
            Self::InstructionLimit => 11003,
            Self::LimitExceeded => 11004,
            Self::EngineFault => 11005,
            Self::EngineOther => 11006,
            Self::CallThrown => 12001,
            Self::CallExecution => 12002,
            Self::OpFailure => 14001,
        }
    }

    /// Maps one execution error onto its §12.1 class code.
    #[must_use]
    pub fn from_execution_error(error: &ExecutionError) -> Self {
        match error {
            ExecutionError::Handle(handle) => match handle {
                fusor_runtime::HandleError::Orphaned { .. } => Self::HandleOrphaned,
                fusor_runtime::HandleError::ForeignRuntime { .. } => Self::HandleForeign,
                fusor_runtime::HandleError::Stale { .. } => Self::HandleStale,
                fusor_runtime::HandleError::WrongValueKind { .. } => Self::HandleWrongKind,
            },
            ExecutionError::Exception(_) => Self::UncaughtException,
            ExecutionError::Interrupted { .. } => Self::Interrupted,
            ExecutionError::InstructionLimitExceeded { .. } => Self::InstructionLimit,
            ExecutionError::LimitExceeded { .. } => Self::LimitExceeded,
            ExecutionError::EngineFault(_) => Self::EngineFault,
            _ => Self::EngineOther,
        }
    }

    /// Maps one host call failure onto its §12.1 class code.
    #[must_use]
    pub fn from_call_error(error: &CallError) -> Self {
        match error {
            CallError::Thrown(_) => Self::CallThrown,
            CallError::Execution(source) => Self::from_execution_error(source),
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.number())
    }
}
