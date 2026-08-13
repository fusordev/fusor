//! Date allocation and `[[DateValue]]` access.

use super::{
    DateState, HeapObject, HeapReference, JsNumber, ObjectId, ObjectRecord, RealmId,
    RealmIntrinsics, Runtime, RuntimeResource, check_execution_limit, stale_heap_reference,
    usize_to_u64,
};

impl Runtime {
    pub(crate) fn allocate_date_object(
        &mut self,
        prototype: HeapReference,
        value: JsNumber,
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
            .insert_heap_object(HeapObject::date(
                ObjectRecord::empty(Some(prototype)),
                DateState::new(value),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn date_value(
        &self,
        object: ObjectId,
    ) -> Result<Option<JsNumber>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(|object| object.date_state().map(|state| state.value()))
    }

    pub(crate) fn set_date_value(
        &mut self,
        object: ObjectId,
        value: JsNumber,
    ) -> Result<(), crate::EngineFault> {
        let state = self
            .objects
            .get_mut(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })?
            .date_state_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Date mutation lost its [[DateValue]] slot",
            })?;
        state.set_value(value);
        Ok(())
    }

    pub(crate) fn realm_date_prototype(
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
        let RealmIntrinsics::Ready { date, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Date intrinsics are not initialized",
            });
        };
        if self.objects.get(date.prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Date.prototype intrinsic",
                index: date.prototype.index(),
                generation: date.prototype.generation(),
            });
        }
        Ok(date.prototype)
    }
}
