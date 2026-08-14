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
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crate::process::{
    ExitCode, ProcessState, RejectionQueue, Signal, SignalState, has_pending_rejections,
    install_process_state, install_signal_state, push_rejection_event, take_process_state,
    take_rejection_events, take_signal_state, with_process_state,
};
use fusor_runtime::{
    CallError, Context, ErrorObjectKind, ExceptionKind, ExecutionError, ExecutionLimits,
    GlobalScriptError, JsException, JsValue, PromiseRejectionEvent, PromiseRejectionOperation,
    Realm, Runtime,
};
use timers::{TimerState, install_timer_state, take_timer_state};

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
        install_process_state(ProcessState::default())
            .map_err(|_| HostLoopError::AlreadyInstalled)?;
        crate::ops::OpStateRegistry::install(RejectionQueue::default())
            .map_err(|_| HostLoopError::AlreadyInstalled)?;
        let signals = SignalState::default();
        install_signal_state(signals.clone()).map_err(|_| HostLoopError::AlreadyInstalled)?;
        runtime.set_interrupt_handler(Arc::new({
            let signals = signals.clone();
            move || signals.interrupt_requested()
        }));
        // The rejection tracker (§7.3) runs while JavaScript is suspended
        // and must not re-enter the runtime: it retains each notification
        // into the end-of-turn queue. A retain failure (root resource
        // exhaustion) drops the notification; the rejection itself is
        // unaffected.
        runtime.set_promise_rejection_tracker(Rc::new(
            |mut event: PromiseRejectionEvent<'_>| {
                if let Ok(owned) = event.retain() {
                    push_rejection_event(owned);
                }
            },
        ));
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
                    return Err(source);
                }
                match source {
                    // §7.3: an uncaught exception goes through the
                    // uncaughtException handler or the default exit-1
                    // path.
                    ExecutionError::Exception(exception) => {
                        let value = exception_value(&mut context, &exception)?;
                        self.handle_uncaught(value)?;
                    }
                    other => return Err(other),
                }
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
        self.drain_routing()?;
        self.run_signal_handlers()?;
        self.fire_due_timers()?;
        while let Some(event) = self.custom_events.pop_front() {
            let mut context = self
                .runtime
                .context(&self.realm)
                .map_err(ExecutionError::from)?;
            match event(&mut context) {
                Ok(()) => {}
                // §7.3: an uncaught exception in host event work goes
                // through the uncaughtException path; the event is
                // dropped (one-shot host work).
                Err(ExecutionError::Exception(exception)) => {
                    let value = exception_value(&mut context, &exception)?;
                    self.handle_uncaught(value)?;
                }
                Err(error) => return Err(error),
            }
            self.drain_routing()?;
        }
        // `setImmediate` runs after this turn's events, before the
        // turn-final checkpoint (§6.4).
        self.run_immediates()?;
        self.drain_routing()?;
        // The rejection tracker's notifications reconcile at the end of
        // the turn, after the final checkpoint (§7.3).
        self.handle_promise_rejections()?;
        Ok(())
    }

    /// Runs the ECMA-262 microtask checkpoint to quiescence; called
    /// immediately after every host event (§6.2). An escaping job
    /// exception propagates to the caller (handler-internal drains fail
    /// closed on it).
    fn drain(&mut self) -> Result<(), ExecutionError> {
        let mut context = self
            .runtime
            .context(&self.realm)
            .map_err(ExecutionError::from)?;
        context.drain_host_jobs(ExecutionLimits::default(), None)
    }

    /// The turn-level checkpoint: like [`Self::drain`], but an escaping
    /// job exception (a throwing microtask, §7.3) routes through the
    /// uncaught path instead of failing the turn.
    fn drain_routing(&mut self) -> Result<(), ExecutionError> {
        let mut context = self
            .runtime
            .context(&self.realm)
            .map_err(ExecutionError::from)?;
        match context.drain_host_jobs(ExecutionLimits::default(), None) {
            Err(ExecutionError::Exception(exception)) => {
                let value = exception_value(&mut context, &exception)?;
                self.handle_uncaught(value)
            }
            other => other,
        }
    }

    /// Invokes the registered JS SIGINT handler once per pending delivery
    /// (§7.1), draining to quiescence after each invocation (§6.2). The
    /// receiver is `undefined` (the process object surface is removed,
    /// §7.1 note); a handler that throws goes to the uncaughtException
    /// path (Node semantics) and its delivery is spent.
    fn run_signal_handlers(&mut self) -> Result<(), ExecutionError> {
        let pending = self.signals.take_pending_sigint();
        for _ in 0..pending {
            let Some(handler) = with_process_state(|state| state.sigint_handler.clone())
                .ok()
                .flatten()
            else {
                return Ok(());
            };
            let mut context = self
                .runtime
                .context(&self.realm)
                .map_err(ExecutionError::from)?;
            let receiver = context.undefined();
            match invoke_callback_with(&mut context, &handler, receiver, Vec::new()) {
                Ok(()) => {}
                Err(CallError::Thrown(error)) => self.handle_uncaught(error)?,
                Err(CallError::Execution(source)) => return Err(source),
            }
            self.drain()?;
        }
        Ok(())
    }

    /// Routes one uncaught JavaScript exception (§7.3): with a registered
    /// `Fusor.ops.op_process_on("uncaughtException")` handler the handler runs as
    /// a host event with the error value (receiver `undefined`) and its
    /// jobs drain immediately; otherwise the default path renders the
    /// exception and requests exit 1.
    fn handle_uncaught(&mut self, error: JsValue) -> Result<(), ExecutionError> {
        let Some(handler) = with_process_state(|state| state.uncaught_handler.clone())
            .ok()
            .flatten()
        else {
            let mut context = self
                .runtime
                .context(&self.realm)
                .map_err(ExecutionError::from)?;
            let message = value_text(&mut context, &error);
            // The default path renders through the unified §7.5
            // pipeline (color policy from the environment).
            let report = crate::process::render_diagnostic(
                crate::process::MessageDiagnostic::new(format!("Uncaught exception: {message}")),
                crate::process::ColorPolicy::from_env(),
            );
            eprint!("{report}");
            self.signals.request_exit(1);
            return Ok(());
        };
        let mut context = self
            .runtime
            .context(&self.realm)
            .map_err(ExecutionError::from)?;
        let receiver = context.undefined();
        match invoke_callback_with(&mut context, &handler, receiver, vec![error]) {
            Ok(()) => {}
            // A throwing uncaughtException handler is not routed again:
            // fail closed with a typed error.
            Err(CallError::Thrown(_)) => {
                return Err(ExecutionError::from(
                    fusor_runtime::EngineFault::RuntimeInvariant {
                        message: "the uncaughtException handler threw an uncaught exception",
                    },
                ));
            }
            Err(CallError::Execution(source)) => return Err(source),
        }
        self.drain()
    }

    /// Reconciles the end-of-turn rejection notifications (§7.3): a
    /// `Reject` whose Promise gained no handler within the turn is an
    /// unhandled rejection — the registered handler receives
    /// `(reason, promise)` (receiver `undefined`), or the default path
    /// warns and requests exit 1. `Handle` notifications only cancel
    /// their matching `Reject`.
    fn handle_promise_rejections(&mut self) -> Result<(), ExecutionError> {
        let events = take_rejection_events();
        if events.is_empty() {
            return Ok(());
        }
        let mut unhandled: Vec<fusor_runtime::OwnedPromiseRejectionEvent> = Vec::new();
        for event in events {
            match event.operation() {
                PromiseRejectionOperation::Reject => unhandled.push(event),
                PromiseRejectionOperation::Handle => {
                    let promise = event.promise().as_value();
                    unhandled.retain(|pending| {
                        !pending.promise().as_value().same_object(&promise)
                    });
                }
            }
        }
        let handler = with_process_state(|state| state.unhandled_rejection_handler.clone())
            .ok()
            .flatten();
        for event in unhandled {
            let (_, promise, reason) = event.into_parts();
            let Some(handler) = handler.clone() else {
                let mut context = self
                    .runtime
                    .context(&self.realm)
                    .map_err(ExecutionError::from)?;
                let message = value_text(&mut context, &reason);
                let report = crate::process::render_diagnostic(
                    crate::process::MessageDiagnostic::new(format!(
                        "Unhandled promise rejection: {message}"
                    )),
                    crate::process::ColorPolicy::from_env(),
                );
                eprint!("{report}");
                self.signals.request_exit(1);
                continue;
            };
            let mut context = self
                .runtime
                .context(&self.realm)
                .map_err(ExecutionError::from)?;
            let receiver = context.undefined();
            match invoke_callback_with(
                &mut context,
                &handler,
                receiver,
                vec![reason, promise.as_value()],
            ) {
                Ok(()) => {}
                Err(CallError::Thrown(_)) => {
                    return Err(ExecutionError::from(
                        fusor_runtime::EngineFault::RuntimeInvariant {
                            message: "the unhandledRejection handler threw an uncaught exception",
                        },
                    ));
                }
                Err(CallError::Execution(source)) => return Err(source),
            }
            self.drain()?;
        }
        Ok(())
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

    /// Returns the virtual duration until the next timer deadline (§6.4),
    /// or `None` when no timer is pending.
    ///
    /// Hosts that wait in real time (V8 semantics — a delayed timer fires
    /// after its delay, not instantly) sleep this long, then call
    /// [`Self::advance_time`] with the same duration and run a turn. The
    /// virtual clock never advances on its own; [`Self::run_until_idle`]
    /// advances it instantly for hosts that prefer the simulated select.
    #[must_use]
    pub fn next_deadline_in(&self) -> Option<Duration> {
        with_timer_state(|state| {
            state
                .next_deadline()
                .map(|deadline| deadline.saturating_duration_since(state.now))
        })
        .ok()
        .flatten()
    }

    /// Returns whether any event is due this turn.
    fn turn_has_work(&self) -> bool {
        !self.custom_events.is_empty()
            || self.signals.has_pending_sigint()
            || has_pending_rejections()
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
            match invoke_callback(&mut context, &callback.callback) {
                Ok(()) => {}
                // A throwing callback is spent (§7.3): the exception
                // goes to the uncaughtException path and the sweep
                // continues.
                Err(CallError::Thrown(error)) => self.handle_uncaught(error)?,
                // The callback did not complete its firing: keep it
                // registered so it fires again in a later turn.
                Err(CallError::Execution(source)) => {
                    with_timer_state(|state| {
                        state.callbacks.insert(entry.id, callback);
                    })
                    .ok();
                    return Err(source);
                }
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
            match invoke_callback(&mut context, &callback.callback) {
                Ok(()) => {}
                // A throwing callback is spent (§7.3).
                Err(CallError::Thrown(error)) => self.handle_uncaught(error)?,
                // The callback did not complete: restore it to the queue
                // for a later turn.
                Err(CallError::Execution(source)) => {
                    with_timer_state(|state| {
                        state.immediates.push_front(id);
                        state.callbacks.insert(id, callback);
                    })
                    .ok();
                    return Err(source);
                }
            }
        }
    }

    /// Queues one custom host event for the next turn (§6.3 event source ⑤).
    pub fn post_event(&mut self, event: HostEvent) {
        self.custom_events.push_back(event);
    }

    /// Runs the documented shutdown sequence (§7.4):
    ///
    /// ① consuming the loop stops every event source;
    /// ② the installed [`crate::ops::OpRuntime`] is dropped, cancelling
    ///    every pending async-op future (Tokio cancellation);
    /// ③ every table-exclusive resource closes
    ///    ([`crate::ops::close_all_resources`], §5.6);
    /// ④ `Atomics.waitAsync` waiters cancel through the engine's own
    ///    drop path when the runtime goes;
    /// ⑤ the [`Runtime`] is dropped, and with it every remaining
    ///    loop-owned thread-local state — no microtasks drain in between
    ///    (§7.4).
    ///
    /// Returns the process exit code the driver should use: the pending
    /// requested code, or [`ExitCode::Clean`].
    #[must_use]
    pub fn shutdown(self) -> ExitCode {
        let exit_code = match self.signals.pending_exit_code() {
            Some(code) => ExitCode::Requested(code),
            None => ExitCode::Clean,
        };
        // ② Cancel pending async ops: dropping the op runtime drops its
        // executor and every spawned future.
        drop(crate::ops::take_op_runtime());
        // ③ Close table-exclusive resources (§5.6).
        crate::ops::close_all_resources();
        // ④–⑤ Tear down the loop-owned state (pending callbacks and
        // handler roots release while the runtime mailbox is still
        // alive), then drop the Runtime. No drain happens in between.
        drop(take_process_state());
        drop(crate::ops::OpStateRegistry::take::<RejectionQueue>());
        drop(take_timer_state());
        drop(take_signal_state());
        drop(self);
        exit_code
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
                || self.signals.has_pending_sigint()
                || has_pending_rejections()
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
fn invoke_callback(
    context: &mut Context<'_>,
    callback: &JsValue,
) -> Result<(), CallError> {
    invoke_callback_with(context, callback, context.undefined(), Vec::new())
}

/// Invokes one host callback with an explicit receiver and argument list
/// (timer, signal, and exception handlers all use the `undefined`
/// receiver, §6.4, §7.1, §7.3).
fn invoke_callback_with(
    context: &mut Context<'_>,
    callback: &JsValue,
    receiver: JsValue,
    arguments: Vec<JsValue>,
) -> Result<(), CallError> {
    let function = callback
        .clone()
        .into_function()
        .map_err(|error| CallError::Execution(ExecutionError::from(error)))?;
    context
        .call_function(
            &function,
            receiver,
            arguments,
            ExecutionLimits::default(),
        )
        .map(|_completion| ())
}

/// Extracts the error value a handler observes from one escaping
/// exception (§7.3): the original thrown value, or a materialized
/// equivalent error object for engine-created errors.
fn exception_value(
    context: &mut Context<'_>,
    exception: &JsException,
) -> Result<JsValue, ExecutionError> {
    if let Some(value) = exception.thrown_value() {
        return Ok(value.clone());
    }
    let kind = exception.kind().unwrap_or(ExceptionKind::TypeError);
    let message = exception
        .message()
        .and_then(|message| message.to_utf8_lossy().ok())
        .unwrap_or_default();
    context.error(error_object_kind(kind), &message)
}

/// Maps one engine exception kind onto the error-object family.
fn error_object_kind(kind: ExceptionKind) -> ErrorObjectKind {
    match kind {
        ExceptionKind::InternalError => ErrorObjectKind::InternalError,
        ExceptionKind::RangeError => ErrorObjectKind::RangeError,
        ExceptionKind::ReferenceError => ErrorObjectKind::ReferenceError,
        ExceptionKind::SyntaxError => ErrorObjectKind::SyntaxError,
        ExceptionKind::TypeError => ErrorObjectKind::TypeError,
        ExceptionKind::UriError => ErrorObjectKind::UriError,
    }
}

/// Renders one value's `ToString` for the default diagnostic paths; never
/// fails, panics, or re-enters the engine on a conversion error. Objects
/// (Error instances among them) cannot use the host `ToString` (fail
/// closed), so an object renders as `<name>: <message>` when the shape is
/// there.
fn value_text(context: &mut Context<'_>, value: &JsValue) -> String {
    if let Ok(string) = value.to_string(context)
        && let Ok(text) = string.to_utf8_lossy()
    {
        return text;
    }
    if let Ok(object) = value.clone().into_object()
        && let Ok(name_key) = context.property_key("name")
        && let Ok(message_key) = context.property_key("message")
    {
        let name = object
            .get(context, name_key)
            .ok()
            .map(|name| value_text(context, &name))
            .unwrap_or_else(|| "Object".to_owned());
        let message = object
            .get(context, message_key)
            .ok()
            .map(|message| value_text(context, &message))
            .unwrap_or_default();
        return format!("{name}: {message}");
    }
    "<unrenderable>".to_owned()
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
