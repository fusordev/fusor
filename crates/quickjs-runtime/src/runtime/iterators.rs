/*
 * JavaScript iterator object storage derived from QuickJS.
 *
 * Copyright (c) 2017-2018 Fabrice Bellard
 * Copyright (c) 2017-2018 Charlie Gordon
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! Realm-owned synchronous iterator state and iterator-result allocation.

use super::{
    ArrayIndex, ArrayIterator, ArrayIteratorKind, ArrayState, FunctionId, HeapObject,
    HeapReference, JsNumber, JsString, ObjectId, ObjectRecord, PredefinedAtom, PropertyKey,
    PropertyLayout, RealmId, RealmIntrinsics, RegExpStringIterator, Runtime, RuntimeResource,
    StoredValue, StringIterator, check_execution_limit, stale_heap_reference, usize_to_u64,
};
use crate::object::{
    IteratorConcatIterable, IteratorHelperKind, IteratorHelperLifecycle, IteratorRecord,
    IteratorZipMode, IteratorZipRecord, OwnProperty,
};

pub(crate) struct PreparedIteratorResultPlan {
    result: ObjectRecord,
    entry_pair: Option<ObjectRecord>,
    callback_boundary: bool,
}

impl PreparedIteratorResultPlan {
    pub(crate) fn retained_values(&self) -> u64 {
        if self.entry_pair.is_some() { 5 } else { 2 }
    }

    pub(crate) fn mark_callback_boundary(&mut self) {
        self.callback_boundary = true;
    }

    fn additional_objects(&self) -> usize {
        if self.entry_pair.is_some() { 2 } else { 1 }
    }

    fn additional_properties(&self) -> u64 {
        self.retained_values()
    }
}

pub(crate) struct ArrayIteratorSnapshot {
    pub(crate) iterated: Option<StoredValue>,
    pub(crate) kind: ArrayIteratorKind,
    pub(crate) next: u64,
}

pub(crate) struct RegExpStringIteratorSnapshot {
    pub(crate) matcher: Option<StoredValue>,
    pub(crate) input: JsString,
    pub(crate) global: bool,
    pub(crate) full_unicode: bool,
    pub(crate) phase: crate::object::RegExpStringIteratorPhase,
}

pub(crate) struct IteratorHelperSnapshot {
    pub(crate) iterator: StoredValue,
    pub(crate) next_method: StoredValue,
    pub(crate) kind: IteratorHelperKind,
    pub(crate) callback: Option<FunctionId>,
    pub(crate) counter: u64,
    pub(crate) remaining: f64,
    pub(crate) lifecycle: IteratorHelperLifecycle,
    pub(crate) inner_iterator: Option<StoredValue>,
    pub(crate) inner_next_method: Option<StoredValue>,
    pub(crate) concat_iterable: Option<IteratorConcatIterable>,
    pub(crate) zip_mode: IteratorZipMode,
    pub(crate) zip_record_count: usize,
    pub(crate) zip_keys: Option<Vec<PropertyKey>>,
    pub(crate) chunk_source_done: bool,
}

impl Runtime {
    pub(crate) fn realm_iterator_constructor(
        &self,
        realm: RealmId,
    ) -> Result<FunctionId, crate::EngineFault> {
        let state = self
            .realms
            .get(realm)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "realm",
                index: realm.index(),
                generation: realm.generation(),
            })?;
        let RealmIntrinsics::Ready { iterators, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm iterator intrinsics are not initialized",
            });
        };
        Ok(iterators.constructor)
    }

    pub(crate) fn realm_iterator_prototype(
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
        let RealmIntrinsics::Ready { iterators, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm iterator intrinsics are not initialized",
            });
        };
        Ok(iterators.iterator_prototype)
    }

    pub(crate) fn realm_iterator_wrapper_prototype(
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
        let RealmIntrinsics::Ready { iterators, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm iterator intrinsics are not initialized",
            });
        };
        Ok(iterators.wrapper_prototype)
    }

    pub(crate) fn realm_iterator_helper_prototype(
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
        let RealmIntrinsics::Ready { iterators, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm iterator intrinsics are not initialized",
            });
        };
        Ok(iterators.helper_prototype)
    }

    pub(crate) fn allocate_iterator_wrapper(
        &mut self,
        realm: RealmId,
        iterator: StoredValue,
        next_method: StoredValue,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_iterator_wrapper_prototype(realm)?;
        self.allocate_iterator_object(HeapObject::iterator_wrapper(
            ObjectRecord::empty(Some(HeapReference::Object(prototype))),
            IteratorRecord::new(iterator, next_method),
        ))
    }

    pub(crate) fn allocate_iterator_callback_helper(
        &mut self,
        realm: RealmId,
        iterator: StoredValue,
        next_method: StoredValue,
        kind: IteratorHelperKind,
        callback: FunctionId,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_iterator_helper_prototype(realm)?;
        self.allocate_iterator_object(HeapObject::iterator_wrapper(
            ObjectRecord::empty(Some(HeapReference::Object(prototype))),
            IteratorRecord::new_callback_helper(iterator, next_method, kind, callback),
        ))
    }

    pub(crate) fn allocate_iterator_limit_helper(
        &mut self,
        realm: RealmId,
        iterator: StoredValue,
        next_method: StoredValue,
        kind: IteratorHelperKind,
        remaining: f64,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_iterator_helper_prototype(realm)?;
        self.allocate_iterator_object(HeapObject::iterator_wrapper(
            ObjectRecord::empty(Some(HeapReference::Object(prototype))),
            IteratorRecord::new_limit_helper(iterator, next_method, kind, remaining),
        ))
    }

    pub(crate) fn allocate_iterator_chunking_helper(
        &mut self,
        realm: RealmId,
        iterator: StoredValue,
        next_method: StoredValue,
        kind: IteratorHelperKind,
        size: u32,
        allow_partial: bool,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_iterator_helper_prototype(realm)?;
        self.allocate_iterator_object(HeapObject::iterator_wrapper(
            ObjectRecord::empty(Some(HeapReference::Object(prototype))),
            IteratorRecord::new_chunking_helper(iterator, next_method, kind, size, allow_partial),
        ))
    }

    pub(crate) fn allocate_iterator_concat_helper(
        &mut self,
        realm: RealmId,
        iterables: Vec<IteratorConcatIterable>,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_iterator_helper_prototype(realm)?;
        self.allocate_iterator_object(HeapObject::iterator_wrapper(
            ObjectRecord::empty(Some(HeapReference::Object(prototype))),
            IteratorRecord::new_concat_helper(iterables),
        ))
    }

    pub(crate) fn allocate_iterator_zip_helper(
        &mut self,
        realm: RealmId,
        records: Vec<IteratorZipRecord>,
        padding: Vec<StoredValue>,
        mode: IteratorZipMode,
        keys: Option<Vec<PropertyKey>>,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_iterator_helper_prototype(realm)?;
        self.allocate_iterator_object(HeapObject::iterator_wrapper(
            ObjectRecord::empty(Some(HeapReference::Object(prototype))),
            IteratorRecord::new_zip_helper(records, padding, mode, keys),
        ))
    }

    pub(crate) fn iterator_wrapper_record(
        &self,
        wrapper: ObjectId,
    ) -> Result<Option<IteratorRecord>, crate::EngineFault> {
        let object = self
            .objects
            .get(wrapper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(wrapper)))?;
        Ok(object
            .iterator_wrapper_state()
            .filter(|record| record.helper().is_none())
            .map(|record| {
                IteratorRecord::new(
                    record.iterator().duplicate(),
                    record.next_method().duplicate(),
                )
            }))
    }

    pub(crate) fn iterator_helper_snapshot(
        &self,
        helper: ObjectId,
    ) -> Result<Option<IteratorHelperSnapshot>, crate::EngineFault> {
        let object = self
            .objects
            .get(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?;
        let Some(record) = object.iterator_wrapper_state() else {
            return Ok(None);
        };
        let Some(helper_state) = record.helper() else {
            return Ok(None);
        };
        Ok(Some(IteratorHelperSnapshot {
            iterator: record.iterator().duplicate(),
            next_method: record.next_method().duplicate(),
            kind: helper_state.kind(),
            callback: helper_state.callback(),
            counter: helper_state.counter(),
            remaining: helper_state.remaining(),
            lifecycle: helper_state.lifecycle(),
            inner_iterator: helper_state.inner_iterator().map(StoredValue::duplicate),
            inner_next_method: helper_state.inner_next_method().map(StoredValue::duplicate),
            concat_iterable: helper_state
                .current_concat_iterable()
                .map(IteratorConcatIterable::duplicate),
            zip_mode: helper_state.zip_mode(),
            zip_record_count: helper_state.zip_records().len(),
            zip_keys: helper_state.zip_keys().map(<[PropertyKey]>::to_vec),
            chunk_source_done: helper_state.chunk_source_done(),
        }))
    }

    pub(crate) fn iterator_zip_record(
        &self,
        helper: ObjectId,
        index: usize,
    ) -> Result<Option<IteratorZipRecord>, crate::EngineFault> {
        let helper_state = self
            .objects
            .get(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state()
            .and_then(IteratorRecord::helper)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator.zip state disappeared",
            })?;
        Ok(helper_state
            .zip_record(index)
            .map(IteratorZipRecord::duplicate))
    }

    pub(crate) fn iterator_zip_padding(
        &self,
        helper: ObjectId,
        index: usize,
    ) -> Result<Option<StoredValue>, crate::EngineFault> {
        let helper_state = self
            .objects
            .get(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state()
            .and_then(IteratorRecord::helper)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator.zip state disappeared",
            })?;
        Ok(helper_state
            .zip_padding()
            .get(index)
            .map(StoredValue::duplicate))
    }

    pub(crate) fn finish_iterator_zip_record(
        &mut self,
        helper: ObjectId,
        index: usize,
    ) -> Result<(), crate::EngineFault> {
        let helper_state = self
            .objects
            .get_mut(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state_mut()
            .and_then(IteratorRecord::helper_mut)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator.zip state disappeared",
            })?;
        if !helper_state.finish_zip_record(index) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Iterator.zip record index is missing",
            });
        }
        Ok(())
    }

    pub(crate) fn finish_iterator_zip_yield(
        &mut self,
        helper: ObjectId,
    ) -> Result<(), crate::EngineFault> {
        let helper_state = self
            .objects
            .get_mut(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state_mut()
            .and_then(IteratorRecord::helper_mut)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator.zip state disappeared",
            })?;
        helper_state.finish_zip_yield();
        Ok(())
    }

    pub(crate) fn iterator_zip_open_iterators(
        &self,
        helper: ObjectId,
    ) -> Result<Vec<StoredValue>, crate::ExecutionError> {
        let helper_state = self
            .objects
            .get(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state()
            .and_then(IteratorRecord::helper)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator.zip state disappeared",
            })?;
        let open_count = helper_state
            .zip_records()
            .iter()
            .filter(|record| !record.is_done())
            .count();
        let mut iterators = Vec::new();
        iterators.try_reserve_exact(open_count).map_err(|_| {
            crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::FrameValues,
                additional: open_count,
            }
        })?;
        iterators.extend(
            helper_state
                .zip_records()
                .iter()
                .filter(|record| !record.is_done())
                .map(|record| record.iterator().duplicate()),
        );
        Ok(iterators)
    }

    pub(crate) fn current_iterator_concat_iterable(
        &self,
        helper: ObjectId,
    ) -> Result<Option<IteratorConcatIterable>, crate::EngineFault> {
        let helper_state = self
            .objects
            .get(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state()
            .and_then(IteratorRecord::helper)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator concat state disappeared",
            })?;
        Ok(helper_state
            .current_concat_iterable()
            .map(IteratorConcatIterable::duplicate))
    }

    pub(crate) fn set_iterator_helper_lifecycle(
        &mut self,
        helper: ObjectId,
        lifecycle: IteratorHelperLifecycle,
    ) -> Result<(), crate::EngineFault> {
        let helper_state = self
            .objects
            .get_mut(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state_mut()
            .and_then(IteratorRecord::helper_mut)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator Helper state disappeared",
            })?;
        helper_state.set_lifecycle(lifecycle);
        Ok(())
    }

    pub(crate) fn finish_iterator_helper_callback(
        &mut self,
        helper: ObjectId,
        yielded: bool,
    ) -> Result<(), crate::EngineFault> {
        let helper_state = self
            .objects
            .get_mut(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state_mut()
            .and_then(IteratorRecord::helper_mut)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator Helper state disappeared",
            })?;
        helper_state.finish_callback(yielded);
        Ok(())
    }

    pub(crate) fn consume_iterator_helper_remaining(
        &mut self,
        helper: ObjectId,
    ) -> Result<f64, crate::EngineFault> {
        let helper_state = self
            .objects
            .get_mut(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state_mut()
            .and_then(IteratorRecord::helper_mut)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator Helper state disappeared",
            })?;
        Ok(helper_state.consume_remaining())
    }

    pub(crate) fn finish_iterator_limit_yield(
        &mut self,
        helper: ObjectId,
    ) -> Result<(), crate::EngineFault> {
        let helper_state = self
            .objects
            .get_mut(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state_mut()
            .and_then(IteratorRecord::helper_mut)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator Helper state disappeared",
            })?;
        helper_state.finish_limit_yield();
        Ok(())
    }

    pub(crate) fn push_iterator_chunking_value(
        &mut self,
        helper: ObjectId,
        value: StoredValue,
    ) -> Result<Option<Vec<StoredValue>>, crate::ExecutionError> {
        let helper_state = self
            .objects
            .get_mut(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state_mut()
            .and_then(IteratorRecord::helper_mut)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator chunking state disappeared",
            })?;
        let output = helper_state.push_chunking_value(value).map_err(|_| {
            crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::FrameValues,
                additional: 1,
            }
        })?;
        self.collection_pending = true;
        Ok(output)
    }

    pub(crate) fn finish_iterator_chunking_source(
        &mut self,
        helper: ObjectId,
    ) -> Result<Option<Vec<StoredValue>>, crate::EngineFault> {
        let helper_state = self
            .objects
            .get_mut(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state_mut()
            .and_then(IteratorRecord::helper_mut)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator chunking state disappeared",
            })?;
        let output = helper_state.finish_chunking_source();
        self.collection_pending = true;
        Ok(output)
    }

    pub(crate) fn install_iterator_helper_inner(
        &mut self,
        helper: ObjectId,
        iterator: StoredValue,
        next_method: StoredValue,
    ) -> Result<(), crate::EngineFault> {
        let helper_state = self
            .objects
            .get_mut(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state_mut()
            .and_then(IteratorRecord::helper_mut)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator Helper state disappeared",
            })?;
        helper_state.install_inner(iterator, next_method);
        Ok(())
    }

    pub(crate) fn finish_iterator_flat_map_yield(
        &mut self,
        helper: ObjectId,
    ) -> Result<(), crate::EngineFault> {
        let helper_state = self
            .objects
            .get_mut(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state_mut()
            .and_then(IteratorRecord::helper_mut)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator Helper state disappeared",
            })?;
        helper_state.finish_flat_map_yield();
        Ok(())
    }

    pub(crate) fn finish_iterator_flat_map_inner(
        &mut self,
        helper: ObjectId,
    ) -> Result<(), crate::EngineFault> {
        let helper_state = self
            .objects
            .get_mut(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state_mut()
            .and_then(IteratorRecord::helper_mut)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator Helper state disappeared",
            })?;
        helper_state.finish_flat_map_inner();
        Ok(())
    }

    pub(crate) fn finish_iterator_concat_inner(
        &mut self,
        helper: ObjectId,
    ) -> Result<(), crate::EngineFault> {
        let helper_state = self
            .objects
            .get_mut(helper)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(helper)))?
            .iterator_wrapper_state_mut()
            .and_then(IteratorRecord::helper_mut)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Iterator concat state disappeared",
            })?;
        helper_state.finish_concat_inner();
        Ok(())
    }

    pub(crate) fn allocate_async_from_sync_iterator(
        &mut self,
        realm: RealmId,
        iterator: StoredValue,
        next: StoredValue,
    ) -> Result<ObjectId, crate::ExecutionError> {
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties.saturating_add(2),
        )?;
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let prototype = self.realm_async_from_sync_iterator_prototype(realm)?;
        let mut record = ObjectRecord::empty(Some(HeapReference::Object(prototype)));
        record
            .try_reserve_data(2)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 2,
            })?;
        let internal = PropertyLayout::data(false, false, false);
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Value),
                internal,
                iterator,
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 2,
            })?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Next),
                internal,
                next,
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 2,
            })?;
        let wrapper = self
            .insert_heap_object(HeapObject::ordinary(record))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.object_properties = self.object_properties.saturating_add(2);
        self.collection_pending = true;
        Ok(wrapper)
    }

    pub(crate) fn async_from_sync_iterator_record(
        &self,
        realm: RealmId,
        wrapper: ObjectId,
    ) -> Result<Option<(StoredValue, StoredValue)>, crate::EngineFault> {
        let expected = self.realm_async_from_sync_iterator_prototype(realm)?;
        let Some(object) = self.objects.get(wrapper) else {
            return Err(stale_heap_reference(HeapReference::Object(wrapper)));
        };
        if object.record.prototype() != Some(HeapReference::Object(expected)) {
            return Ok(None);
        }
        let value = match object
            .record
            .own_property(&self.predefined_property_key(PredefinedAtom::Value))
        {
            Some(OwnProperty::Data { value, .. }) => value,
            Some(OwnProperty::Accessor { .. }) | None => return Ok(None),
        };
        let next = match object
            .record
            .own_property(&self.predefined_property_key(PredefinedAtom::Next))
        {
            Some(OwnProperty::Data { value, .. }) => value,
            Some(OwnProperty::Accessor { .. }) | None => return Ok(None),
        };
        Ok(Some((value, next)))
    }

    pub(crate) fn preflight_iterator_result_allocation(
        &mut self,
        additional_objects: usize,
        additional_properties: u64,
    ) -> Result<(), crate::ExecutionError> {
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(usize_to_u64(additional_objects)),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties.saturating_add(additional_properties),
        )?;
        self.objects.try_reserve(additional_objects).map_err(|_| {
            crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: additional_objects,
            }
        })
    }

    pub(crate) fn prepare_iterator_result_allocation(
        &mut self,
        realm: RealmId,
        entry_key: Option<StoredValue>,
    ) -> Result<PreparedIteratorResultPlan, crate::ExecutionError> {
        let result_prototype = self.realm_object_prototype(realm)?;
        let entry_prototype = entry_key
            .as_ref()
            .map(|_| self.realm_array_prototype(realm))
            .transpose()?;
        let additional_objects = if entry_key.is_some() { 2 } else { 1 };
        let additional_properties = if entry_key.is_some() { 5 } else { 2 };
        self.preflight_iterator_result_allocation(additional_objects, additional_properties)?;

        let layout = PropertyLayout::data(true, true, true);
        let mut result = ObjectRecord::empty(Some(HeapReference::Object(result_prototype)));
        result
            .try_reserve_data(2)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 2,
            })?;
        result
            .append_data(
                self.predefined_property_key(PredefinedAtom::Value),
                layout,
                StoredValue::Undefined,
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 2,
            })?;
        result
            .append_data(
                self.predefined_property_key(PredefinedAtom::Done),
                layout,
                StoredValue::Boolean(false),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 2,
            })?;

        let entry_pair = entry_key
            .zip(entry_prototype)
            .map(|(entry_key, prototype)| {
                let mut pair = ObjectRecord::empty(Some(HeapReference::Object(prototype)));
                pair.try_reserve_data(3)
                    .map_err(|_| crate::ExecutionError::AllocationFailed {
                        resource: RuntimeResource::ObjectProperties,
                        additional: 3,
                    })?;
                pair.append_data(
                    self.predefined_property_key(PredefinedAtom::Length),
                    PropertyLayout::data(true, false, false),
                    StoredValue::Number(JsNumber::from_i32(2)),
                )
                .map_err(|_| crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::ObjectProperties,
                    additional: 3,
                })?;
                for (index, value) in [entry_key, StoredValue::Undefined].into_iter().enumerate() {
                    pair.append_data(
                        PropertyKey::from_index(
                            ArrayIndex::new(
                                u32::try_from(index)
                                    .expect("the prepared entry pair has two canonical indices"),
                            )
                            .expect("the prepared entry pair never uses the length sentinel"),
                        ),
                        layout,
                        value,
                    )
                    .map_err(|_| crate::ExecutionError::AllocationFailed {
                        resource: RuntimeResource::ObjectProperties,
                        additional: 3,
                    })?;
                }
                Ok::<ObjectRecord, crate::ExecutionError>(pair)
            })
            .transpose()?;
        Ok(PreparedIteratorResultPlan {
            result,
            entry_pair,
            callback_boundary: false,
        })
    }

    pub(crate) fn commit_prepared_iterator_result(
        &mut self,
        mut prepared: PreparedIteratorResultPlan,
        value: StoredValue,
        done: bool,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if prepared.callback_boundary {
            self.preflight_iterator_result_allocation(
                prepared.additional_objects(),
                prepared.additional_properties(),
            )?;
        }
        let mut pair_id = None;
        let value = if let Some(mut pair) = prepared.entry_pair.take() {
            let value_key = PropertyKey::from_index(
                ArrayIndex::new(1).expect("the prepared entry value index is canonical"),
            );
            assert!(pair.replace_existing_data(&value_key, value));
            let object = self
                .insert_heap_object(HeapObject::array(pair, ArrayState::sparse(2)))
                .map_err(|_| crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::HeapObjects,
                    additional: 2,
                })?;
            pair_id = Some(object);
            StoredValue::Object(object)
        } else {
            value
        };
        assert!(prepared.result.replace_existing_data(
            &self.predefined_property_key(PredefinedAtom::Value),
            value,
        ));
        assert!(prepared.result.replace_existing_data(
            &self.predefined_property_key(PredefinedAtom::Done),
            StoredValue::Boolean(done),
        ));
        let additional_objects = if pair_id.is_some() { 2 } else { 1 };
        let Ok(result) = self.insert_heap_object(HeapObject::ordinary(prepared.result)) else {
            if let Some(pair) = pair_id {
                debug_assert!(self.objects.remove(pair).is_some());
            }
            return Err(crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: additional_objects,
            });
        };
        self.object_properties =
            self.object_properties
                .saturating_add(if pair_id.is_some() { 5 } else { 2 });
        self.collection_pending = true;
        Ok(result)
    }

    pub(crate) fn finish_iterator_result(
        &mut self,
        result: ObjectId,
        value: StoredValue,
        done: bool,
    ) -> Result<(), crate::ExecutionError> {
        let value_key = self.predefined_property_key(PredefinedAtom::Value);
        let done_key = self.predefined_property_key(PredefinedAtom::Done);
        let record = self.object_record_mut(HeapReference::Object(result))?;
        if !record.replace_existing_data(&value_key, value)
            || !record.replace_existing_data(&done_key, StoredValue::Boolean(done))
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "reserved iterator result lost its data properties",
            }
            .into());
        }
        Ok(())
    }

    pub(crate) fn discard_reserved_iterator_result(
        &mut self,
        result: ObjectId,
    ) -> Result<(), crate::EngineFault> {
        let object = self
            .objects
            .get(result)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "reserved iterator result",
                index: result.index(),
                generation: result.generation(),
            })?;
        if object.record.property_count() != 2 {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "reserved iterator result changed before delegated yield",
            });
        }
        let object = self
            .objects
            .remove(result)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "reserved iterator result",
                index: result.index(),
                generation: result.generation(),
            })?;
        debug_assert_eq!(object.record.property_count(), 2);
        self.object_properties = self.object_properties.saturating_sub(2);
        Ok(())
    }

    pub(crate) fn realm_array_iterator_prototype(
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
        let RealmIntrinsics::Ready { iterators, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm iterator intrinsics are not initialized",
            });
        };
        let prototype = self.objects.get(iterators.array_iterator_prototype).ok_or(
            crate::EngineFault::StaleHeapEdge {
                edge: "Array Iterator prototype intrinsic",
                index: iterators.array_iterator_prototype.index(),
                generation: iterators.array_iterator_prototype.generation(),
            },
        )?;
        if prototype.record.prototype() != Some(HeapReference::Object(iterators.iterator_prototype))
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "Array Iterator prototype has the wrong prototype",
            });
        }
        Ok(iterators.array_iterator_prototype)
    }

    pub(crate) fn realm_string_iterator_prototype(
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
        let RealmIntrinsics::Ready { iterators, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm iterator intrinsics are not initialized",
            });
        };
        let prototype = self
            .objects
            .get(iterators.string_iterator_prototype)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "String Iterator prototype intrinsic",
                index: iterators.string_iterator_prototype.index(),
                generation: iterators.string_iterator_prototype.generation(),
            })?;
        if prototype.record.prototype() != Some(HeapReference::Object(iterators.iterator_prototype))
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "String Iterator prototype has the wrong prototype",
            });
        }
        Ok(iterators.string_iterator_prototype)
    }

    pub(crate) fn allocate_array_iterator(
        &mut self,
        realm: RealmId,
        iterated: StoredValue,
        kind: ArrayIteratorKind,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_array_iterator_prototype(realm)?;
        self.allocate_iterator_object(HeapObject::array_iterator(
            ObjectRecord::empty(Some(HeapReference::Object(prototype))),
            ArrayIterator::new(iterated, kind),
        ))
    }

    pub(crate) fn allocate_string_iterator(
        &mut self,
        realm: RealmId,
        iterated: JsString,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_string_iterator_prototype(realm)?;
        self.allocate_iterator_object(HeapObject::string_iterator(
            ObjectRecord::empty(Some(HeapReference::Object(prototype))),
            StringIterator::new(iterated),
        ))
    }

    pub(crate) fn realm_regexp_string_iterator_prototype(
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
        let RealmIntrinsics::Ready { iterators, .. } = state.intrinsics else {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "realm iterator intrinsics are not initialized",
            });
        };
        let prototype = self
            .objects
            .get(iterators.regexp_string_iterator_prototype)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "RegExp String Iterator prototype intrinsic",
                index: iterators.regexp_string_iterator_prototype.index(),
                generation: iterators.regexp_string_iterator_prototype.generation(),
            })?;
        if prototype.record.prototype() != Some(HeapReference::Object(iterators.iterator_prototype))
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "RegExp String Iterator prototype has the wrong prototype",
            });
        }
        Ok(iterators.regexp_string_iterator_prototype)
    }

    pub(crate) fn allocate_regexp_string_iterator(
        &mut self,
        realm: RealmId,
        matcher: StoredValue,
        input: JsString,
        global: bool,
        full_unicode: bool,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_regexp_string_iterator_prototype(realm)?;
        self.allocate_iterator_object(HeapObject::regexp_string_iterator(
            ObjectRecord::empty(Some(HeapReference::Object(prototype))),
            RegExpStringIterator::new(matcher, input, global, full_unicode),
        ))
    }

    pub(crate) fn regexp_string_iterator_snapshot(
        &self,
        object: ObjectId,
    ) -> Result<RegExpStringIteratorSnapshot, crate::EngineFault> {
        let object = self
            .objects
            .get(object)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(object)))?;
        let iterator =
            object
                .regexp_string_iterator_state()
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "RegExp String Iterator method called on an incompatible receiver",
                })?;
        Ok(RegExpStringIteratorSnapshot {
            matcher: iterator.matcher().map(StoredValue::duplicate),
            input: iterator.input().clone(),
            global: iterator.global(),
            full_unicode: iterator.full_unicode(),
            phase: iterator.phase(),
        })
    }

    pub(crate) fn mark_regexp_string_iterator_non_global_yielded(
        &mut self,
        object: ObjectId,
    ) -> Result<(), crate::EngineFault> {
        self.objects
            .get_mut(object)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(object)))?
            .regexp_string_iterator_state_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "RegExp String Iterator method called on an incompatible receiver",
            })?
            .mark_non_global_yielded();
        Ok(())
    }

    pub(crate) fn start_regexp_string_iterator(
        &mut self,
        object: ObjectId,
    ) -> Result<(), crate::EngineFault> {
        self.objects
            .get_mut(object)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(object)))?
            .regexp_string_iterator_state_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "RegExp String Iterator method called on an incompatible receiver",
            })?
            .start();
        Ok(())
    }

    pub(crate) fn suspend_regexp_string_iterator(
        &mut self,
        object: ObjectId,
    ) -> Result<(), crate::EngineFault> {
        self.objects
            .get_mut(object)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(object)))?
            .regexp_string_iterator_state_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "RegExp String Iterator method called on an incompatible receiver",
            })?
            .suspend();
        Ok(())
    }

    pub(crate) fn finish_regexp_string_iterator(
        &mut self,
        object: ObjectId,
    ) -> Result<(), crate::EngineFault> {
        self.objects
            .get_mut(object)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(object)))?
            .regexp_string_iterator_state_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "RegExp String Iterator method called on an incompatible receiver",
            })?
            .finish();
        self.collection_pending = true;
        Ok(())
    }

    fn allocate_iterator_object(
        &mut self,
        iterator: HeapObject,
    ) -> Result<ObjectId, crate::ExecutionError> {
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
        let object = self.insert_heap_object(iterator).map_err(|_| {
            crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            }
        })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn array_iterator_snapshot(
        &self,
        object: ObjectId,
    ) -> Result<ArrayIteratorSnapshot, crate::EngineFault> {
        let object = self
            .objects
            .get(object)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(object)))?;
        let iterator =
            object
                .array_iterator_state()
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "Array Iterator method called on an incompatible receiver",
                })?;
        Ok(ArrayIteratorSnapshot {
            iterated: iterator.iterated().map(StoredValue::duplicate),
            kind: iterator.kind(),
            next: iterator.next(),
        })
    }

    pub(crate) fn advance_array_iterator(
        &mut self,
        object: ObjectId,
    ) -> Result<(), crate::EngineFault> {
        self.objects
            .get_mut(object)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(object)))?
            .array_iterator_state_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Array Iterator method called on an incompatible receiver",
            })?
            .advance();
        Ok(())
    }

    pub(crate) fn finish_array_iterator(
        &mut self,
        object: ObjectId,
    ) -> Result<(), crate::EngineFault> {
        self.objects
            .get_mut(object)
            .ok_or_else(|| stale_heap_reference(HeapReference::Object(object)))?
            .array_iterator_state_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "Array Iterator method called on an incompatible receiver",
            })?
            .finish();
        self.collection_pending = true;
        Ok(())
    }

    pub(crate) fn string_iterator_next(
        &mut self,
        object: ObjectId,
    ) -> Result<Option<JsString>, crate::ExecutionError> {
        let (string, index) = {
            let object = self
                .objects
                .get(object)
                .ok_or_else(|| stale_heap_reference(HeapReference::Object(object)))?;
            let iterator =
                object
                    .string_iterator_state()
                    .ok_or(crate::EngineFault::RuntimeInvariant {
                        message: "String Iterator method called on an incompatible receiver",
                    })?;
            (iterator.iterated().cloned(), iterator.next())
        };
        let Some(string) = string else {
            return Ok(None);
        };
        if index >= string.len() {
            self.objects
                .get_mut(object)
                .expect("live String Iterator remains present")
                .string_iterator_state_mut()
                .expect("validated String Iterator retains its class")
                .finish();
            return Ok(None);
        }

        let first = string
            .code_unit_at(index)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "String Iterator index exceeded its retained String",
            })?;
        let width = if (0xD800..=0xDBFF).contains(&first)
            && string
                .code_unit_at(index.saturating_add(1))
                .is_some_and(|second| (0xDC00..=0xDFFF).contains(&second))
        {
            2
        } else {
            1
        };
        let next = index.saturating_add(width);
        let value = string.slice(index..next)?;
        self.objects
            .get_mut(object)
            .expect("live String Iterator remains present")
            .string_iterator_state_mut()
            .expect("validated String Iterator retains its class")
            .set_next(next);
        Ok(Some(value))
    }

    pub(crate) fn allocate_iterator_result(
        &mut self,
        realm: RealmId,
        value: StoredValue,
        done: bool,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prepared = self.prepare_iterator_result_allocation(realm, None)?;
        self.commit_prepared_iterator_result(prepared, value, done)
    }
}
