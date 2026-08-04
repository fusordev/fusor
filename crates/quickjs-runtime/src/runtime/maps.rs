//! Map allocation, intrinsic lookup, and resource-accounted storage updates.

use super::{
    HeapObject, HeapReference, JsNumber, ObjectId, ObjectRecord, RealmId, RealmIntrinsics, Runtime,
    RuntimeResource, StoredValue, check_execution_limit, stale_heap_reference, usize_to_u64,
};
use crate::object::{MapIterator, MapIteratorKind, MapSetOutcome, MapState};

impl Runtime {
    pub(crate) fn realm_map_prototype(
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
        let RealmIntrinsics::Ready { map, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Map intrinsics are not initialized",
            });
        };
        Ok(map.prototype)
    }

    pub(crate) fn realm_map_iterator_prototype(
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
        let RealmIntrinsics::Ready { map, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Map intrinsics are not initialized",
            });
        };
        Ok(map.iterator_prototype)
    }

    pub(crate) fn allocate_map_object(
        &mut self,
        prototype: HeapReference,
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
        let object = self
            .objects
            .try_insert(HeapObject::map(
                ObjectRecord::empty(Some(prototype)),
                MapState::empty(),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn allocate_map_iterator(
        &mut self,
        realm: RealmId,
        map: ObjectId,
        kind: MapIteratorKind,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_map_iterator_prototype(realm)?;
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
            .objects
            .try_insert(HeapObject::map_iterator(
                ObjectRecord::empty(Some(HeapReference::Object(prototype))),
                MapIterator::new(map, kind),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn map_set(
        &mut self,
        map: ObjectId,
        key: StoredValue,
        value: StoredValue,
    ) -> Result<(), crate::ExecutionError> {
        let key = canonicalize_map_key(key);
        let inserts = self
            .objects
            .get(map)
            .and_then(HeapObject::map_state)
            .is_some_and(|state| !state.contains_key(&key));
        if inserts {
            check_execution_limit(
                RuntimeResource::CollectionEntries,
                self.limits.max_collection_entries,
                self.collection_entries.saturating_add(1),
            )?;
        }
        let state = self
            .objects
            .get_mut(map)
            .and_then(HeapObject::map_state_mut)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "Map object",
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
}

fn canonicalize_map_key(key: StoredValue) -> StoredValue {
    match key {
        StoredValue::Number(value) if value.as_f64() == 0.0 => {
            StoredValue::Number(JsNumber::from_f64(0.0))
        }
        key => key,
    }
}
