//! `RegExp` allocation and internal-slot access.

use super::{
    HeapObject, HeapReference, JsString, ObjectId, ObjectRecord, RegExpState, Runtime,
    RuntimeResource, check_execution_limit, stale_heap_reference, usize_to_u64,
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
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let source_len = source.len();
        let flags_len = flags.len();
        let object = self
            .objects
            .try_insert(HeapObject::regexp(
                ObjectRecord::empty(Some(prototype)),
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
            || state.last_index() != 0
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "RegExp allocation changed its internal slots",
            }
            .into());
        }
        self.collection_pending = true;
        Ok(object)
    }
}
