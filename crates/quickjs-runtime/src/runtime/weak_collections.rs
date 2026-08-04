//! Weak collection allocation and resource-accounted storage updates.

use super::{
    HeapObject, HeapReference, ObjectId, ObjectRecord, RealmId, RealmIntrinsics, Runtime,
    RuntimeResource, StoredValue, check_execution_limit, stale_heap_reference, usize_to_u64,
};
use crate::object::{MapSetOutcome, WeakKey, WeakMapState, WeakSetState};

impl Runtime {
    pub(crate) fn realm_weak_map_prototype(
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
        let RealmIntrinsics::Ready { weak_map, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm WeakMap intrinsics are not initialized",
            });
        };
        Ok(weak_map.prototype)
    }

    pub(crate) fn realm_weak_set_prototype(
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
        let RealmIntrinsics::Ready { weak_set, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm WeakSet intrinsics are not initialized",
            });
        };
        Ok(weak_set.prototype)
    }

    pub(crate) fn allocate_weak_map_object(
        &mut self,
        prototype: HeapReference,
    ) -> Result<ObjectId, crate::ExecutionError> {
        self.allocate_weak_collection_object(prototype, true)
    }

    pub(crate) fn allocate_weak_set_object(
        &mut self,
        prototype: HeapReference,
    ) -> Result<ObjectId, crate::ExecutionError> {
        self.allocate_weak_collection_object(prototype, false)
    }

    fn allocate_weak_collection_object(
        &mut self,
        prototype: HeapReference,
        map: bool,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
        }
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
        let record = ObjectRecord::empty(Some(prototype));
        let object = if map {
            HeapObject::weak_map(record, WeakMapState::empty())
        } else {
            HeapObject::weak_set(record, WeakSetState::empty())
        };
        let object = self.objects.try_insert(object).map_err(|_| {
            crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            }
        })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn weak_map_set(
        &mut self,
        map: ObjectId,
        key: &StoredValue,
        value: StoredValue,
    ) -> Result<(), crate::ExecutionError> {
        if WeakKey::from_value(key).is_none() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "WeakMap key was not validated",
            }
            .into());
        }
        let inserts = self
            .objects
            .get(map)
            .and_then(HeapObject::weak_map_state)
            .is_some_and(|state| !state.contains_key(key));
        if inserts {
            self.preflight_weak_collection_insert()?;
        }
        let state = self
            .objects
            .get_mut(map)
            .and_then(HeapObject::weak_map_state_mut)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "WeakMap object",
                index: map.index(),
                generation: map.generation(),
            })?;
        let outcome =
            state
                .try_set(key, value)
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

    pub(crate) fn weak_set_add(
        &mut self,
        set: ObjectId,
        value: &StoredValue,
    ) -> Result<(), crate::ExecutionError> {
        if WeakKey::from_value(value).is_none() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "WeakSet value was not validated",
            }
            .into());
        }
        let inserts = self
            .objects
            .get(set)
            .and_then(HeapObject::weak_set_state)
            .is_some_and(|state| !state.contains(value));
        if inserts {
            self.preflight_weak_collection_insert()?;
        }
        let state = self
            .objects
            .get_mut(set)
            .and_then(HeapObject::weak_set_state_mut)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "WeakSet object",
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

    pub(crate) fn weak_map_delete(
        &mut self,
        map: ObjectId,
        key: &StoredValue,
    ) -> Result<bool, crate::EngineFault> {
        let state = self
            .objects
            .get_mut(map)
            .and_then(HeapObject::weak_map_state_mut)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "WeakMap object",
                index: map.index(),
                generation: map.generation(),
            })?;
        let deleted = state.delete(key);
        if deleted {
            self.collection_entries = self.collection_entries.saturating_sub(1);
        }
        Ok(deleted)
    }

    pub(crate) fn weak_set_delete(
        &mut self,
        set: ObjectId,
        value: &StoredValue,
    ) -> Result<bool, crate::EngineFault> {
        let state = self
            .objects
            .get_mut(set)
            .and_then(HeapObject::weak_set_state_mut)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "WeakSet object",
                index: set.index(),
                generation: set.generation(),
            })?;
        let deleted = state.delete(value);
        if deleted {
            self.collection_entries = self.collection_entries.saturating_sub(1);
        }
        Ok(deleted)
    }

    fn preflight_weak_collection_insert(&self) -> Result<(), crate::ExecutionError> {
        check_execution_limit(
            RuntimeResource::CollectionEntries,
            self.limits.max_collection_entries,
            self.collection_entries.saturating_add(1),
        )
    }
}
