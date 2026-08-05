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
    ArrayIndex, ArrayIterator, ArrayIteratorKind, ArrayState, HeapObject, HeapReference, JsNumber,
    JsString, ObjectId, ObjectRecord, PredefinedAtom, PropertyKey, PropertyLayout, RealmId,
    RealmIntrinsics, Runtime, RuntimeResource, StoredValue, StringIterator, check_execution_limit,
    stale_heap_reference, usize_to_u64,
};
use crate::object::OwnProperty;

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
    pub(crate) next: u32,
}

impl Runtime {
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
