//! Process ops (§7.1, §7.2) installed on `Fusor.ops` (§5.4). The dedicated
//! `Fusor.process` object surface is removed: every op lives under
//! `Fusor.ops` like all other ops.

use fusor_ops::op;
use fusor_runtime::{Context, ExecutionError, JsValue};

use super::{OpError, install_op};
use crate::process::{with_process_state, with_signal_state};

#[op]
fn op_process_on(event: String, handler: JsValue) -> Result<(), OpError> {
    let _function = handler
        .clone()
        .into_function()
        .map_err(|_| OpError::type_error(1, "expected a function"))?;
    match event.as_str() {
        "SIGINT" => {
            with_process_state(|state| {
                state.sigint_handler = Some(handler);
            })
            .map_err(|error| OpError::new(error.to_string()))?;
            with_signal_state(|state| {
                state.set_js_sigint_handler(true);
                // A handler replaces the default policy (§7.1): a pending
                // interrupt request from before the registration must not
                // abort the next script.
                state.consume_interrupt();
            })
            .map_err(|error| OpError::new(error.to_string()))?;
        }
        "uncaughtException" => {
            with_process_state(|state| {
                state.uncaught_handler = Some(handler);
            })
            .map_err(|error| OpError::new(error.to_string()))?;
        }
        "unhandledRejection" => {
            with_process_state(|state| {
                state.unhandled_rejection_handler = Some(handler);
            })
            .map_err(|error| OpError::new(error.to_string()))?;
        }
        _ => {
            return Err(OpError::of_class(
                "RangeError",
                format!(
                    "unsupported process event '{event}' (the alpha host supports \
                     'SIGINT', 'uncaughtException', and 'unhandledRejection')"
                ),
            ));
        }
    }
    Ok(())
}

/// Truncates the JavaScript-supplied exit code to 8 bits (Node
/// semantics: `op_process_exit(256)` exits 0, `op_process_exit(-1)` exits
/// 255); non-finite values resolve to 0.
fn resolve_exit_code(code: f64) -> i32 {
    (code as i32) & 0xFF
}

#[op]
fn op_process_exit(code: f64) -> Result<(), OpError> {
    with_signal_state(|state| {
        state.request_exit(resolve_exit_code(code));
    })
    .map_err(|error| OpError::new(error.to_string()))?;
    Ok(())
}

/// Installs the process ops as `Fusor.ops.op_process_on` (§7.1) and
/// `Fusor.ops.op_process_exit` (§7.2). The assembly builder
/// (`HostRuntime::builder()`, §9) performs this installation as a fixed
/// host-core step; embedders never call it directly.
///
/// `Fusor.ops.op_process_exit(code)` truncates the code to 8 bits and
/// requests the exit; it does not wait for pending async ops, and the
/// exit takes effect at the next turn boundary (§7.2, documented).
///
/// # Errors
///
/// Returns an [`ExecutionError`] when an op cannot be installed.
pub(crate) fn install_process(context: &mut Context<'_>) -> Result<(), ExecutionError> {
    install_op(context, op_process_on::declaration(), op_process_on::call)?;
    install_op(
        context,
        op_process_exit::declaration(),
        op_process_exit::call,
    )
}
