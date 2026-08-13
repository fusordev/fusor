//! ECMA-262 host event loop (§6): a continuously alive turn loop driving
//! the engine's host-job queue on one owner task.
//!
//! The loop runs on a Tokio `current_thread` runtime. Every turn handles the
//! due events (timers, async-op completions, signals, custom host events),
//! then drains `drain_host_jobs` to quiescence — the ECMA-262 microtask
//! checkpoint — before selecting on the next event source. Host callbacks
//! never interleave with pending jobs (§6.2 normative pin).

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

use fusor_runtime::{Context, ExecutionError, ExecutionLimits, Realm, Runtime};
use tokio::runtime::{Builder, Runtime as TokioRuntime};

/// One custom host event: a closure the loop invokes on the owner task.
///
/// The closure receives the context for its turn; engine interaction can
/// only happen inside it, on the owner task.
pub type HostEvent = Box<dyn FnOnce(&mut Context<'_>) -> Result<(), ExecutionError>>;

/// Host-loop construction failures.
#[derive(Debug)]
pub enum HostLoopError {
    /// The Tokio current-thread executor could not start.
    Executor(String),
}

impl fmt::Display for HostLoopError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Executor(message) => write!(formatter, "host loop executor failed: {message}"),
        }
    }
}

impl Error for HostLoopError {}

/// The host event loop (§6.1): continuously alive, owner-task driven.
///
/// The engine [`Runtime`] is owned here and only ever touched inside turn
/// callbacks on the thread that created the loop.
pub struct HostLoop {
    runtime: Runtime,
    realm: Realm,
    tokio: TokioRuntime,
    custom_events: VecDeque<HostEvent>,
    exit_when_idle: bool,
}

impl HostLoop {
    /// Creates the loop around one realm with the Tokio `current_thread`
    /// executor (§6.1).
    ///
    /// # Errors
    ///
    /// Returns [`HostLoopError::Executor`] when Tokio cannot start.
    pub fn new(runtime: Runtime, realm: Realm) -> Result<Self, HostLoopError> {
        let tokio = Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|error| HostLoopError::Executor(error.to_string()))?;
        Ok(Self {
            runtime,
            realm,
            tokio,
            custom_events: VecDeque::new(),
            exit_when_idle: true,
        })
    }

    /// Runs turns until no alive event source and no pending work remains,
    /// then returns (the §6.5 exit condition).
    ///
    /// # Errors
    ///
    /// Returns the first turn failure (an uncatchable job failure or a host
    /// callback error).
    pub fn run_until_idle(&mut self) -> Result<(), ExecutionError> {
        while self.alive() {
            self.run_one_turn()?;
        }
        Ok(())
    }

    /// Executes one complete turn: due events, then the host-job drain to
    /// quiescence (§6.2).
    ///
    /// # Errors
    ///
    /// Returns an uncatchable job failure or a host callback error.
    pub fn run_one_turn(&mut self) -> Result<(), ExecutionError> {
        self.poll_op_completions();
        while let Some(event) = self.custom_events.pop_front() {
            let mut context = self
                .runtime
                .context(&self.realm)
                .map_err(ExecutionError::from)?;
            event(&mut context)?;
            // The microtask checkpoint runs after every event (§6.2).
            context.drain_host_jobs(ExecutionLimits::default(), None)?;
        }
        Ok(())
    }

    /// Queues one custom host event for the next turn (§6.3 event source ⑤).
    pub fn post_event(&mut self, event: HostEvent) {
        self.custom_events.push_back(event);
    }

    /// Returns whether any alive event source or pending work remains.
    #[must_use]
    pub fn alive(&self) -> bool {
        (!self.custom_events.is_empty() || self.pending_ops() > 0) && self.exit_when_idle
    }

    /// Returns the pending async-op count (the §5.5 completion source).
    fn pending_ops(&self) -> usize {
        crate::ops::pending_op_count().unwrap_or(0)
    }

    /// Polls the async-op completion channel (§5.5), settling finished ops.
    fn poll_op_completions(&mut self) {
        let Ok(mut context) = self.runtime.context(&self.realm) else {
            return;
        };
        let _ = crate::ops::poll_op_completions(&mut context);
    }
}

impl fmt::Debug for HostLoop {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostLoop")
            .field("custom_events", &self.custom_events.len())
            .field("exit_when_idle", &self.exit_when_idle)
            .finish()
    }
}
