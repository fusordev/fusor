//! Owner-task process state (§7): JS-side handler registrations that
//! signal deliveries consult. Like the timer state, one instance is
//! installed per [`crate::r#loop::HostLoop`] on the owner task; the
//! registered handlers are engine values and therefore never cross to the
//! signal delivery thread.

use std::cell::RefCell;

use fusor_runtime::JsValue;

/// The loop-owned process state.
#[derive(Debug, Default)]
pub(crate) struct ProcessState {
    /// The JS-side SIGINT handler registered through `process.on`
    /// (§7.1); `None` means the default interrupt/exit policy applies.
    pub sigint_handler: Option<JsValue>,
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
