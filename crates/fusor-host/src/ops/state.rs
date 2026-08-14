//! The centralized op-state registry (§5.6, §6): one owner-task
//! singleton table keyed by [`std::any::TypeId`], replacing per-module
//! thread_locals. Every host state — the resource table, the async-op
//! runtime, timer and process state, signal state, the rejection queue,
//! the print sink — installs as its type's single slot.
//!
//! The registry is deliberately thread-local (the single-owner rule:
//! engine state never crosses tasks) and allocation-typed; a missing
//! slot fails closed with a typed [`OpStateError`] naming the state
//! type.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

thread_local! {
    static SLOTS: RefCell<HashMap<TypeId, Box<dyn Any>>> = RefCell::new(HashMap::new());
}

/// The owner-task op-state registry.
///
/// This type is never instantiated; every operation is an associated
/// function over the thread-local slot table.
pub struct OpStateRegistry;

impl OpStateRegistry {
    /// Installs one singleton state under its type (§5.6).
    ///
    /// # Errors
    ///
    /// Returns the state unchanged when its type's slot is already
    /// taken.
    pub fn install<T: Any>(state: T) -> Result<(), T> {
        SLOTS.with(|slots| {
            let mut slots = slots.borrow_mut();
            if slots.contains_key(&TypeId::of::<T>()) {
                return Err(state);
            }
            slots.insert(TypeId::of::<T>(), Box::new(state));
            Ok(())
        })
    }

    /// Borrows one singleton state immutably.
    ///
    /// # Errors
    ///
    /// Returns [`OpStateError::NotInstalled`] when the type's slot is
    /// empty.
    pub fn with<T: Any, R>(operation: impl FnOnce(&T) -> R) -> Result<R, OpStateError> {
        SLOTS.with(|slots| {
            slots
                .borrow()
                .get(&TypeId::of::<T>())
                .and_then(|state| state.downcast_ref::<T>())
                .map(operation)
                .ok_or_else(OpStateError::not_installed::<T>)
        })
    }

    /// Borrows one singleton state mutably.
    ///
    /// # Errors
    ///
    /// Returns [`OpStateError::NotInstalled`] when the type's slot is
    /// empty.
    pub fn with_mut<T: Any, R>(operation: impl FnOnce(&mut T) -> R) -> Result<R, OpStateError> {
        SLOTS.with(|slots| {
            slots
                .borrow_mut()
                .get_mut(&TypeId::of::<T>())
                .and_then(|state| state.downcast_mut::<T>())
                .map(operation)
                .ok_or_else(OpStateError::not_installed::<T>)
        })
    }

    /// Removes one singleton state, returning it (shutdown teardown,
    /// §7.4).
    #[must_use]
    pub fn take<T: Any>() -> Option<T> {
        SLOTS.with(|slots| {
            slots
                .borrow_mut()
                .remove(&TypeId::of::<T>())
                .and_then(|state| state.downcast::<T>().ok())
                .map(|state| *state)
        })
    }

    /// Returns whether the type's slot is installed.
    #[must_use]
    pub fn has<T: Any>() -> bool {
        SLOTS.with(|slots| slots.borrow().contains_key(&TypeId::of::<T>()))
    }
}

/// Op-state registry failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpStateError {
    /// The type's slot is empty (install the state first).
    NotInstalled {
        /// The state type's name, for diagnostics.
        type_name: &'static str,
    },
}

impl OpStateError {
    /// Builds the not-installed error for one state type.
    #[must_use]
    pub fn not_installed<T: Any>() -> Self {
        Self::NotInstalled {
            type_name: std::any::type_name::<T>(),
        }
    }
}

impl fmt::Display for OpStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInstalled { type_name } => {
                write!(formatter, "op state '{type_name}' is not installed")
            }
        }
    }
}

impl std::error::Error for OpStateError {}
