/*
 * JavaScript Promise rejection tracking derived from QuickJS.
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

//! Runtime-local host notifications for rejected Promises.

use std::{fmt, rc::Rc, sync::Arc};

use crate::{
    Atom, ExecutionError, JsBigInt, JsNumber, JsString, JsValue, Object, Runtime, ValueKind,
    ids::ObjectId, value::StoredValue,
};

/// The host-observable transition reported for a rejected Promise.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromiseRejectionOperation {
    /// A Promise became rejected before any reaction was attached.
    Reject,
    /// The previously reported rejection gained its first reaction.
    Handle,
}

impl PromiseRejectionOperation {
    /// Returns the `is_handled` flag used by `QuickJS`'s C embedding API.
    #[must_use]
    pub const fn is_handled(self) -> bool {
        matches!(self, Self::Handle)
    }
}

/// A borrowed rejection reason valid for one tracker callback.
///
/// Heap identity can be retained through [`PromiseRejectionEvent::retain`].
#[derive(Clone, Copy)]
pub struct PromiseRejectionValue<'event>(&'event StoredValue);

impl<'event> PromiseRejectionValue<'event> {
    /// Returns the observable value family.
    #[must_use]
    pub const fn kind(self) -> ValueKind {
        self.0.kind()
    }

    /// Returns the Boolean payload, or `None` for another value kind.
    #[must_use]
    pub const fn as_boolean(self) -> Option<bool> {
        match self.0 {
            StoredValue::Boolean(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the Number payload, or `None` for another value kind.
    #[must_use]
    pub const fn as_number(self) -> Option<JsNumber> {
        match self.0 {
            StoredValue::Number(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the `BigInt` payload, or `None` for another value kind.
    #[must_use]
    pub fn as_bigint(self) -> Option<&'event JsBigInt> {
        match self.0 {
            StoredValue::BigInt(value) => Some(Arc::as_ref(value)),
            _ => None,
        }
    }

    /// Returns the String payload, or `None` for another value kind.
    #[must_use]
    pub const fn as_string(self) -> Option<&'event JsString> {
        match self.0 {
            StoredValue::String(value) => Some(value),
            _ => None,
        }
    }

    /// Returns the Symbol identity, or `None` for another value kind.
    #[must_use]
    pub const fn as_symbol(self) -> Option<&'event Atom> {
        match self.0 {
            StoredValue::Symbol(value) => Some(value),
            _ => None,
        }
    }
}

impl fmt::Debug for PromiseRejectionValue<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PromiseRejectionValue")
            .field("kind", &self.kind())
            .finish_non_exhaustive()
    }
}

/// One synchronous, borrowed host rejection-tracker notification.
///
/// `QuickJS` exposes `JSValueConst` values to this hook. This safe equivalent
/// keeps the same non-owning default: inspect the reason during the callback,
/// or call [`Self::retain`] to explicitly create owned public roots. The event
/// cannot escape the callback and exposes no runtime re-entry path.
pub struct PromiseRejectionEvent<'runtime> {
    operation: PromiseRejectionOperation,
    promise: ObjectId,
    reason: StoredValue,
    runtime: &'runtime mut Runtime,
}

impl fmt::Debug for PromiseRejectionEvent<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PromiseRejectionEvent")
            .field("operation", &self.operation)
            .field("reason", &self.reason())
            .finish_non_exhaustive()
    }
}

impl PromiseRejectionEvent<'_> {
    /// Returns whether this reports an unhandled rejection or its first handler.
    #[must_use]
    pub const fn operation(&self) -> PromiseRejectionOperation {
        self.operation
    }

    /// Returns a borrowed view of the rejection reason.
    #[must_use]
    pub const fn reason(&self) -> PromiseRejectionValue<'_> {
        PromiseRejectionValue(&self.reason)
    }

    /// Explicitly roots the Promise and reason for use after this callback.
    ///
    /// # Errors
    ///
    /// Returns a public-root resource or allocation failure. Failure is local
    /// to the host callback and does not alter the JavaScript rejection.
    pub fn retain(&mut self) -> Result<OwnedPromiseRejectionEvent, ExecutionError> {
        let (promise, reason) = self
            .runtime
            .public_value_pair(StoredValue::Object(self.promise), self.reason.duplicate())?;
        Ok(OwnedPromiseRejectionEvent {
            operation: self.operation,
            promise: Object::from_root(promise),
            reason,
        })
    }
}

/// An explicitly retained rejection-tracker notification.
///
/// The Promise and reason are rooted public handles. Dropping the last clone
/// queues their roots for release at the next mutable runtime boundary.
#[derive(Clone, Debug)]
pub struct OwnedPromiseRejectionEvent {
    operation: PromiseRejectionOperation,
    promise: Object,
    reason: JsValue,
}

impl OwnedPromiseRejectionEvent {
    /// Returns whether this reports an unhandled rejection or its first handler.
    #[must_use]
    pub const fn operation(&self) -> PromiseRejectionOperation {
        self.operation
    }

    /// Returns the rejected Promise.
    #[must_use]
    pub const fn promise(&self) -> &Object {
        &self.promise
    }

    /// Returns the rejection reason.
    #[must_use]
    pub const fn reason(&self) -> &JsValue {
        &self.reason
    }

    /// Splits this notification into its operation, Promise, and reason.
    #[must_use]
    pub fn into_parts(self) -> (PromiseRejectionOperation, Object, JsValue) {
        (self.operation, self.promise, self.reason)
    }
}

/// A synchronous host observer for Promise rejection transitions.
///
/// The callback runs while JavaScript execution is suspended. It must not
/// re-enter the runtime. The runtime is local to one thread, so the tracker and
/// its events intentionally do not require `Send` or `Sync`.
pub trait PromiseRejectionTracker {
    /// Observes one `HostPromiseRejectionTracker` operation.
    fn promise_rejection(&self, event: PromiseRejectionEvent<'_>);
}

impl<F> PromiseRejectionTracker for F
where
    F: for<'event> Fn(PromiseRejectionEvent<'event>),
{
    fn promise_rejection(&self, event: PromiseRejectionEvent<'_>) {
        self(event);
    }
}

/// The installed tracker for one runtime.
#[derive(Clone, Default)]
pub(crate) struct PromiseRejectionState {
    tracker: Option<Rc<dyn PromiseRejectionTracker>>,
}

impl fmt::Debug for PromiseRejectionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PromiseRejectionState")
            .field("installed", &self.tracker.is_some())
            .finish()
    }
}

impl PromiseRejectionState {
    fn set_tracker(&mut self, tracker: Rc<dyn PromiseRejectionTracker>) {
        self.tracker = Some(tracker);
    }

    fn clear_tracker(&mut self) {
        self.tracker = None;
    }

    const fn is_installed(&self) -> bool {
        self.tracker.is_some()
    }
}

impl Runtime {
    /// Installs the host Promise rejection tracker, replacing any previous one.
    pub fn set_promise_rejection_tracker(&mut self, tracker: Rc<dyn PromiseRejectionTracker>) {
        self.promise_rejections.set_tracker(tracker);
    }

    /// Removes the installed host Promise rejection tracker.
    pub fn clear_promise_rejection_tracker(&mut self) {
        self.promise_rejections.clear_tracker();
    }

    /// Returns whether a host Promise rejection tracker is installed.
    #[must_use]
    pub fn has_promise_rejection_tracker(&self) -> bool {
        self.promise_rejections.is_installed()
    }

    pub(crate) fn dispatch_promise_rejection(
        &mut self,
        operation: PromiseRejectionOperation,
        promise: ObjectId,
        reason: StoredValue,
    ) {
        let Some(tracker) = self.promise_rejections.tracker.clone() else {
            return;
        };
        tracker.promise_rejection(PromiseRejectionEvent {
            operation,
            promise,
            reason,
            runtime: self,
        });
    }
}
