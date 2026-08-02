/*
 * JavaScript interrupt polling derived from QuickJS.
 *
 * Copyright (c) 2017-2018 Fabrice Bellard
 * Copyright (c) 2017-2018 Charlie Gordon
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! Host interrupt polling.
//!
//! A host can cancel a running script by installing an [`InterruptHandler`].
//! The interpreter polls it on a decrementing counter rather than on every
//! instruction, matching `js_poll_interrupts` and its
//! `JS_INTERRUPT_COUNTER_INIT` of 10,000 (`quickjs.c:512`, `quickjs.c:7877`).
//!
//! Fuel and interrupts answer different questions and are therefore separate.
//! Fuel is a deterministic, pre-committed work budget; an interrupt is a
//! decision the host makes while the script is already running, which is what
//! makes wall-clock deadlines and user cancellation expressible.
//!
//! An interrupt is not a catchable JavaScript exception. Upstream marks it
//! uncatchable (`JS_SetUncatchableException`, `quickjs.c:7861`) so a script
//! cannot swallow a host cancellation with `try`/`catch`; this port reports it
//! as a structured error that bypasses the JavaScript unwinder entirely, which
//! preserves that property by construction.

use std::{fmt, sync::Arc};

/// The interval between interrupt polls, in interpreter steps.
///
/// Polling on every step would make the handler's cost dominate execution, so
/// the counter reproduces upstream's `JS_INTERRUPT_COUNTER_INIT`
/// (`quickjs.c:512`). A handler therefore observes cancellation within this many
/// steps rather than immediately.
pub const INTERRUPT_POLL_INTERVAL: u32 = 10_000;

/// A host callback deciding whether execution should stop.
///
/// Returning `true` requests cancellation. The callback runs while the
/// interpreter is suspended at a step boundary, so it must not re-enter the
/// runtime; it exists to consult host state such as a deadline or a cancellation
/// flag.
pub trait InterruptHandler: Send + Sync {
    /// Returns whether the running script should be cancelled.
    fn should_interrupt(&self) -> bool;
}

impl<F> InterruptHandler for F
where
    F: Fn() -> bool + Send + Sync,
{
    fn should_interrupt(&self) -> bool {
        self()
    }
}

/// The interrupt state of one runtime.
#[derive(Clone, Default)]
pub(crate) struct InterruptState {
    handler: Option<Arc<dyn InterruptHandler>>,
}

impl fmt::Debug for InterruptState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InterruptState")
            .field("installed", &self.handler.is_some())
            .finish()
    }
}

impl InterruptState {
    /// Installs a handler, replacing any previous one.
    pub(crate) fn set_handler(&mut self, handler: Arc<dyn InterruptHandler>) {
        self.handler = Some(handler);
    }

    /// Removes the installed handler.
    pub(crate) fn clear_handler(&mut self) {
        self.handler = None;
    }

    /// Returns whether a handler is installed.
    pub(crate) fn is_installed(&self) -> bool {
        self.handler.is_some()
    }

    /// Asks the handler whether execution should stop.
    ///
    /// Answers `false` when no handler is installed, so an embedder that never
    /// installs one pays only the counter decrement.
    pub(crate) fn should_interrupt(&self) -> bool {
        self.handler
            .as_ref()
            .is_some_and(|handler| handler.should_interrupt())
    }
}

/// The decrementing poll counter for one execution session.
#[derive(Clone, Copy, Debug)]
pub(crate) struct InterruptCounter {
    remaining: u32,
}

impl Default for InterruptCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl InterruptCounter {
    /// Creates a counter at the start of its interval.
    pub(crate) const fn new() -> Self {
        Self {
            remaining: INTERRUPT_POLL_INTERVAL,
        }
    }

    /// Charges one interpreter step, reporting whether the handler is due.
    ///
    /// The counter resets on each poll, so the handler is consulted once per
    /// interval rather than once per session.
    pub(crate) fn charge_step(&mut self) -> bool {
        match self.remaining.checked_sub(1) {
            Some(remaining) if remaining > 0 => {
                self.remaining = remaining;
                false
            }
            _ => {
                self.remaining = INTERRUPT_POLL_INTERVAL;
                true
            }
        }
    }
}
