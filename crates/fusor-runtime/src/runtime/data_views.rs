//! `DataView` allocation and branded-state lookup.

use super::{
    DataViewState, HeapObject, HeapReference, ObjectId, ObjectRecord, Runtime, RuntimeResource,
    check_execution_limit, stale_heap_reference, usize_to_u64,
};

impl Runtime {
    /// Allocates a `DataView` after the constructor has completed every
    /// observable conversion and post-prototype-lookup buffer check.
    pub(crate) fn allocate_data_view(
        &mut self,
        prototype: HeapReference,
        state: DataViewState,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
        }
        if self.objects.get(state.buffer()).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "DataView backing buffer",
                index: state.buffer().index(),
                generation: state.buffer().generation(),
            }
            .into());
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
            .insert_heap_object(HeapObject::data_view(
                ObjectRecord::empty(Some(prototype)),
                state,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn data_view_state(
        &self,
        object: ObjectId,
    ) -> Result<Option<&DataViewState>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::data_view_state)
    }

    pub(crate) fn realm_data_view_prototype(
        &self,
        realm: super::RealmId,
    ) -> Result<ObjectId, crate::EngineFault> {
        let state = self
            .realms
            .get(realm)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })?;
        let super::RealmIntrinsics::Ready { data_view, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm DataView intrinsics are not initialized",
            });
        };
        if self.objects.get(data_view.prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "DataView.prototype intrinsic",
                index: data_view.prototype.index(),
                generation: data_view.prototype.generation(),
            });
        }
        Ok(data_view.prototype)
    }
}
