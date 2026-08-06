//! Temporal object allocation and internal-slot access.

use temporal_rs::Instant;

use super::{
    HeapObject, HeapReference, ObjectId, ObjectRecord, RealmId, RealmIntrinsics, Runtime,
    RuntimeResource, check_execution_limit, stale_heap_reference, usize_to_u64,
};

impl Runtime {
    pub(crate) fn allocate_temporal_instant(
        &mut self,
        prototype: HeapReference,
        instant: Instant,
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
            .insert_heap_object(HeapObject::temporal_instant(
                ObjectRecord::empty(Some(prototype)),
                instant,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn temporal_instant(
        &self,
        object: ObjectId,
    ) -> Result<Option<Instant>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::temporal_instant_value)
    }

    pub(crate) fn realm_temporal_instant_prototype(
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
        let RealmIntrinsics::Ready { temporal, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Temporal intrinsics are not initialized",
            });
        };
        if self.objects.get(temporal.instant_prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Temporal.Instant.prototype intrinsic",
                index: temporal.instant_prototype.index(),
                generation: temporal.instant_prototype.generation(),
            });
        }
        Ok(temporal.instant_prototype)
    }
}
