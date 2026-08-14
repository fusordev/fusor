//! ECMA-262 host event loop (§6): a continuously alive turn loop driving
//! the engine's host-job queue on one owner task.
//!
//! The loop runs over a virtual clock (§6.4, §12.2: tests advance time
//! deterministically; real waiting is a later event source). Every turn
//! handles the due events (timers, async-op completions, signal
//! deliveries §7.1, custom host events), runs the `setImmediate` queue,
//! and — after **every** host event — drains `drain_host_jobs` to
//! quiescence, the ECMA-262 microtask checkpoint. Host callbacks never
//! interleave with pending jobs (§6.2 normative pin).

mod timers;

pub(crate) use timers::{TimerCallback, TimerId, with_timer_state};

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::process::{Signal, SignalState};
use fusor_runtime::{Context, ExecutionError, ExecutionLimits, GlobalScriptError, Realm, Runtime};
use timers::{TimerState, install_timer_state};

/// One custom host event: a closure the loop invokes on the owner task.
///
/// The closure receives the context for its turn; engine interaction can
/// only happen inside it, on the owner task.
pub type HostEvent = Box<dyn FnOnce(&mut Context<'_>) -> Result<(), ExecutionError>>;

/// Host-loop construction failures.
#[derive(Debug)]
pub enum HostLoopError {
    /// Another [`HostLoop`] is already installed on this thread (one loop
    /// per owner task, §6.1).
    AlreadyInstalled,
}

impl fmt::Display for HostLoopError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInstalled => {
                formatter.write_str("a HostLoop is already installed on this thread")
            }
        }
    }
}

impl Error for HostLoopError {}

/// The host event loop (§6.1): continuously alive, owner-task driven.
///
/// The engine [`Runtime`] is owned here and only ever touched inside turn
/// callbacks on the thread that created the loop. The virtual clock starts
/// at loop construction; [`Self::advance_time`] moves it forward and fires
/// due timers deterministically.
pub struct HostLoop {
    runtime: Runtime,
    realm: Realm,
    custom_events: VecDeque<HostEvent>,
    signals: SignalState,
    exit_when_idle: bool,
}

impl HostLoop {
    /// Creates the loop around one realm with a fresh virtual clock (§6.1)
    /// and the shared signal state (§7.1). The engine's
    /// [`InterruptHandler`] is installed here so a signal delivery thread
    /// can cancel a running script at the next instruction-boundary poll.
    ///
    /// The loop is driven synchronously by [`Self::run_one_turn`] /
    /// [`Self::run_until_idle`] over the virtual clock; OS signal
    /// deliveries reach the shared state through
    /// [`crate::process::spawn_signal_forwarder`] without an executor of
    /// their own on the loop.
    ///
    /// # Errors
    ///
    /// Returns [`HostLoopError::AlreadyInstalled`] when another loop owns
    /// this thread.
    pub fn new(mut runtime: Runtime, realm: Realm) -> Result<Self, HostLoopError> {
        install_timer_state(TimerState::default())
            .map_err(|_| HostLoopError::AlreadyInstalled)?;
        let signals = SignalState::default();
        runtime.set_interrupt_handler(Arc::new({
            let signals = signals.clone();
            move || signals.interrupt_requested()
        }));
        Ok(Self {
            runtime,
            realm,
            custom_events: VecDeque::new(),
            signals,
            exit_when_idle: true,
        })
    }

