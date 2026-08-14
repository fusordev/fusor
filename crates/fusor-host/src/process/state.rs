//! Owner-task process state (§7): JS-side handler registrations that
//! signal deliveries consult. Like the timer state, one instance is
//! installed per [`crate::r#loop::HostLoop`] on the owner task; the
//! registered handlers are engine values and therefore never cross to the
//! signal delivery thread.

use std::cell::RefCell;

use fusor_runtime::{JsValue, OwnedPromiseRejectionEvent};

/// The loop-owned process state.
#[derive(Debug, Default)]
pub(crate) struct ProcessState {
    /// The JS-side SIGINT handler registered through `process.on`
    /// (§7.1); `None` means the default interrupt/exit policy applies.
    pub sigint_handler: Option<JsValue>,
    /// The JS-side `uncaughtException` handler (§7.3); `None` means the
    /// default path (full stack render + exit 1).
    pub uncaught_handler: Option<JsValue>,
    /// The JS-side `unhandledRejection` handler (§7.3); `None` means the
    /// default path (warn + exit 1).
    pub unhandled_rejection_handler: Option<JsValue>,
}

thread_local! {
    static PROCESS_STATE: RefCell<Option<ProcessState>> = const { RefCell::new(None) };
}

/// Installs a fresh process state for one [`crate::r#loop::HostLoop`]
/// (owner-task bootstrap).
///
/// # Errors
///
/// Returns the state unchanged when one is already installed.
pub(crate) fn install_process_state(state: ProcessState) -> Result<(), ProcessState> {
    PROCESS_STATE.with(|slot| {
        if slot.borrow().is_some() {
            return Err(state);
        }
        *slot.borrow_mut() = Some(state);
        Ok(())
    })
}

/// Borrows the installed process state mutably (the op entry points).
pub(crate) fn with_process_state<R>(
    operation: impl FnOnce(&mut ProcessState) -> R,
) -> Result<R, ProcessStateError> {
    PROCESS_STATE.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .map(operation)
            .ok_or(ProcessStateError::NotInstalled)
    })
}

/// Process-state failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProcessStateError {
    /// No process state is installed on the owner task.
    NotInstalled,
}

impl std::fmt::Display for ProcessStateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => formatter.write_str(
                "no process state is installed (create the HostLoop first)",
            ),
        }
    }
}

impl std::error::Error for ProcessStateError {}

thread_local! {
    /// Retained rejection-tracker notifications queued during execution,
    /// drained and reconciled at the end of each turn (§7.3).
    static REJECTION_QUEUE: RefCell<Vec<OwnedPromiseRejectionEvent>> =
        const { RefCell::new(Vec::new()) };
}

/// Queues one retained rejection-tracker notification (the tracker
/// callback, which runs while JavaScript is suspended).
pub(crate) fn push_rejection_event(event: OwnedPromiseRejectionEvent) {
    REJECTION_QUEUE.with(|slot| slot.borrow_mut().push(event));
}

/// Takes every queued rejection notification for end-of-turn
/// reconciliation (§7.3).
pub(crate) fn take_rejection_events() -> Vec<OwnedPromiseRejectionEvent> {
    REJECTION_QUEUE.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

/// Returns whether rejection notifications are waiting for the
/// end-of-turn reconciliation (the loop's alive/work predicates).
pub(crate) fn has_pending_rejections() -> bool {
    REJECTION_QUEUE.with(|slot| !slot.borrow().is_empty())
}
