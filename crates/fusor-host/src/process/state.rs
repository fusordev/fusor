//! Owner-task process state (§7): JS-side handler registrations that
//! signal deliveries consult, and the end-of-turn rejection queue. The
//! states live in the centralized op-state registry; the registered
//! handlers are engine values and therefore never cross to the signal
//! delivery thread.

use fusor_runtime::{JsValue, OwnedPromiseRejectionEvent};

/// The loop-owned process state.
#[derive(Debug, Default)]
pub(crate) struct ProcessState {
    /// The JS-side SIGINT handler registered through `Fusor.ops.op_process_on`
    /// (§7.1); `None` means the default interrupt/exit policy applies.
    pub sigint_handler: Option<JsValue>,
    /// The JS-side `uncaughtException` handler (§7.3); `None` means the
    /// default path (full stack render + exit 1).
    pub uncaught_handler: Option<JsValue>,
    /// The JS-side `unhandledRejection` handler (§7.3); `None` means the
    /// default path (warn + exit 1).
    pub unhandled_rejection_handler: Option<JsValue>,
}

/// Retained rejection-tracker notifications queued during execution,
/// drained and reconciled at the end of each turn (§7.3).
#[derive(Debug, Default)]
pub(crate) struct RejectionQueue(pub Vec<OwnedPromiseRejectionEvent>);

/// Installs a fresh process state for one [`crate::r#loop::HostLoop`]
/// into the op-state registry (owner-task bootstrap).
///
/// # Errors
///
/// Returns the state unchanged when one is already installed.
pub(crate) fn install_process_state(state: ProcessState) -> Result<(), ProcessState> {
    crate::ops::OpStateRegistry::install(state)
}

/// Removes the installed process state (shutdown teardown, §7.4),
/// dropping the registered handlers with it so a fresh loop can install
/// on the same thread.
#[must_use]
pub(crate) fn take_process_state() -> Option<ProcessState> {
    crate::ops::OpStateRegistry::take::<ProcessState>()
}

/// Borrows the installed process state mutably (the op entry points).
pub(crate) fn with_process_state<R>(
    operation: impl FnOnce(&mut ProcessState) -> R,
) -> Result<R, crate::ops::OpStateError> {
    crate::ops::OpStateRegistry::with_mut::<ProcessState, R>(operation)
}

/// Queues one retained rejection-tracker notification (the tracker
/// callback, which runs while JavaScript is suspended).
pub(crate) fn push_rejection_event(event: OwnedPromiseRejectionEvent) {
    if !crate::ops::OpStateRegistry::has::<RejectionQueue>() {
        return;
    }
    let _ = crate::ops::OpStateRegistry::with_mut::<RejectionQueue, _>(|queue| {
        queue.0.push(event);
    });
}

/// Takes every queued rejection notification for end-of-turn
/// reconciliation (§7.3).
pub(crate) fn take_rejection_events() -> Vec<OwnedPromiseRejectionEvent> {
    crate::ops::OpStateRegistry::with_mut::<RejectionQueue, _>(|queue| std::mem::take(&mut queue.0))
        .unwrap_or_default()
}

/// Returns whether rejection notifications are waiting for the
/// end-of-turn reconciliation (the loop's alive/work predicates).
pub(crate) fn has_pending_rejections() -> bool {
    crate::ops::OpStateRegistry::with::<RejectionQueue, _>(|queue| !queue.0.is_empty())
        .unwrap_or(false)
}
