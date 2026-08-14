//! Timer and `setImmediate` ops (§6.4) registered by the core overlay
//! (§9) onto `Fusor.ops`.
//!
//! Call signatures follow the Node/browser convention:
//! `setTimeout(callback, delay)` / `setInterval(callback, delay)`. Delays
//! follow the §6.4 semantics: milliseconds truncated toward zero, negative
//! values clamped to 0. Callbacks fire with the job-callback semantics on
//! the owner task; same-deadline timers fire in creation order.

use std::time::Duration;

use fusor_ops::op;
use fusor_runtime::{Context, JsValue};

use super::OpError;
use crate::r#loop::{TimerCallback, with_timer_state};

fn normalized_delay(delay: f64) -> Duration {
    Duration::from_millis(delay.trunc().max(0.0) as u64)
}

#[op]
pub(crate) fn op_set_timeout(callback: JsValue, delay: f64) -> Result<f64, OpError> {
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

#[op]
pub(crate) fn op_set_interval(callback: JsValue, delay: f64) -> Result<f64, OpError> {
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

#[op]
pub(crate) fn op_clear_timeout(id: f64) -> Result<(), OpError> {
    with_timer_state(|state| state.cancel(crate::r#loop::TimerId::from_u32(id as u32)))
        .map_err(|error| OpError::new(error.to_string()))?;
    Ok(())
}

#[op]
pub(crate) fn op_clear_interval(id: f64) -> Result<(), OpError> {
    with_timer_state(|state| state.cancel(crate::r#loop::TimerId::from_u32(id as u32)))
        .map_err(|error| OpError::new(error.to_string()))?;
    Ok(())
}

/// Queues one host callback into the engine's promise-job queue
/// (ECMA-262 `HostEnqueuePromiseJob`): it runs at the next microtask
/// checkpoint in FIFO order with Promise reactions (§6.2), with the
/// `undefined` receiver and no arguments.
#[op]
pub(crate) fn op_queue_microtask(
    context: &mut Context<'_>,
    callback: JsValue,
) -> Result<(), OpError> {
    let _function = callback
        .clone()
        .into_function()
        .map_err(|_| OpError::type_error(0, "expected a function"))?;
    context
        .enqueue_host_job(callback)
        .map_err(|error| OpError::new(error.to_string()))?;
    Ok(())
}

#[op]
pub(crate) fn op_set_immediate(callback: JsValue) -> Result<f64, OpError> {
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
