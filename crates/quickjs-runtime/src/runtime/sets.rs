//! Set allocation, intrinsic lookup, and resource-accounted storage updates.

use super::{
    HeapObject, HeapReference, JsNumber, ObjectId, ObjectRecord, RealmId, RealmIntrinsics, Runtime,
    RuntimeResource, StoredValue, check_execution_limit, stale_heap_reference, usize_to_u64,
};
use crate::object::{MapSetOutcome, SetIterator, SetIteratorKind, SetState};

impl Runtime {
    pub(crate) fn realm_set_prototype(
        &self,
        realm: RealmId,
    ) -> Result<ObjectId, crate::EngineFault> {
        let state = self
            .realms
            .get(realm)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })?;
        let RealmIntrinsics::Ready { set, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Set intrinsics are not initialized",
            });
        };
        Ok(set.prototype)
    }

    pub(crate) fn realm_set_iterator_prototype(
        &self,
        realm: RealmId,
    ) -> Result<ObjectId, crate::EngineFault> {
        let state = self
            .realms
            .get(realm)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })?;
        let RealmIntrinsics::Ready { set, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Set intrinsics are not initialized",
            });
        };
        Ok(set.iterator_prototype)
    }

    pub(crate) fn allocate_set_object(
        &mut self,
        prototype: HeapReference,
    ) -> Result<ObjectId, crate::ExecutionError> {
        self.allocate_set_object_with_state(prototype, SetState::empty())
    }

    pub(crate) fn allocate_set_object_with_state(
        &mut self,
        prototype: HeapReference,
        state: SetState,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
        }
        let additional_entries = usize_to_u64(state.retained_len());
        check_execution_limit(
            RuntimeResource::CollectionEntries,
            self.limits.max_collection_entries,
            self.collection_entries.saturating_add(additional_entries),
        )?;
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let object = self
            .insert_heap_object(HeapObject::set(ObjectRecord::empty(Some(prototype)), state))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_entries = self.collection_entries.saturating_add(additional_entries);
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn allocate_set_iterator(
        &mut self,
        realm: RealmId,
        set: ObjectId,
        kind: SetIteratorKind,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_set_iterator_prototype(realm)?;
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let object = self
            .insert_heap_object(HeapObject::set_iterator(
                ObjectRecord::empty(Some(HeapReference::Object(prototype))),
                SetIterator::new(set, kind),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn set_add(
        &mut self,
        set: ObjectId,
        value: StoredValue,
    ) -> Result<(), crate::ExecutionError> {
        let value = canonicalize_set_value(value);
        let inserts = self
            .objects
            .get(set)
            .and_then(HeapObject::set_state)
            .is_some_and(|state| !state.contains(&value));
        if inserts {
            check_execution_limit(
                RuntimeResource::CollectionEntries,
                self.limits.max_collection_entries,
                self.collection_entries.saturating_add(1),
            )?;
        }
        let state = self
            .objects
            .get_mut(set)
            .and_then(HeapObject::set_state_mut)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Set object",
                index: set.index(),
                generation: set.generation(),
            })?;
        let outcome =
            state
                .try_add(value)
                .map_err(|_| crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::CollectionEntries,
                    additional: 1,
                })?;
        if matches!(outcome, MapSetOutcome::Inserted) {
            self.collection_entries = self.collection_entries.saturating_add(1);
        }
        self.collection_pending = true;
        Ok(())
    }
}

fn canonicalize_set_value(value: StoredValue) -> StoredValue {
    match value {
        StoredValue::Number(value) if value.as_f64() == 0.0 => {
            StoredValue::Number(JsNumber::from_f64(0.0))
        }
        value => value,
    }
}
