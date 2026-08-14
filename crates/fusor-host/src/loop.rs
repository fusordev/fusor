//! ECMA-262 host event loop (§6): a continuously alive turn loop driving
//! the engine's host-job queue on one owner task.
//!
//! The loop runs over a virtual clock (§6.4, §12.2: tests advance time
//! deterministically; real waiting is a later event source). Every turn
//! handles the due events (timers, async-op completions, custom host
//! events), runs the `setImmediate` queue, and — after **every** host event
//! — drains `drain_host_jobs` to quiescence, the ECMA-262 microtask
//! checkpoint. Host callbacks never interleave with pending jobs (§6.2
//! normative pin).

mod timers;

pub(crate) use timers::{TimerCallback, TimerId, with_timer_state};

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::time::Duration;

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
    exit_when_idle: bool,
}

impl HostLoop {
    /// Creates the loop around one realm with a fresh virtual clock (§6.1).
    ///
    /// The Tokio executor of the eventual select-wait returns together with
    /// the signal event source (subproject 4); until then the loop is
    /// driven synchronously by [`Self::run_one_turn`] /
    /// [`Self::run_until_idle`] over the virtual clock.
    ///
    /// # Errors
    ///
    /// Returns [`HostLoopError::AlreadyInstalled`] when another loop owns
    /// this thread.
    pub fn new(runtime: Runtime, realm: Realm) -> Result<Self, HostLoopError> {
        install_timer_state(TimerState::default())
            .map_err(|_| HostLoopError::AlreadyInstalled)?;
        Ok(Self {
            runtime,
            realm,
            custom_events: VecDeque::new(),
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
            Err(GlobalScriptError::Execution(source)) => return Err(source),
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
    /// # Errors
    ///
    /// Returns an uncatchable job failure or a host callback error.
    pub fn run_one_turn(&mut self) -> Result<(), ExecutionError> {
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
    /// # Errors
    ///
    /// Returns a timer-callback failure.
    pub fn advance_time(&mut self, duration: Duration) -> Result<(), ExecutionError> {
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
    /// order (earliest deadline, then creation sequence, §6.4), draining to
    /// quiescence after each callback (§6.2). A repeating timer re-arms at
    /// `now + delay`; a zero-delay re-arm therefore fires in the next
    /// sweep, never twice in one.
    fn fire_due_timers(&mut self) -> Result<(), ExecutionError> {
        let due = with_timer_state(|state| {
            let now = state.now;
            let mut due = Vec::new();
            while let Some(entry) = state.heap.peek() {
                if entry.deadline > now {
                    break;
                }
                due.push(*entry);
                state.heap.pop();
            }
            due
        })
        .ok()
        .unwrap_or_default();
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
            invoke_callback(&mut context, &callback.callback)?;
            self.drain()?;
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
        Ok(())
    }

    /// Runs every queued `setImmediate` callback once (§6.4).
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
            invoke_callback(&mut context, &callback.callback)?;
        }
    }

    /// Queues one custom host event for the next turn (§6.3 event source ⑤).
    pub fn post_event(&mut self, event: HostEvent) {
        self.custom_events.push_back(event);
    }

    /// Returns whether any alive event source or pending work remains.
    #[must_use]
    pub fn alive(&self) -> bool {
        self.exit_when_idle
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
            .finish()
    }
}
