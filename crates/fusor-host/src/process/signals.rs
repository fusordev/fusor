//! Signal lifecycle (§7.1): the injectable signal event source, the
//! interrupt request shared with the engine's [`InterruptHandler`], and
//! the documented delivery policy.
//!
//! Policy (§7.1, documented semantics): the **first** SIGINT requests an
//! interrupt — the engine cancels the running script at its next
//! instruction-boundary poll with an uncatchable
//! [`ExecutionError::Interrupted`]. A SIGINT delivered while a request is
//! still outstanding is the **second** SIGINT and force-exits with
//! `128 + 2`; SIGTERM always force-exits with `128 + 15`. A request no
//! script consumes within its turn is cleared at the turn's end (an idle
//! Ctrl+C interrupts nothing, exactly like an idle REPL prompt). Force
//! exits take effect at the next turn boundary and never reset.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// The host-interpreted signal subset (§7.1).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Signal {
    /// SIGINT (2): the first delivery requests an interrupt; a second
    /// delivery while the request is outstanding force-exits with `128 + 2`.
    Interrupt,
    /// SIGTERM (15): always force-exits with `128 + 15`.
    Terminate,
}

impl Signal {
    /// The process exit code `128 + n` this signal produces on force exit
    /// (§7.2).
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Interrupt => 130,
            Self::Terminate => 143,
        }
    }
}

/// The shared signal state: the interrupt request flag consulted by the
/// engine's [`InterruptHandler`] and the pending force-exit code.
///
/// All mutations are atomic, so OS signal-delivery threads and the owner
/// task share one state without further synchronization (§7.6: signal
/// delivery is injectable and thread-safe by construction). This type
/// never panics and is cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct SignalState {
    interrupt: Arc<AtomicBool>,
    force_exit: Arc<AtomicI32>,
}

impl SignalState {
    /// Delivers one signal, applying the §7.1 policy.
    ///
    /// Returns `true` when this delivery forces an exit (a second SIGINT
    /// while a request was outstanding, or any SIGTERM).
    pub fn deliver(&self, signal: Signal) -> bool {
        match signal {
            Signal::Interrupt => {
                // `swap(true)` reports whether a request was already
                // outstanding: that makes this delivery the second SIGINT.
                if self.interrupt.swap(true, Ordering::SeqCst) {
                    self.force_exit.store(signal.exit_code(), Ordering::SeqCst);
                    true
                } else {
                    false
                }
            }
            Signal::Terminate => {
                self.force_exit.store(signal.exit_code(), Ordering::SeqCst);
                true
            }
        }
    }

    /// Returns whether an interrupt is requested (the engine's
    /// [`InterruptHandler`] consults this per poll).
    #[must_use]
    pub fn interrupt_requested(&self) -> bool {
        self.interrupt.load(Ordering::SeqCst)
    }

    /// Clears the interrupt request once it has been consumed: the running
    /// script aborted with [`ExecutionError::Interrupted`], or the turn
    /// ended with nothing to interrupt.
    pub fn consume_interrupt(&self) {
        self.interrupt.store(false, Ordering::SeqCst);
    }

    /// Returns the pending force-exit code (§7.2), if one was requested.
    ///
    /// The code never resets: a force exit terminates the process.
    #[must_use]
    pub fn pending_exit_code(&self) -> Option<i32> {
        match self.force_exit.load(Ordering::SeqCst) {
            0 => None,
            code => Some(code),
        }
    }
}

/// Failures while attaching the OS signal forwarder.
#[derive(Debug)]
pub enum SignalForwardError {
    /// The Tokio current-thread executor for the forwarder could not start.
    Executor(String),
    /// A signal stream could not be registered.
    Stream(String),
    /// The forwarder thread could not be spawned.
    Spawn(String),
}

impl std::fmt::Display for SignalForwardError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Executor(message) => write!(formatter, "signal forwarder executor failed: {message}"),
            Self::Stream(message) => write!(formatter, "signal stream registration failed: {message}"),
            Self::Spawn(message) => write!(formatter, "signal forwarder thread failed: {message}"),
        }
    }
}

impl std::error::Error for SignalForwardError {}

/// One attached OS signal forwarder (§7.1): real SIGINT/SIGTERM
/// deliveries land in the shared [`SignalState`].
///
/// The forwarder thread runs until process exit; the shutdown sequence
/// (§7.4) replaces this with a controlled stop.
#[derive(Debug)]
pub struct SignalForwarder {
    handle: std::thread::JoinHandle<()>,
}

impl SignalForwarder {
    /// Waits for the forwarder thread to exit (which never happens on its
    /// own before process exit; §7.4 adds a controlled stop).
    pub fn join(self) -> std::thread::Result<()> {
        self.handle.join()
    }
}

/// Spawns the OS signal forwarder feeding `state` (§7.1): SIGINT and
/// SIGTERM streams on a dedicated thread with a Tokio current-thread
/// executor, applying the same [`SignalState::deliver`] policy as the
/// injectable path.
///
/// # Errors
///
/// Returns a [`SignalForwardError`] when the executor, the streams, or
/// the thread cannot be created.
pub fn spawn_signal_forwarder(state: SignalState) -> Result<SignalForwarder, SignalForwardError> {
    use tokio::signal::unix::{SignalKind, signal};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|error| SignalForwardError::Executor(error.to_string()))?;
    let mut sigint = signal(SignalKind::interrupt())
        .map_err(|error| SignalForwardError::Stream(error.to_string()))?;
    let mut sigterm = signal(SignalKind::terminate())
        .map_err(|error| SignalForwardError::Stream(error.to_string()))?;
    let handle = std::thread::Builder::new()
        .name("fusor-signals".to_owned())
        .spawn(move || {
            runtime.block_on(async move {
                loop {
                    tokio::select! {
                        received = sigint.recv() => {
                            if received.is_some() {
                                state.deliver(Signal::Interrupt);
                            }
                        }
                        received = sigterm.recv() => {
                            if received.is_some() {
                                state.deliver(Signal::Terminate);
                            }
                        }
                    }
                }
            });
        })
        .map_err(|error| SignalForwardError::Spawn(error.to_string()))?;
    Ok(SignalForwarder { handle })
}