    /// Evaluates the main Global Script, then runs turns until no alive
    /// event source remains (§6.5).
    ///
    /// The compiled authority is produced by the caller (the frontend). The
    /// builder form from §6.5 — `HostRuntime::run_main` with overlays,
    /// snapshots, and init sources — lands with subproject 6.
    ///
    /// # Errors
    ///
    /// Returns the script installation or execution failure, or the first
    /// turn failure.
    pub fn run_main(
        &mut self,
        authority: std::sync::Arc<fusor_bytecode::VerifiedBytecode>,
        limits: ExecutionLimits,
    ) -> Result<(), ExecutionError> {
        let mut context = self
            .runtime
            .context(&self.realm)
            .map_err(ExecutionError::from)?;
        match context.execute_global_script(authority, limits) {
            Ok(_) => {}
            Err(GlobalScriptError::Install(source)) => return Err(source.into()),
            Err(GlobalScriptError::Execution(source)) => {
                if matches!(&source, ExecutionError::Interrupted { .. }) {
                    // The running script consumed the request (§7.1).
                    self.signals.consume_interrupt();
                }
                return Err(source);
            }
        }
        self.run_until_idle()
    }

    /// Runs turns until no alive event source and no pending work remains,
    /// then returns (the §6.5 exit condition). While only future timers
    /// remain alive, the virtual clock advances to the next deadline
    /// (simulating the select wait, §6.3).
    ///
    /// # Errors
    ///
    /// Returns the first turn failure (an uncatchable job failure or a host
    /// callback error).
    pub fn run_until_idle(&mut self) -> Result<(), ExecutionError> {
        while self.alive() {
            if self.turn_has_work() {
                self.run_one_turn()?;
            } else if let Some(deadline) = self.next_deadline() {
                // Nothing due: simulate the select wait until the next timer.
                self.advance_to(deadline)?;
                self.run_one_turn()?;
            } else {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Executes one complete turn (§6.2, §6.4): due timers and custom
    /// events — each followed immediately by the microtask checkpoint —
    /// then the `setImmediate` queue and the turn-final checkpoint.
    ///
    /// A signal request aborts the turn with
    /// [`ExecutionError::Interrupted`] and is consumed by the abort;
    /// timers and immediates that did not complete their firing stay
    /// registered and run in a later turn, while a failed custom event is
    /// dropped (one-shot host work is the embedder's to retry, §7.1). An
    /// interrupt request that reaches the end of a turn without being
    /// consumed dies with the turn (an idle Ctrl+C interrupts nothing).
    /// After a force exit the turn is a no-op.
    ///
    /// # Errors
    ///
    /// Returns an uncatchable job failure, a host callback error, or an
    /// interrupt abort.
    pub fn run_one_turn(&mut self) -> Result<(), ExecutionError> {
        if self.signals.pending_exit_code().is_some() {
            // Force exit: the loop no longer runs work (§7.1).
            return Ok(());
        }
        let result = self.run_turn_body();
        if matches!(&result, Err(ExecutionError::Interrupted { .. })) {
            // The running script consumed the request (§7.1).
            self.signals.consume_interrupt();
        } else if result.is_ok() && self.signals.interrupt_requested() {
            // Idle turn: nothing was there to interrupt, so the request
            // dies with the turn (an idle Ctrl+C interrupts nothing).
            self.signals.consume_interrupt();
        }
        result
    }

    /// The turn body without the signal bookkeeping of
    /// [`Self::run_one_turn`].
    fn run_turn_body(&mut self) -> Result<(), ExecutionError> {
        // Async-op settlements are host events: resolve/reject queues
        // Promise reactions that must reach quiescence before any further
        // host callback (§6.2).
        self.poll_op_completions();
        self.drain()?;
        self.fire_due_timers()?;
        while let Some(event) = self.custom_events.pop_front() {
            let mut context = self
                .runtime
                .context(&self.realm)
                .map_err(ExecutionError::from)?;
            event(&mut context)?;
            self.drain()?;
        }
        // `setImmediate` runs after this turn's events, before the
        // turn-final checkpoint (§6.4).
        self.run_immediates()?;
        self.drain()?;
        Ok(())
    }

    /// Runs the ECMA-262 microtask checkpoint to quiescence; called
    /// immediately after every host event (§6.2).
    fn drain(&mut self) -> Result<(), ExecutionError> {
        let mut context = self
            .runtime
            .context(&self.realm)
            .map_err(ExecutionError::from)?;
        context.drain_host_jobs(ExecutionLimits::default(), None)
    }

    /// Advances the virtual clock, firing every timer that comes due.
    ///
    /// After a force exit the clock no longer advances (§7.1).
    ///
    /// # Errors
    ///
    /// Returns a timer-callback failure or an interrupt abort.
    pub fn advance_time(&mut self, duration: Duration) -> Result<(), ExecutionError> {
        if self.signals.pending_exit_code().is_some() {
            // Force exit: the loop no longer advances (§7.1).
            return Ok(());
        }
        self.advance_to(self.now() + duration)
    }

    /// Advances the virtual clock to an absolute deadline.
    fn advance_to(&mut self, deadline: std::time::Instant) -> Result<(), ExecutionError> {
        with_timer_state(|state| {
            if deadline > state.now {
                state.now = deadline;
            }
        })
        .map_err(|_| ExecutionError::from(fusor_runtime::EngineFault::RuntimeInvariant {
            message: "timer state vanished",
        }))?;
        self.fire_due_timers()
    }

    /// Returns the virtual clock reading.
    fn now(&self) -> std::time::Instant {
        with_timer_state(|state| state.now).unwrap_or(std::time::Instant::now())
    }

    /// Returns the next timer deadline, if any.
    fn next_deadline(&self) -> Option<std::time::Instant> {
        with_timer_state(|state| state.next_deadline()).ok().flatten()
    }

    /// Returns whether any event is due this turn.
    fn turn_has_work(&self) -> bool {
        !self.custom_events.is_empty()
            || with_timer_state(|state| {
                state.next_deadline().is_some_and(|deadline| deadline <= state.now)
                    || !state.immediates.is_empty()
            })
            .unwrap_or(false)
            || self.pending_ops() > 0
    }

    /// Fires every timer whose deadline was due at sweep start, in heap
    /// order (earliest deadline, then creation sequence, §6.4), draining
    /// to quiescence after each callback (§6.2). A repeating timer re-arms
    /// at `now + delay`; a zero-delay re-arm therefore fires in the next
    /// sweep, never twice in one. A callback that fails (for example an
    /// interrupt) stays registered and fires again in a later sweep; the
    /// sweep finally prunes heap entries whose callbacks are gone.
    fn fire_due_timers(&mut self) -> Result<(), ExecutionError> {
        let due = with_timer_state(|state| {
            let now = state.now;
            state
                .heap
                .clone()
                .into_sorted_vec()
                .into_iter()
                // The heap's Ord is reversed (earlier deadlines are
                // "greater"), so ascending sort order is the reverse of
                // pop order.
                .rev()
                .take_while(|entry| entry.deadline <= now)
                .collect::<Vec<_>>()
        })
        .ok()
        .unwrap_or_default();
        // The heap entries of completed firings, pruned after the sweep:
        // a re-armed timer keeps its id registered, so pruning by id alone
        // would also remove its fresh entry — only the exact fired tuples
        // are stale.
        let mut fired = std::collections::HashSet::with_capacity(due.len());
        for entry in due {
            let callback = with_timer_state(|state| state.callbacks.remove(&entry.id))
                .ok()
                .flatten();
            let Some(callback) = callback else {
                continue;
            };
            let mut context = self
                .runtime
                .context(&self.realm)
                .map_err(ExecutionError::from)?;
            if let Err(error) = invoke_callback(&mut context, &callback.callback) {
                // The timer did not complete its firing: keep it
                // registered so it fires again in a later turn.
                with_timer_state(|state| {
                    state.callbacks.insert(entry.id, callback);
                })
                .ok();
                return Err(error);
            }
            self.drain()?;
            fired.insert(entry);
            if callback.repeating {
                with_timer_state(|state| {
                    let deadline = state.now + callback.delay;
                    let sequence = state.sequence;
                    state.heap.push(timers::TimerEntry {
                        deadline,
                        sequence,
                        id: entry.id,
                    });
                    state.callbacks.insert(entry.id, callback);
                })
                .map_err(|_| {
                    ExecutionError::from(fusor_runtime::EngineFault::RuntimeInvariant {
                        message: "timer state vanished",
                    })
                })?;
            }
        }
        // Prune stale heap entries: completed firings and canceled timers.
        with_timer_state(|state| {
            state
                .heap
                .retain(|entry| !fired.contains(entry) && state.callbacks.contains_key(&entry.id));
        })
        .ok();
        Ok(())
    }

    /// Runs every queued `setImmediate` callback once (§6.4). A callback
    /// that fails (for example an interrupt) is restored to the queue and
    /// runs again in a later turn.
    fn run_immediates(&mut self) -> Result<(), ExecutionError> {
        loop {
            let immediate = with_timer_state(|state| state.immediates.pop_front())
                .ok()
                .flatten();
            let Some(id) = immediate else {
                return Ok(());
            };
            let callback = with_timer_state(|state| state.callbacks.remove(&id))
                .ok()
                .flatten();
            let Some(callback) = callback else {
                continue;
            };
            let mut context = self
                .runtime
                .context(&self.realm)
                .map_err(ExecutionError::from)?;
            if let Err(error) = invoke_callback(&mut context, &callback.callback) {
                with_timer_state(|state| {
                    state.immediates.push_front(id);
                    state.callbacks.insert(id, callback);
                })
                .ok();
                return Err(error);
            }
        }
    }

    /// Queues one custom host event for the next turn (§6.3 event source ⑤).
    pub fn post_event(&mut self, event: HostEvent) {
        self.custom_events.push_back(event);
    }

    /// Delivers one signal through the injectable event source (§7.6):
    /// the same [`SignalState::deliver`] policy as OS delivery — the first
    /// SIGINT requests an interrupt, a second SIGINT while the request is
    /// outstanding or any SIGTERM requests a force exit.
    pub fn post_signal(&mut self, signal: Signal) {
        self.signals.deliver(signal);
    }

    /// Returns the pending force-exit code (§7.2), if any.
    ///
    /// A force exit never resets: the loop stops running work until the
    /// process exits with this code.
    #[must_use]
    pub fn pending_exit_code(&self) -> Option<i32> {
        self.signals.pending_exit_code()
    }

    /// Returns whether any alive event source or pending work remains.
    #[must_use]
    pub fn alive(&self) -> bool {
        self.signals.pending_exit_code().is_none()
            && self.exit_when_idle
            && (!self.custom_events.is_empty()
                || self.pending_ops() > 0
                || with_timer_state(|state| state.has_pending()).unwrap_or(false))
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

/// Invokes one timer/immediate callback with the job-callback semantics:
/// an ordinary `[[Call]]` with the `undefined` receiver (§6.4).
fn invoke_callback(context: &mut Context<'_>, callback: &fusor_runtime::JsValue) -> Result<(), ExecutionError> {
    let function = callback
        .clone()
        .into_function()
        .map_err(ExecutionError::from)?;
    context
        .call_function(
            &function,
            context.undefined(),
            Vec::new(),
            ExecutionLimits::default(),
        )
        .map(|_completion| ())
        .map_err(|error| match error {
            fusor_runtime::CallError::Execution(source) => source,
            fusor_runtime::CallError::Thrown(_) => {
                ExecutionError::from(fusor_runtime::EngineFault::RuntimeInvariant {
                    message: "timer callback threw an uncaught exception",
                })
            }
        })
}

impl fmt::Debug for HostLoop {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostLoop")
            .field("custom_events", &self.custom_events.len())
            .field("exit_when_idle", &self.exit_when_idle)
            .field("pending_exit_code", &self.signals.pending_exit_code())
            .finish()
    }
}
