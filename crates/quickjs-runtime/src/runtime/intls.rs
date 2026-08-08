//! `%Intl%` object allocation and Locale internal-slot access.

use super::{
    HeapObject, HeapReference, JsString, ObjectId, ObjectRecord, RealmId, RealmIntrinsics, Runtime,
    RuntimeResource, check_execution_limit, stale_heap_reference, usize_to_u64,
};

impl Runtime {
    pub(crate) fn allocate_intl_locale(
        &mut self,
        prototype: HeapReference,
        locale: JsString,
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
            .insert_heap_object(HeapObject::intl_locale(
                ObjectRecord::empty(Some(prototype)),
                locale,
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn intl_locale_value(
        &self,
        object: ObjectId,
    ) -> Result<Option<&JsString>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::intl_locale_value)
    }

    pub(crate) fn realm_intl_locale_prototype(
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
        let RealmIntrinsics::Ready { intl, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm Intl intrinsics are not initialized",
            });
        };
        if self.objects.get(intl.locale_prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "Intl.Locale.prototype intrinsic",
                index: intl.locale_prototype.index(),
                generation: intl.locale_prototype.generation(),
            });
        }
        Ok(intl.locale_prototype)
    }
}
