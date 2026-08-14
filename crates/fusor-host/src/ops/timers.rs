//! Timer and `setImmediate` ops (§6.4) installed on `Fusor.ops`.
//!
//! Call signatures follow the Node/browser convention:
//! `setTimeout(callback, delay)` / `setInterval(callback, delay)`. Delays
//! follow the §6.4 semantics: milliseconds truncated toward zero, negative
//! values clamped to 0. Callbacks fire with the job-callback semantics on
//! the owner task; same-deadline timers fire in creation order.

use std::time::Duration;

use fusor_ops::op;
use fusor_runtime::{Context, ExecutionError, JsValue};

use super::{OpError, install_op};
use crate::r#loop::{TimerCallback, with_timer_state};

fn normalized_delay(delay: f64) -> Duration {
    Duration::from_millis(delay.trunc().max(0.0) as u64)
}

#[op(name = "setTimeout")]
fn op_set_timeout(callback: JsValue, delay: f64) -> Result<f64, OpError> {
    let normalized = normalized_delay(delay);
    let id = with_timer_state(|state| {
        state
            .push(
                TimerCallback {
                    callback,
                    repeating: false,
                    delay: normalized,
                },
                state.now + normalized,
            )
            .as_u32()
    })
    .map_err(|error| OpError::new(error.to_string()))?;
    Ok(f64::from(id))
}

#[op(name = "setInterval")]
fn op_set_interval(callback: JsValue, delay: f64) -> Result<f64, OpError> {
    let normalized = normalized_delay(delay);
    let id = with_timer_state(|state| {
        state
            .push(
                TimerCallback {
                    callback,
                    repeating: true,
                    delay: normalized,
                },
                state.now + normalized,
            )
            .as_u32()
    })
    .map_err(|error| OpError::new(error.to_string()))?;
    Ok(f64::from(id))
}

#[op(name = "clearTimeout")]
fn op_clear_timeout(id: f64) -> Result<(), OpError> {
    with_timer_state(|state| state.cancel(crate::r#loop::TimerId::from_u32(id as u32)))
        .map_err(|error| OpError::new(error.to_string()))?;
    Ok(())
}

#[op(name = "clearInterval")]
fn op_clear_interval(id: f64) -> Result<(), OpError> {
    with_timer_state(|state| state.cancel(crate::r#loop::TimerId::from_u32(id as u32)))
        .map_err(|error| OpError::new(error.to_string()))?;
    Ok(())
}

#[op(name = "setImmediate")]
fn op_set_immediate(callback: JsValue) -> Result<f64, OpError> {
    let id = with_timer_state(|state| {
        state
            .push_immediate(TimerCallback {
                callback,
                repeating: false,
                delay: Duration::ZERO,
            })
            .as_u32()
    })
    .map_err(|error| OpError::new(error.to_string()))?;
    Ok(f64::from(id))
}

/// Installs the five timer ops as `Fusor.ops.setTimeout` etc. (§5.4 host
/// global conventions).
///
/// # Errors
///
/// Returns an [`ExecutionError`] when an op cannot be installed.
pub fn install_timers(context: &mut Context<'_>) -> Result<(), ExecutionError> {
    install_op(
        context,
        __fusor_op_declaration_op_set_timeout(),
        __fusor_op_call_op_set_timeout,
    )?;
    install_op(
        context,
        __fusor_op_declaration_op_set_interval(),
        __fusor_op_call_op_set_interval,
    )?;
    install_op(
        context,
        __fusor_op_declaration_op_clear_timeout(),
        __fusor_op_call_op_clear_timeout,
    )?;
    install_op(
        context,
        __fusor_op_declaration_op_clear_interval(),
        __fusor_op_call_op_clear_interval,
    )?;
    install_op(
        context,
        __fusor_op_declaration_op_set_immediate(),
        __fusor_op_call_op_set_immediate,
    )
}
