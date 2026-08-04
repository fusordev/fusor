//! Weak-reference allocation, kept-alive targets, and registry cell updates.

use super::{
    FunctionId, HeapObject, HeapReference, ObjectId, ObjectRecord, RealmId, RealmIntrinsics,
    Runtime, RuntimeResource, StoredValue, check_execution_limit, stale_heap_reference,
    usize_to_u64,
};
use crate::object::{FinalizationRegistryState, WeakKey, WeakRefState};

impl Runtime {
    pub(crate) fn realm_weak_ref_prototype(
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
        let RealmIntrinsics::Ready { weak_ref, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm WeakRef intrinsics are not initialized",
            });
        };
        Ok(weak_ref.prototype)
    }

    pub(crate) fn realm_finalization_registry_prototype(
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
        let RealmIntrinsics::Ready {
            finalization_registry,
            ..
        } = state.intrinsics
        else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm FinalizationRegistry intrinsics are not initialized",
            });
        };
        Ok(finalization_registry.prototype)
    }

    pub(crate) fn allocate_weak_ref_object(
        &mut self,
        prototype: HeapReference,
        target: &StoredValue,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if WeakKey::from_value(target).is_none() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "WeakRef target was not validated",
            }
            .into());
        }
        self.preflight_weak_reference_object(prototype)?;
        let object = self
            .objects
            .try_insert(HeapObject::weak_ref(
                ObjectRecord::empty(Some(prototype)),
                WeakRefState::new(target),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn allocate_finalization_registry_object(
        &mut self,
        prototype: HeapReference,
        realm: RealmId,
        cleanup_callback: FunctionId,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.functions.contains(cleanup_callback) {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "FinalizationRegistry cleanup callback",
                index: cleanup_callback.index(),
                generation: cleanup_callback.generation(),
            }
            .into());
        }
        self.preflight_weak_reference_object(prototype)?;
        let object = self
            .objects
            .try_insert(HeapObject::finalization_registry(
                ObjectRecord::empty(Some(prototype)),
                FinalizationRegistryState::new(realm, cleanup_callback),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    fn preflight_weak_reference_object(
        &mut self,
        prototype: HeapReference,
    ) -> Result<(), crate::ExecutionError> {
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
            })
    }

    pub(crate) fn live_weak_target(&self, target: &WeakKey) -> Option<StoredValue> {
        match target {
            WeakKey::Function(function) if self.functions.contains(*function) => {
                Some(StoredValue::Function(*function))
            }
            WeakKey::Object(object) if self.objects.contains(*object) => {
                Some(StoredValue::Object(*object))
            }
            WeakKey::Symbol(symbol) => symbol.upgrade().map(StoredValue::Symbol),
            WeakKey::Function(_) | WeakKey::Object(_) => None,
        }
    }

    pub(crate) fn keep_alive(&mut self, value: StoredValue) -> Result<(), crate::ExecutionError> {
        if self
            .kept_alive
            .iter()
            .any(|candidate| candidate.same_value(&value))
        {
            return Ok(());
        }
        check_execution_limit(
            RuntimeResource::KeptAlive,
            self.limits.max_kept_alive,
            usize_to_u64(self.kept_alive.len()).saturating_add(1),
        )?;
        self.kept_alive
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::KeptAlive,
                additional: 1,
            })?;
        self.kept_alive.push(value);
        Ok(())
    }

    pub(crate) fn finalization_registry_register(
        &mut self,
        registry: ObjectId,
        target: &StoredValue,
        held_value: StoredValue,
        unregister_token: Option<&StoredValue>,
    ) -> Result<(), crate::ExecutionError> {
        check_execution_limit(
            RuntimeResource::CollectionEntries,
            self.limits.max_collection_entries,
            self.collection_entries.saturating_add(1),
        )?;
        let state = self
            .objects
            .get_mut(registry)
            .and_then(HeapObject::finalization_registry_state_mut)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "FinalizationRegistry object",
                index: registry.index(),
                generation: registry.generation(),
            })?;
        state
            .try_register(target, held_value, unregister_token)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::CollectionEntries,
                additional: 1,
            })?;
        self.collection_entries = self.collection_entries.saturating_add(1);
        self.collection_pending = true;
        Ok(())
    }

    pub(crate) fn finalization_registry_unregister(
        &mut self,
        registry: ObjectId,
        token: &StoredValue,
    ) -> Result<bool, crate::EngineFault> {
        let state = self
            .objects
            .get_mut(registry)
            .and_then(HeapObject::finalization_registry_state_mut)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "FinalizationRegistry object",
                index: registry.index(),
                generation: registry.generation(),
            })?;
        let removed = state.unregister(token);
        self.collection_entries = self
            .collection_entries
            .saturating_sub(usize_to_u64(removed));
        Ok(removed != 0)
    }

    pub(crate) fn take_finalization_cleanup_value(
        &mut self,
        registry: ObjectId,
    ) -> Result<Option<(RealmId, FunctionId, StoredValue)>, crate::EngineFault> {
        let state = self
            .objects
            .get_mut(registry)
            .and_then(HeapObject::finalization_registry_state_mut)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "FinalizationRegistry cleanup job",
                index: registry.index(),
                generation: registry.generation(),
            })?;
        let realm = state.realm();
        let callback = state.cleanup_callback();
        let value = state.take_cleanup_value();
        if value.is_some() {
            self.collection_entries = self.collection_entries.saturating_sub(1);
        }
        if !state.has_cleanup_cell() {
            state.set_cleanup_pending(false);
        }
        Ok(value.map(|value| (realm, callback, value)))
    }

    pub(crate) fn finish_finalization_cleanup_job(
        &mut self,
        registry: ObjectId,
    ) -> Result<(), crate::EngineFault> {
        let state = self
            .objects
            .get_mut(registry)
            .and_then(HeapObject::finalization_registry_state_mut)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "FinalizationRegistry cleanup job",
                index: registry.index(),
                generation: registry.generation(),
            })?;
        state.set_cleanup_pending(false);
        Ok(())
    }
}
