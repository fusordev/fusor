//! The documented process exit-code table (§7.2).
//!
//! | situation | code |
//! | --- | --- |
//! | main script completed, no alive event remained | 0 ([`ExitCode::Clean`]) |
//! | uncaughtException | 1 ([`ExitCode::UncaughtException`]) |
//! | unhandledRejection (Node 15+ alignment) | 1 ([`ExitCode::UnhandledRejection`]) |
//! | force signal | 128 + n ([`ExitCode::Requested`]) |
//! | `process.exit(code)` | code truncated to 8 bits ([`ExitCode::Requested`]) |
//! | resource/limit-class engine abort (instruction limit, allocation, engine fault) | 2 ([`ExitCode::EngineAbort`]) |
//!
//! An interrupt ([`ExecutionError::Interrupted`]) is not a
//! process-terminating error by itself: a REPL consumes it and keeps
//! running, so it has no table entry.

use fusor_runtime::ExecutionError;

/// One row of the documented exit-code table (§7.2).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitCode {
    /// The main script completed and no alive event source remained.
    Clean,
    /// A JavaScript exception escaped the host with no handler (§7.3).
    UncaughtException,
    /// A promise rejection remained unhandled (§7.3, Node 15+ alignment).
    UnhandledRejection,
    /// A requested exit carrying the resolved code: a force signal
    /// (`128 + n`) or `process.exit(code)` truncated to 8 bits.
    Requested(i32),
    /// A resource- or limit-class engine abort (instruction fuel,
    /// allocation, engine fault).
    EngineAbort,
}

impl ExitCode {
    /// The numeric process exit code.
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        match self {
            Self::Clean => 0,
            Self::UncaughtException | Self::UnhandledRejection => 1,
            Self::Requested(code) => code,
            Self::EngineAbort => 2,
        }
    }

    /// Maps an [`ExecutionError`] onto the table (§7.2): a JavaScript
    /// exception that escaped a host invocation is an uncaught exception;
    /// every other failure class is an engine abort. An interrupt is not
    /// a process-terminating error by itself and maps to `None`.
    #[must_use]
    pub fn from_execution_error(error: &ExecutionError) -> Option<Self> {
        match error {
            ExecutionError::Exception(_) => Some(Self::UncaughtException),
            ExecutionError::Interrupted { .. } => None,
            _ => Some(Self::EngineAbort),
        }
    }
}
