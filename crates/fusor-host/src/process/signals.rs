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
//!
//! A JS-side SIGINT handler registered through
//! `Fusor.ops.op_process_on` replaces the default policy: each delivery
//! is handed to the handler by the loop instead of arming the interrupt
//! or the exit, and registering clears any pending interrupt request.
//! SIGTERM is not interceptable in the alpha host (documented).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

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
/// engine's [`InterruptHandler`], the pending force-exit code, and the
/// JS-handler delivery channel.
///
/// All mutations are atomic, so OS signal-delivery threads and the owner
/// task share one state without further synchronization (§7.6: signal
/// delivery is injectable and thread-safe by construction). This type
/// never panics and is cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct SignalState {
    interrupt: Arc<AtomicBool>,
    exit_requested: Arc<AtomicBool>,
    exit_code: Arc<AtomicI32>,
    js_sigint_handler: Arc<AtomicBool>,
    pending_sigint: Arc<AtomicU32>,
}

impl SignalState {
    /// Delivers one signal, applying the §7.1 policy.
    ///
    /// Returns `true` when this delivery forces an exit (a second SIGINT
    /// while a request was outstanding, or any SIGTERM). With a JS SIGINT
    /// handler registered the delivery is handed to the handler instead
    /// and returns `false`.
    pub fn deliver(&self, signal: Signal) -> bool {
        match signal {
            Signal::Interrupt => {
                if self.js_sigint_handler.load(Ordering::SeqCst) {
                    self.pending_sigint.fetch_add(1, Ordering::SeqCst);
                    false
                } else if self.interrupt.swap(true, Ordering::SeqCst) {
                    // `swap(true)` reports whether a request was already
                    // outstanding: that makes this delivery the second
                    // SIGINT.
                    self.request_exit(signal.exit_code());
                    true
                } else {
                    false
                }
            }
            Signal::Terminate => {
                self.request_exit(signal.exit_code());
                true
            }
        }
    }

    /// Requests a process exit with the given code (force signals carry
    /// `128 + n`, `op_process_exit` the caller's truncated code, §7.2).
    ///
    /// Only the first request wins: the exit code never resets, so a
    /// later signal or `process.exit` call cannot replace it. Returns
    /// `true` when this call is the one that requested the exit.
    pub(crate) fn request_exit(&self, code: i32) -> bool {
        if self.exit_requested.swap(true, Ordering::SeqCst) {
            false
        } else {
            self.exit_code.store(code, Ordering::SeqCst);
            true
        }
    }

    /// Enables or disables the JS SIGINT handler path (§7.1). The owner
    /// task toggles this when `op_process_on("SIGINT", ...)` registers;
    /// the delivery thread reads it.
    pub(crate) fn set_js_sigint_handler(&self, registered: bool) {
        self.js_sigint_handler.store(registered, Ordering::SeqCst);
    }

    /// Returns whether SIGINT deliveries are waiting for the JS handler
    /// (the loop's alive/work predicates).
    pub(crate) fn has_pending_sigint(&self) -> bool {
        self.pending_sigint.load(Ordering::SeqCst) > 0
    }

    /// Takes every pending JS-handler delivery, returning the count to
    /// invoke (§7.1).
    pub(crate) fn take_pending_sigint(&self) -> u32 {
        self.pending_sigint.swap(0, Ordering::SeqCst)
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

    /// Returns the pending exit code (§7.2), if an exit was requested.
    ///
    /// The code never resets: the first exit request terminates the
    /// process.
    #[must_use]
    pub fn pending_exit_code(&self) -> Option<i32> {
        if self.exit_requested.load(Ordering::SeqCst) {
            Some(self.exit_code.load(Ordering::SeqCst))
        } else {
            None
        }
    }
}

/// Installs the owner-task [`SignalState`] clone into the op-state
/// registry (the `op_process_on` op entry point).
///
/// # Errors
///
/// Returns the state unchanged when one is already installed.
pub(crate) fn install_signal_state(state: SignalState) -> Result<(), SignalState> {
    crate::ops::OpStateRegistry::install(state)
}

/// Removes the installed signal state (shutdown teardown, §7.4) so a
/// fresh loop can install on the same thread.
#[must_use]
pub(crate) fn take_signal_state() -> Option<SignalState> {
    crate::ops::OpStateRegistry::take::<SignalState>()
}

/// Borrows the installed signal state (the `process.on` op entry point).
pub(crate) fn with_signal_state<R>(
    operation: impl FnOnce(&SignalState) -> R,
) -> Result<R, crate::ops::OpStateError> {
    crate::ops::OpStateRegistry::with::<SignalState, R>(operation)
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
/// The forwarder thread runs until [`SignalForwarder::shutdown`] stops it
/// (shutdown step ①, §7.4).
#[derive(Debug)]
pub struct SignalForwarder {
    handle: std::thread::JoinHandle<()>,
    stop: tokio::sync::oneshot::Sender<()>,
}

impl SignalForwarder {
    /// Stops the forwarder (shutdown step ①, §7.4): the signal streams
    /// are dropped, the thread joins, and OS signals regain their default
    /// disposition.
    pub fn shutdown(self) -> std::thread::Result<()> {
        let _ = self.stop.send(());
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
        .enable_io()
        .build()
        .map_err(|error| SignalForwardError::Executor(error.to_string()))?;
    // The signal streams register against the reactor, so they must be
    // created inside a runtime context: the caller drives one bootstrap
    // block_on and the streams move into the forwarder thread.
    let (sigint, sigterm) = runtime.block_on(async {
        let sigint = signal(SignalKind::interrupt())
            .map_err(|error| SignalForwardError::Stream(error.to_string()))?;
        let sigterm = signal(SignalKind::terminate())
            .map_err(|error| SignalForwardError::Stream(error.to_string()))?;
        Ok::<_, SignalForwardError>((sigint, sigterm))
    })?;
    let (stop, mut stop_rx) = tokio::sync::oneshot::channel();
    let handle = std::thread::Builder::new()
        .name("fusor-signals".to_owned())
        .spawn(move || {
            runtime.block_on(async move {
                let mut sigint = sigint;
                let mut sigterm = sigterm;
                loop {
                    tokio::select! {
                        _ = &mut stop_rx => break,
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
    Ok(SignalForwarder { handle, stop })
}
