//! `RegExp` allocation and internal-slot access.

use super::{
    HeapObject, HeapReference, JsNumber, JsString, ObjectId, ObjectRecord, PredefinedAtom,
    PropertyLayout, RealmId, RealmIntrinsics, RegExpState, Runtime, RuntimeResource, StoredValue,
    check_execution_limit, stale_heap_reference, usize_to_u64,
};

impl Runtime {
    pub(crate) fn allocate_regexp_object(
        &mut self,
        prototype: HeapReference,
        source: JsString,
        flags: JsString,
        matcher: quickjs_regexp::CompiledRegExp,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
        }
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties.saturating_add(1),
        )?;
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let source_len = source.len();
        let flags_len = flags.len();
        let mut record = ObjectRecord::empty(Some(prototype));
        record
            .try_reserve_data(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::LastIndex),
                PropertyLayout::data(true, false, false),
                StoredValue::Number(JsNumber::from_i32(0)),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        let object = self
            .insert_heap_object(HeapObject::regexp(
                record,
                RegExpState::new(source, flags, matcher),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let state = self
            .objects
            .get(object)
            .and_then(HeapObject::regexp_state)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "RegExp allocation lost its internal slots",
            })?;
        if state.source().len() != source_len
            || state.flags().len() != flags_len
            || state.matcher().flags().encode_utf16().count() != flags_len as usize
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "RegExp allocation changed its internal slots",
            }
            .into());
        }
        self.object_properties = self.object_properties.saturating_add(1);
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn regexp_state(
        &self,
        object: ObjectId,
    ) -> Result<Option<&RegExpState>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::regexp_state)
    }

    pub(crate) fn regexp_state_mut(
        &mut self,
        object: ObjectId,
    ) -> Result<Option<&mut RegExpState>, crate::EngineFault> {
        self.objects
            .get_mut(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::regexp_state_mut)
    }

    pub(crate) fn realm_regexp_prototype(
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
        let RealmIntrinsics::Ready { regexp, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm RegExp intrinsics are not initialized",
            });
        };
        if self.objects.get(regexp.prototype).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "RegExp.prototype intrinsic",
                index: regexp.prototype.index(),
                generation: regexp.prototype.generation(),
            });
        }
        Ok(regexp.prototype)
    }
}
