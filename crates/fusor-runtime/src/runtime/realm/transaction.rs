//! Allocation-free reverse journal for Realm construction.

use std::ops::{Deref, DerefMut};

use crate::Atom;

use super::{
    FunctionId, ObjectId, RealmId, Runtime, RuntimeError, RuntimeResource,
    allocation::AllocatedIntrinsics,
    allocation_failed,
    atoms::RealmAtomBindings,
    reservation::RealmReservationPlan,
    schema::{IntrinsicFunctionId, IntrinsicObjectId},
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
    pub(super) allocated: AllocatedIntrinsics,
    committed: bool,
}

impl<'runtime> RealmBuildTransaction<'runtime> {
    pub(super) fn try_new(
        runtime: &'runtime mut Runtime,
        reservation: RealmReservationPlan,
    ) -> Result<Self, RuntimeError> {
        let journal_entries = reservation.journal_entries();
        let mut journal = Vec::new();
        journal
            .try_reserve_exact(journal_entries)
            .map_err(|_| allocation_failed(RuntimeResource::ObjectProperties, journal_entries))?;
        let allocated =
            AllocatedIntrinsics::try_new(reservation.objects(), reservation.functions())?;
        Ok(Self {
            runtime,
            journal,
            allocated,
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

    pub(super) fn record_object(&mut self, id: IntrinsicObjectId, object: ObjectId) {
        self.allocated.insert_object(id, object);
        self.push(RealmUndo::Object(object));
    }

    pub(super) fn record_function(&mut self, id: IntrinsicFunctionId, function: FunctionId) {
        self.allocated.insert_function(id, function);
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
