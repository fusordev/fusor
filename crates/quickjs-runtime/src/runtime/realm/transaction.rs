//! Allocation-free reverse journal for Realm construction.

use std::ops::{Deref, DerefMut};

use crate::Atom;

use super::{
    FunctionId, ObjectId, RealmId, Runtime, RuntimeError, RuntimeResource, allocation_failed,
    atoms::RealmAtomBindings,
};

enum RealmUndo {
    Atom(Atom),
    Realm(RealmId),
    Object(ObjectId),
    Function(FunctionId),
}

/// Exclusive, pre-reserved Realm build transaction.
///
/// Every journal push is infallible after construction.  Dropping an
/// uncommitted transaction removes mutations in strict reverse order and never
/// invokes JavaScript, GC, weak callbacks, or finalizers.
pub(super) struct RealmBuildTransaction<'runtime> {
    runtime: &'runtime mut Runtime,
    journal: Vec<RealmUndo>,
    committed: bool,
}

impl<'runtime> RealmBuildTransaction<'runtime> {
    pub(super) fn try_new(
        runtime: &'runtime mut Runtime,
        journal_entries: usize,
    ) -> Result<Self, RuntimeError> {
        let mut journal = Vec::new();
        journal
            .try_reserve_exact(journal_entries)
            .map_err(|_| allocation_failed(RuntimeResource::ObjectProperties, journal_entries))?;
        Ok(Self {
            runtime,
            journal,
            committed: false,
        })
    }

    pub(super) fn record_atoms(&mut self, atoms: &RealmAtomBindings) {
        for atom in atoms.atoms() {
            self.push(RealmUndo::Atom(atom.clone()));
        }
    }

    pub(super) fn record_realm(&mut self, realm: RealmId) {
        self.push(RealmUndo::Realm(realm));
    }

    pub(super) fn record_object(&mut self, object: ObjectId) {
        self.push(RealmUndo::Object(object));
    }

    pub(super) fn record_function(&mut self, function: FunctionId) {
        self.push(RealmUndo::Function(function));
    }

    pub(super) const fn commit(&mut self) {
        self.committed = true;
    }

    fn push(&mut self, undo: RealmUndo) {
        debug_assert!(self.journal.len() < self.journal.capacity());
        self.journal.push(undo);
    }

    fn rollback(&mut self) {
        while let Some(undo) = self.journal.pop() {
            match undo {
                RealmUndo::Atom(atom) => self.runtime.atoms.rollback_interned_string(atom),
                RealmUndo::Realm(realm) => {
                    debug_assert!(self.runtime.realms.remove(realm).is_some());
                }
                RealmUndo::Object(object) => {
                    debug_assert!(self.runtime.objects.remove(object).is_some());
                }
                RealmUndo::Function(function) => {
                    debug_assert!(self.runtime.functions.remove(function).is_some());
                }
            }
        }
    }
}

impl Deref for RealmBuildTransaction<'_> {
    type Target = Runtime;

    fn deref(&self) -> &Self::Target {
        self.runtime
    }
}

impl DerefMut for RealmBuildTransaction<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.runtime
    }
}

impl Drop for RealmBuildTransaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.rollback();
        }
    }
}
