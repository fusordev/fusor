/*
 * JavaScript value ownership derived from QuickJS.
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

use std::{
    cell::Cell,
    collections::TryReserveError,
    sync::{Arc, Weak},
};

use crate::{
    Atom, HandleError, HandleKind, JsBigInt, JsNumber, JsString, ValueKind,
    ids::{FunctionId, ObjectId},
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum HeapReference {
    Function(FunctionId),
    Object(ObjectId),
}

impl HeapReference {
    fn into_stored_value(self) -> StoredValue {
        match self {
            Self::Function(function) => StoredValue::Function(function),
            Self::Object(object) => StoredValue::Object(object),
        }
    }
}

#[derive(Debug)]
pub(crate) enum PrimitiveValue {
    Undefined,
    Null,
    Boolean(bool),
    Number(JsNumber),
    BigInt(Arc<JsBigInt>),
    String(JsString),
    Symbol(Atom),
}

impl PrimitiveValue {
    fn into_stored_value(self) -> StoredValue {
        match self {
            Self::Undefined => StoredValue::Undefined,
            Self::Null => StoredValue::Null,
            Self::Boolean(value) => StoredValue::Boolean(value),
            Self::Number(value) => StoredValue::Number(value),
            Self::BigInt(value) => StoredValue::BigInt(value),
            Self::String(value) => StoredValue::String(value),
            Self::Symbol(value) => StoredValue::Symbol(value),
        }
    }
}

pub(crate) enum RootTarget {
    Primitive(PrimitiveValue),
    Heap(HeapReference),
}

#[derive(Debug)]
pub(crate) enum StoredValue {
    Undefined,
    Null,
    Boolean(bool),
    Number(JsNumber),
    /// An ECMAScript `BigInt`.
    ///
    /// The payload is `Arc`-shared because a `BigInt` is an immutable
    /// arbitrary-width value: duplicating one must not copy its limbs.
    BigInt(Arc<JsBigInt>),
    String(JsString),
    Symbol(Atom),
    Function(FunctionId),
    Object(ObjectId),
}

impl StoredValue {
    pub(crate) fn into_root_target(self) -> RootTarget {
        match self {
            Self::Undefined => RootTarget::Primitive(PrimitiveValue::Undefined),
            Self::Null => RootTarget::Primitive(PrimitiveValue::Null),
            Self::Boolean(value) => RootTarget::Primitive(PrimitiveValue::Boolean(value)),
            Self::Number(value) => RootTarget::Primitive(PrimitiveValue::Number(value)),
            Self::BigInt(value) => RootTarget::Primitive(PrimitiveValue::BigInt(value)),
            Self::String(value) => RootTarget::Primitive(PrimitiveValue::String(value)),
            Self::Symbol(value) => RootTarget::Primitive(PrimitiveValue::Symbol(value)),
            Self::Function(function) => RootTarget::Heap(HeapReference::Function(function)),
            Self::Object(object) => RootTarget::Heap(HeapReference::Object(object)),
        }
    }

    pub(crate) const fn kind(&self) -> ValueKind {
        match self {
            Self::Undefined => ValueKind::Undefined,
            Self::Null => ValueKind::Null,
            Self::Boolean(_) => ValueKind::Boolean,
            Self::Number(_) => ValueKind::Number,
            Self::BigInt(_) => ValueKind::BigInt,
            Self::String(_) => ValueKind::String,
            Self::Symbol(_) => ValueKind::Symbol,
            Self::Function(_) => ValueKind::Function,
            Self::Object(_) => ValueKind::Object,
        }
    }

    pub(crate) fn duplicate(&self) -> Self {
        match self {
            Self::Undefined => Self::Undefined,
            Self::Null => Self::Null,
            Self::Boolean(value) => Self::Boolean(*value),
            Self::Number(value) => Self::Number(*value),
            Self::BigInt(value) => Self::BigInt(Arc::clone(value)),
            Self::String(value) => Self::String(value.clone()),
            Self::Symbol(value) => Self::Symbol(value.clone()),
            Self::Function(value) => Self::Function(*value),
            Self::Object(value) => Self::Object(*value),
        }
    }

    pub(crate) fn is_truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Boolean(value) => *value,
            Self::Number(value) => {
                let value = value.as_f64();
                value != 0.0 && !value.is_nan()
            }
            // `0n` is the only falsy `BigInt`; there is no negative zero and no
            // NaN in the domain.
            Self::BigInt(value) => !value.is_zero(),
            Self::String(value) => !value.is_empty(),
            Self::Symbol(_) | Self::Function(_) | Self::Object(_) => true,
        }
    }

    pub(crate) fn strict_equals(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Undefined, Self::Undefined) | (Self::Null, Self::Null) => true,
            (Self::Boolean(left), Self::Boolean(right)) => left == right,
            (Self::Number(left), Self::Number(right)) => left.strict_equals(*right),
            // A `BigInt` is never strictly equal to a Number, so `1n === 1` is
            // `false` while `1n === 1n` compares mathematical values.
            (Self::BigInt(left), Self::BigInt(right)) => left == right,
            (Self::String(left), Self::String(right)) => left == right,
            (Self::Symbol(left), Self::Symbol(right)) => left.is_same_identity(right),
            (Self::Function(left), Self::Function(right)) => left == right,
            (Self::Object(left), Self::Object(right)) => left == right,
            (
                Self::Undefined
                | Self::Null
                | Self::Boolean(_)
                | Self::Number(_)
                | Self::BigInt(_)
                | Self::String(_)
                | Self::Symbol(_)
                | Self::Function(_)
                | Self::Object(_),
                _,
            ) => false,
        }
    }

    /// Applies ECMAScript `SameValue`.
    ///
    /// This differs from strict equality only for Numbers: `NaN` equals
    /// itself, and the two signed zeros differ. `Object.defineProperty` uses
    /// it to decide whether redefining a non-configurable, non-writable data
    /// property is a no-op or a `TypeError`.
    pub(crate) fn same_value(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Number(left), Self::Number(right)) => left.same_value(*right),
            _ => self.strict_equals(other),
        }
    }

    /// Applies ECMAScript `SameValueZero`.
    ///
    /// This differs from [`Self::same_value`] only in treating `+0` and `-0` as
    /// equal, which is the comparison `Array.prototype.includes` uses:
    /// `[NaN].includes(NaN)` is `true` while `[NaN].indexOf(NaN)` is `-1`.
    pub(crate) fn same_value_zero(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Number(left), Self::Number(right)) => left.same_value_zero(*right),
            _ => self.strict_equals(other),
        }
    }

    pub(crate) const fn heap_reference(&self) -> Option<HeapReference> {
        match self {
            Self::Undefined
            | Self::Null
            | Self::Boolean(_)
            | Self::Number(_)
            | Self::BigInt(_)
            | Self::String(_)
            | Self::Symbol(_) => None,
            Self::Function(function) => Some(HeapReference::Function(*function)),
            Self::Object(object) => Some(HeapReference::Object(*object)),
        }
    }
}

pub(crate) enum SlotValue {
    Uninitialized,
    Value(StoredValue),
}

impl SlotValue {
    pub(crate) fn duplicate(&self) -> Self {
        match self {
            Self::Uninitialized => Self::Uninitialized,
            Self::Value(value) => Self::Value(value.duplicate()),
        }
    }
}

pub(crate) struct ReleaseMailbox {
    pending: Cell<Vec<HeapReference>>,
    outstanding: Cell<usize>,
}

impl ReleaseMailbox {
    pub(crate) const fn new() -> Self {
        Self {
            pending: Cell::new(Vec::new()),
            outstanding: Cell::new(0),
        }
    }

    pub(crate) fn try_reserve_root(&self) -> Result<(), TryReserveError> {
        let mut pending = self.pending.take();
        let outstanding = self.outstanding.get();
        let required_spare = outstanding.saturating_add(1);
        let spare = pending.capacity().saturating_sub(pending.len());
        let result = if spare < required_spare {
            pending.try_reserve(required_spare)
        } else {
            Ok(())
        };
        self.pending.set(pending);
        result?;
        self.outstanding.set(outstanding.saturating_add(1));
        Ok(())
    }

    pub(crate) fn cancel_reserved_root(&self) {
        let outstanding = self.outstanding.get();
        debug_assert!(outstanding > 0);
        self.outstanding.set(outstanding.saturating_sub(1));
    }

    fn queue_release(&self, reference: HeapReference) {
        let outstanding = self.outstanding.get();
        debug_assert!(outstanding > 0);
        let mut pending = self.pending.take();
        debug_assert!(pending.len() < pending.capacity());
        pending.push(reference);
        self.pending.set(pending);
        self.outstanding.set(outstanding.saturating_sub(1));
    }

    pub(crate) fn take_pending(&self) -> Vec<HeapReference> {
        self.pending.take()
    }

    pub(crate) fn restore_pending(&self, mut pending: Vec<HeapReference>) {
        let current = self.pending.take();
        debug_assert!(current.is_empty());
        pending.clear();
        self.pending.set(pending);
    }

    pub(crate) fn pending_len(&self) -> usize {
        let pending = self.pending.take();
        let len = pending.len();
        self.pending.set(pending);
        len
    }
}

struct ValueRoot {
    owner: Weak<ReleaseMailbox>,
    value: StoredValue,
    release: Option<HeapReference>,
}

impl Drop for ValueRoot {
    fn drop(&mut self) {
        if let Some(reference) = self.release
            && let Some(owner) = self.owner.upgrade()
        {
            owner.queue_release(reference);
        }
    }
}

/// A cloned public root for one runtime-local JavaScript value.
///
/// Clones share one immutable [`Arc`] root header. For a function value, the
/// final clone queues one allocation-free release back to the owning runtime;
/// primitive headers need no deferred heap release. Handles deliberately
/// remain `!Send + !Sync`; JavaScript heap ownership never crosses threads.
///
/// ```compile_fail
/// use quickjs_runtime::JsValue;
///
/// fn require_send<T: Send>() {}
/// require_send::<JsValue>();
/// ```
#[derive(Clone)]
pub struct JsValue(Arc<ValueRoot>);

impl JsValue {
    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "user-selected Arc root headers deliberately remain runtime-local through Cell"
    )]
    pub(crate) fn primitive(owner: &Arc<ReleaseMailbox>, value: PrimitiveValue) -> Self {
        Self(Arc::new(ValueRoot {
            owner: Arc::downgrade(owner),
            value: value.into_stored_value(),
            release: None,
        }))
    }

    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "user-selected Arc root headers deliberately remain runtime-local through Cell"
    )]
    pub(crate) fn rooted_heap(owner: &Arc<ReleaseMailbox>, reference: HeapReference) -> Self {
        Self(Arc::new(ValueRoot {
            owner: Arc::downgrade(owner),
            value: reference.into_stored_value(),
            release: Some(reference),
        }))
    }

    pub(crate) fn owner(&self) -> Result<Arc<ReleaseMailbox>, HandleError> {
        self.0.owner.upgrade().ok_or(HandleError::Orphaned {
            kind: HandleKind::Value,
        })
    }

    pub(crate) fn stored(&self) -> Result<&StoredValue, HandleError> {
        self.owner()?;
        Ok(&self.0.value)
    }

    /// Returns the observable value family.
    ///
    /// # Errors
    ///
    /// Returns [`HandleError::Orphaned`] after the owning runtime is dropped.
    pub fn kind(&self) -> Result<ValueKind, HandleError> {
        self.stored().map(StoredValue::kind)
    }

    /// Returns the Boolean payload, or `None` for another live value kind.
    ///
    /// # Errors
    ///
    /// Returns an error for an orphaned handle.
    pub fn as_boolean(&self) -> Result<Option<bool>, HandleError> {
        Ok(match self.stored()? {
            StoredValue::Boolean(value) => Some(*value),
            _ => None,
        })
    }

    /// Returns the Number payload, or `None` for another live value kind.
    ///
    /// # Errors
    ///
    /// Returns an error for an orphaned handle.
    pub fn as_number(&self) -> Result<Option<JsNumber>, HandleError> {
        Ok(match self.stored()? {
            StoredValue::Number(value) => Some(*value),
            _ => None,
        })
    }

    /// Returns the String payload, or `None` for another live value kind.
    ///
    /// # Errors
    ///
    /// Returns an error for an orphaned handle.
    pub fn as_string(&self) -> Result<Option<&JsString>, HandleError> {
        Ok(match self.stored()? {
            StoredValue::String(value) => Some(value),
            _ => None,
        })
    }

    /// Returns the Symbol identity, or `None` for another live value kind.
    ///
    /// # Errors
    ///
    /// Returns an error for an orphaned handle.
    pub fn as_symbol(&self) -> Result<Option<&Atom>, HandleError> {
        Ok(match self.stored()? {
            StoredValue::Symbol(value) => Some(value),
            _ => None,
        })
    }

    /// Converts a live function value into its typed embedding handle.
    ///
    /// # Errors
    ///
    /// Returns an orphaned-handle error or a value-kind mismatch.
    pub fn into_function(self) -> Result<Function, HandleError> {
        let actual = self.kind()?;
        if actual != ValueKind::Function {
            return Err(HandleError::WrongValueKind {
                expected: ValueKind::Function,
                actual,
            });
        }
        Ok(Function(self))
    }

    /// Converts a live ordinary object value into its typed embedding handle.
    ///
    /// # Errors
    ///
    /// Returns an orphaned-handle error or a value-kind mismatch.
    pub fn into_object(self) -> Result<Object, HandleError> {
        let actual = self.kind()?;
        if actual != ValueKind::Object {
            return Err(HandleError::WrongValueKind {
                expected: ValueKind::Object,
                actual,
            });
        }
        Ok(Object(self))
    }

    pub(crate) fn function_id(&self) -> Result<FunctionId, HandleError> {
        match self.stored()? {
            StoredValue::Function(function) => Ok(*function),
            other => Err(HandleError::WrongValueKind {
                expected: ValueKind::Function,
                actual: other.kind(),
            }),
        }
    }

    pub(crate) fn object_id(&self) -> Result<ObjectId, HandleError> {
        match self.stored()? {
            StoredValue::Object(object) => Ok(*object),
            other => Err(HandleError::WrongValueKind {
                expected: ValueKind::Object,
                actual: other.kind(),
            }),
        }
    }
}

impl std::fmt::Debug for JsValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JsValue")
            .field("kind", &self.kind())
            .finish_non_exhaustive()
    }
}

/// A rooted runtime-local ordinary bytecode function.
///
/// The wrapper is an immutable [`Arc`]-backed public root, not the function
/// heap node itself. Runtime objects and captured cells remain generational
/// nodes owned by one `Runtime`.
///
/// ```compile_fail
/// use quickjs_runtime::Function;
///
/// fn require_sync<T: Sync>() {}
/// require_sync::<Function>();
/// ```
#[derive(Clone, Debug)]
pub struct Function(JsValue);

impl Function {
    pub(crate) const fn from_root(value: JsValue) -> Self {
        Self(value)
    }

    /// Returns this function as an arbitrary value root.
    #[must_use]
    pub fn as_value(&self) -> JsValue {
        self.0.clone()
    }

    pub(crate) fn owner(&self) -> Result<Arc<ReleaseMailbox>, HandleError> {
        self.0.owner().map_err(|error| match error {
            HandleError::Orphaned { .. } => HandleError::Orphaned {
                kind: HandleKind::Function,
            },
            other => other,
        })
    }

    pub(crate) fn id(&self) -> Result<FunctionId, HandleError> {
        self.0.function_id()
    }

    /// Tests runtime-local object identity.
    ///
    /// # Errors
    ///
    /// Returns an error if either function is orphaned or belongs to a
    /// different runtime.
    pub fn same_identity(&self, other: &Self) -> Result<bool, HandleError> {
        let owner = self.owner()?;
        let other_owner = other.owner()?;
        if !Arc::ptr_eq(&owner, &other_owner) {
            return Err(HandleError::ForeignRuntime {
                kind: HandleKind::Function,
            });
        }
        Ok(self.id()? == other.id()?)
    }
}

/// A rooted runtime-local ordinary JavaScript object.
///
/// Clones share one [`Arc`]-backed logical public root. The object heap node
/// remains uniquely owned by its runtime and the handle is `!Send + !Sync`.
///
/// ```compile_fail
/// use quickjs_runtime::Object;
///
/// fn require_send<T: Send>() {}
/// require_send::<Object>();
/// ```
#[derive(Clone, Debug)]
pub struct Object(JsValue);

impl Object {
    /// Returns this object as an arbitrary value root.
    #[must_use]
    pub fn as_value(&self) -> JsValue {
        self.0.clone()
    }

    fn owner(&self) -> Result<Arc<ReleaseMailbox>, HandleError> {
        self.0.owner().map_err(|error| match error {
            HandleError::Orphaned { .. } => HandleError::Orphaned {
                kind: HandleKind::Object,
            },
            other => other,
        })
    }

    fn id(&self) -> Result<ObjectId, HandleError> {
        self.0.object_id()
    }

    /// Tests runtime-local object identity.
    ///
    /// # Errors
    ///
    /// Returns an error if either object is orphaned or belongs to a different
    /// runtime.
    pub fn same_identity(&self, other: &Self) -> Result<bool, HandleError> {
        let owner = self.owner()?;
        let other_owner = other.owner()?;
        if !Arc::ptr_eq(&owner, &other_owner) {
            return Err(HandleError::ForeignRuntime {
                kind: HandleKind::Object,
            });
        }
        Ok(self.id()? == other.id()?)
    }
}
