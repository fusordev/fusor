/*
 * JavaScript runtime and closure ownership derived from QuickJS.
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

//! Array exotic allocation, indexed property definition, and length mutation.

use super::{
    ArrayDefineOutcome, ArrayLengthWriteOutcome, ArrayState, Atom, BindingCellId, HeapObject,
    HeapReference, JsNumber, ObjectId, ObjectRecord, OwnProperty, PredefinedAtom, PropertyKey,
    PropertyLayout, PropertyLayoutKind, RealmId, Runtime, RuntimeResource, SlotValue, StoredValue,
    check_execution_limit, stale_heap_reference, usize_to_u64,
};
use crate::object::HeapObjectKind;

struct ArrayDefinitionFacts {
    length: u32,
    existing: ArrayPropertyLocation,
    extensible: bool,
    length_writable: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ArrayPropertyLocation {
    Absent,
    Dense,
    Sparse,
}

fn default_dense_data_value(property: &OwnProperty) -> Option<StoredValue> {
    match property {
        OwnProperty::Data { layout, value }
            if *layout == PropertyLayout::data(true, true, true) =>
        {
            Some(value.duplicate())
        }
        OwnProperty::Data { .. } | OwnProperty::Accessor { .. } => None,
    }
}

impl Runtime {
    pub(crate) fn allocate_array(
        &mut self,
        realm: RealmId,
        elements: Vec<StoredValue>,
    ) -> Result<ObjectId, crate::ExecutionError> {
        let prototype = self.realm_array_prototype(realm)?;
        self.allocate_array_with_prototype(HeapReference::Object(prototype), elements)
    }

    pub(crate) fn allocate_array_with_prototype(
        &mut self,
        prototype: HeapReference,
        elements: Vec<StoredValue>,
    ) -> Result<ObjectId, crate::ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
        }
        let property_count =
            elements
                .len()
                .checked_add(1)
                .ok_or(crate::ExecutionError::LimitExceeded {
                    resource: RuntimeResource::ObjectProperties,
                    limit: u64::from(u32::MAX).saturating_add(1),
                    observed: u64::MAX,
                })?;
        let length =
            u32::try_from(elements.len()).map_err(|_| crate::ExecutionError::LimitExceeded {
                resource: RuntimeResource::ObjectProperties,
                limit: u64::from(u32::MAX).saturating_add(1),
                observed: usize_to_u64(property_count),
            })?;
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties
                .saturating_add(usize_to_u64(property_count)),
        )?;
        self.objects
            .try_reserve(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;

        let mut record = ObjectRecord::empty(Some(prototype));
        record
            .try_reserve_data(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: property_count,
            })?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Length),
                PropertyLayout::data(true, false, false),
                StoredValue::Number(JsNumber::from_f64(f64::from(length))),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: property_count,
            })?;
        let object = self
            .insert_heap_object(HeapObject::array(
                record,
                ArrayState::dense(length, elements),
            ))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.object_properties = self
            .object_properties
            .saturating_add(usize_to_u64(property_count));
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn allocate_sparse_array_with_prototype(
        &mut self,
        prototype: HeapReference,
        length: u32,
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

        let mut record = ObjectRecord::empty(Some(prototype));
        record
            .try_reserve_data(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        record
            .append_data(
                self.predefined_property_key(PredefinedAtom::Length),
                PropertyLayout::data(true, false, false),
                StoredValue::Number(JsNumber::from_f64(f64::from(length))),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        let object = self
            .insert_heap_object(HeapObject::array(record, ArrayState::new(length)))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.object_properties = self.object_properties.saturating_add(1);
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn is_array_object(&self, object: ObjectId) -> Result<bool, crate::EngineFault> {
        self.objects
            .get(object)
            .map(|object| object.array_state().is_some())
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
    }

    pub(crate) fn is_arguments_object(&self, object: ObjectId) -> Result<bool, crate::EngineFault> {
        self.objects
            .get(object)
            .map(HeapObject::is_arguments)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
    }

    pub(crate) fn mapped_arguments_cell(
        &self,
        object: ObjectId,
        key: &PropertyKey,
    ) -> Result<Option<BindingCellId>, crate::EngineFault> {
        let Some(index) = key.as_index() else {
            return Ok(None);
        };
        self.objects
            .get(object)
            .map(|object| object.arguments_cell(index.get()))
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
    }

    pub(crate) fn mapped_arguments_value(
        &self,
        object: ObjectId,
        key: &PropertyKey,
    ) -> Result<Option<StoredValue>, crate::EngineFault> {
        let Some(cell) = self.mapped_arguments_cell(object, key)? else {
            return Ok(None);
        };
        match &self
            .cells
            .get(cell)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "mapped arguments binding cell",
                index: cell.index(),
                generation: cell.generation(),
            })?
            .value
        {
            SlotValue::Value(value) => Ok(Some(value.duplicate())),
            SlotValue::Uninitialized => Err(crate::EngineFault::RuntimeInvariant {
                message: "mapped arguments binding is initialized",
            }),
        }
    }

    pub(crate) fn replace_mapped_arguments_cell_value(
        &mut self,
        cell: BindingCellId,
        value: StoredValue,
    ) -> Result<(), crate::EngineFault> {
        self.cells
            .get_mut(cell)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "mapped arguments binding cell",
                index: cell.index(),
                generation: cell.generation(),
            })?
            .value = SlotValue::Value(value);
        self.collection_pending = true;
        Ok(())
    }

    pub(crate) fn detach_mapped_arguments_property(
        &mut self,
        object: ObjectId,
        key: &PropertyKey,
    ) -> Result<Option<BindingCellId>, crate::EngineFault> {
        let Some(index) = key.as_index() else {
            return Ok(None);
        };
        let detached = self
            .objects
            .get_mut(object)
            .map(|object| object.detach_arguments_cell(index.get()))
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })?;
        if detached.is_some() {
            self.collection_pending = true;
        }
        Ok(detached)
    }

    pub(crate) fn synchronize_mapped_arguments_property(
        &mut self,
        object: ObjectId,
        key: &PropertyKey,
    ) -> Result<(), crate::EngineFault> {
        let Some(value) = self.mapped_arguments_value(object, key)? else {
            return Ok(());
        };
        if !self
            .objects
            .get_mut(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })?
            .record
            .replace_existing_data(key, value)
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "mapped arguments property remains present data",
            });
        }
        self.collection_pending = true;
        Ok(())
    }

    pub(crate) fn array_length(&self, object: ObjectId) -> Result<Option<u32>, crate::EngineFault> {
        self.objects
            .get(object)
            .map(|object| object.array_state().map(ArrayState::length))
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
    }

    pub(crate) fn array_own_property(
        &self,
        object: ObjectId,
        key: &PropertyKey,
    ) -> Result<Option<OwnProperty>, crate::EngineFault> {
        let object = self
            .objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })?;
        if object.array_state().is_none() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "array own-property lookup received a non-array object",
            });
        }
        Ok(object.array_own_property(key))
    }

    /// Reports whether a heap object's own properties include an array index.
    ///
    /// This includes an Array exotic object's dense elements, which do not
    /// live in its ordinary [`ObjectRecord`] shape.
    pub(crate) fn heap_has_indexed_own_property(
        &self,
        reference: HeapReference,
    ) -> Result<bool, crate::EngineFault> {
        if self.object_record(reference)?.has_indexed_property() {
            return Ok(true);
        }
        let HeapReference::Object(object) = reference else {
            return Ok(false);
        };
        let object = self
            .objects
            .get(object)
            .ok_or_else(|| stale_heap_reference(reference))?;
        Ok(object
            .array_state()
            .is_some_and(|state| state.dense_property_count() != 0))
    }

    /// Whether indexed-property queries on `reference` are entirely described
    /// by its ordinary record and (for Arrays) dense element storage.
    ///
    /// Proxy, boxed-String, arguments, and typed-array exotics can synthesize
    /// indexed properties without an ordinary shape entry, so optimized Array
    /// traversals must continue through their general internal-method path.
    pub(crate) fn has_static_indexed_properties(
        &self,
        reference: HeapReference,
    ) -> Result<bool, crate::EngineFault> {
        if self.proxy_state(reference)?.is_some() {
            return Ok(false);
        }
        let HeapReference::Object(object) = reference else {
            return Ok(true);
        };
        let object = self
            .objects
            .get(object)
            .ok_or_else(|| stale_heap_reference(reference))?;
        Ok(!matches!(
            object.kind(),
            HeapObjectKind::Arguments(_)
                | HeapObjectKind::BoxedPrimitive(_)
                | HeapObjectKind::TypedArray(_)
        ))
    }

    pub(crate) fn preview_array_define_data_property_work(
        &self,
        object: ObjectId,
    ) -> Result<u64, crate::ExecutionError> {
        let object = self
            .objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "array object",
                index: object.index(),
                generation: object.generation(),
            })?;
        if object.array_state().is_none() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "array definition work preview received a non-array object",
            }
            .into());
        }
        Ok(usize_to_u64(object.property_count())
            .saturating_mul(4)
            .saturating_add(4))
    }

    pub(crate) fn preview_array_length_write_work(
        &self,
        object: ObjectId,
        _requested_length: u32,
    ) -> Result<u64, crate::ExecutionError> {
        let object = self
            .objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "array object",
                index: object.index(),
                generation: object.generation(),
            })?;
        if object.array_state().is_none() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "array length work preview received a non-array object",
            }
            .into());
        }
        // The mutation performs at most four linear shape passes: length
        // descriptor lookup, blocker discovery, stable compaction, and length
        // slot update. Return a conservative bound before any mutation.
        Ok(usize_to_u64(object.property_count())
            .saturating_mul(4)
            .saturating_add(4))
    }

    fn array_definition_facts(
        &self,
        object: ObjectId,
        key: &PropertyKey,
    ) -> Result<ArrayDefinitionFacts, crate::EngineFault> {
        let array = self
            .objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "array object",
                index: object.index(),
                generation: object.generation(),
            })?;
        let length = array
            .array_state()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "array property definition received a non-array object",
            })?
            .length();
        let length_property = array
            .record
            .own_property(&self.predefined_property_key(PredefinedAtom::Length))
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "array object has no own length property",
            })?;
        let dense_exists = key.as_index().is_some_and(|index| {
            array
                .array_state()
                .is_some_and(|state| state.dense_value(index).is_some())
        });
        let exists = array.array_own_property(key).is_some();
        Ok(ArrayDefinitionFacts {
            length,
            existing: if dense_exists {
                ArrayPropertyLocation::Dense
            } else if exists {
                ArrayPropertyLocation::Sparse
            } else {
                ArrayPropertyLocation::Absent
            },
            extensible: array.record.is_extensible(),
            length_writable: length_property.layout().writable() == Some(true),
        })
    }

    fn replace_existing_array_property(
        &mut self,
        object: ObjectId,
        key: &PropertyKey,
        property: OwnProperty,
        dense_exists: bool,
    ) -> Result<(), crate::ExecutionError> {
        if dense_exists {
            if let Some(value) = default_dense_data_value(&property) {
                let index = key
                    .as_index()
                    .expect("only an indexed property can be dense");
                let created = self
                    .objects
                    .get_mut(object)
                    .expect("live array remains present")
                    .array_state_mut()
                    .expect("array state remains present")
                    .try_store_dense(index, value)
                    .map_err(|_| crate::ExecutionError::AllocationFailed {
                        resource: RuntimeResource::ObjectProperties,
                        additional: 1,
                    })?;
                debug_assert!(!created, "a located dense property is replaced");
                self.collection_pending = true;
                return Ok(());
            }
            self.objects
                .get_mut(object)
                .expect("live array remains present")
                .transition_array_to_sparse()
                .map_err(|_| crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::ObjectProperties,
                    additional: 1,
                })?;
        }
        if self
            .objects
            .get_mut(object)
            .expect("live array remains present")
            .record
            .restore_existing_property(key, property)
            .is_none()
        {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "located array property disappeared before its definition",
            }
            .into());
        }
        self.collection_pending = true;
        Ok(())
    }

    fn try_store_new_dense_array_property(
        &mut self,
        object: ObjectId,
        key: &PropertyKey,
        property: &OwnProperty,
    ) -> Result<bool, crate::ExecutionError> {
        let (Some(index), Some(value)) = (key.as_index(), default_dense_data_value(property))
        else {
            return Ok(false);
        };
        if !self
            .objects
            .get(object)
            .and_then(HeapObject::array_state)
            .is_some_and(|state| state.can_store_dense(index))
        {
            return Ok(false);
        }
        let created = self
            .objects
            .get_mut(object)
            .expect("live array remains present")
            .array_state_mut()
            .expect("array state remains present")
            .try_store_dense(index, value)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        debug_assert!(created, "an absent dense property is created");
        Ok(true)
    }

    pub(crate) fn define_array_data_property(
        &mut self,
        object: ObjectId,
        key: PropertyKey,
        layout: PropertyLayout,
        value: StoredValue,
    ) -> Result<ArrayDefineOutcome, crate::ExecutionError> {
        if layout.kind() != PropertyLayoutKind::Data {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "array data-property definition received an accessor layout",
            }
            .into());
        }
        self.define_array_own_property(object, key, OwnProperty::Data { layout, value })
    }

    pub(crate) fn define_array_own_property(
        &mut self,
        object: ObjectId,
        key: PropertyKey,
        property: OwnProperty,
    ) -> Result<ArrayDefineOutcome, crate::ExecutionError> {
        if key.as_atom().and_then(Atom::predefined_atom) == Some(PredefinedAtom::Length) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "array length definition bypassed numeric length validation",
            }
            .into());
        }
        let facts = self.array_definition_facts(object, &key)?;
        let extended_length = key.as_index().and_then(|index| {
            (index.get() >= facts.length).then_some(index.get().saturating_add(1))
        });
        if extended_length.is_some() && !facts.length_writable {
            return Ok(ArrayDefineOutcome::ReadOnlyLength);
        }
        if facts.existing != ArrayPropertyLocation::Absent {
            self.replace_existing_array_property(
                object,
                &key,
                property,
                facts.existing == ArrayPropertyLocation::Dense,
            )?;
            if let Some(length) = extended_length {
                self.update_array_length(object, length)?;
            }
            return Ok(ArrayDefineOutcome::Complete);
        }
        if !facts.extensible {
            return Ok(ArrayDefineOutcome::NonExtensible);
        }
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties.saturating_add(1),
        )?;
        if self.try_store_new_dense_array_property(object, &key, &property)? {
            if let Some(length) = extended_length {
                self.update_array_length(object, length)?;
            }
            self.object_properties = self.object_properties.saturating_add(1);
            self.collection_pending = true;
            return Ok(ArrayDefineOutcome::Complete);
        }
        if key.as_index().is_some() {
            self.objects
                .get_mut(object)
                .expect("live array remains present")
                .transition_array_to_sparse()
                .map_err(|_| crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::ObjectProperties,
                    additional: 1,
                })?;
        }
        self.append_own_property(HeapReference::Object(object), key, property)?;
        if let Some(length) = extended_length {
            self.update_array_length(object, length)?;
        }
        self.collection_pending = true;
        Ok(ArrayDefineOutcome::Complete)
    }

    pub(crate) fn set_array_length(
        &mut self,
        object: ObjectId,
        requested_length: u32,
    ) -> Result<ArrayLengthWriteOutcome, crate::EngineFault> {
        let length_key = self.predefined_property_key(PredefinedAtom::Length);
        let (current_length, writable) = {
            let array = self
                .objects
                .get(object)
                .ok_or(crate::EngineFault::StaleHeapEdge {
                    edge: "array object",
                    index: object.index(),
                    generation: object.generation(),
                })?;
            let current_length = array
                .array_state()
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "array length write received a non-array object",
                })?
                .length();
            let property = array.record.own_property(&length_key).ok_or(
                crate::EngineFault::RuntimeInvariant {
                    message: "array object has no own length property",
                },
            )?;
            (current_length, property.layout().writable() == Some(true))
        };
        if !writable {
            return Ok(ArrayLengthWriteOutcome::ReadOnly);
        }
        if requested_length >= current_length {
            self.update_array_length(object, requested_length)?;
            self.collection_pending = true;
            return Ok(ArrayLengthWriteOutcome::Complete);
        }

        if self
            .objects
            .get(object)
            .and_then(HeapObject::array_state)
            .is_some_and(ArrayState::is_dense)
        {
            let removed = self
                .objects
                .get_mut(object)
                .expect("live array remains present")
                .array_state_mut()
                .expect("array state remains present")
                .truncate_dense(requested_length);
            self.object_properties = self.object_properties.saturating_sub(usize_to_u64(removed));
            self.update_array_length(object, requested_length)?;
            self.collection_pending = true;
            return Ok(ArrayLengthWriteOutcome::Complete);
        }

        let truncation = self
            .objects
            .get_mut(object)
            .expect("live array remains present")
            .record
            .truncate_array_indices(requested_length);
        self.object_properties = self
            .object_properties
            .saturating_sub(usize_to_u64(truncation.removed()));
        self.update_array_length(object, truncation.final_length())?;
        self.collection_pending = true;
        Ok(match truncation.blocked_index() {
            Some(index) => ArrayLengthWriteOutcome::BlockedByNonConfigurable {
                index,
                final_length: truncation.final_length(),
            },
            None => ArrayLengthWriteOutcome::Complete,
        })
    }

    /// Applies the final `writable` attribute selected by `ArraySetLength` after
    /// all indexed deletions have completed (or stopped at a blocker).
    pub(crate) fn set_array_length_writable(
        &mut self,
        object: ObjectId,
        writable: bool,
    ) -> Result<(), crate::EngineFault> {
        let length_key = self.predefined_property_key(PredefinedAtom::Length);
        let array = self
            .objects
            .get_mut(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "array object",
                index: object.index(),
                generation: object.generation(),
            })?;
        if array.array_state().is_none() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "array length layout update received a non-array object",
            });
        }
        let previous = array.record.replace_existing_data_layout(
            &length_key,
            PropertyLayout::data(writable, false, false),
        );
        if previous.is_none() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "array object lost its own data length layout",
            });
        }
        self.collection_pending = true;
        Ok(())
    }

    fn update_array_length(
        &mut self,
        object: ObjectId,
        length: u32,
    ) -> Result<(), crate::EngineFault> {
        let length_key = self.predefined_property_key(PredefinedAtom::Length);
        let array = self
            .objects
            .get_mut(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "array object",
                index: object.index(),
                generation: object.generation(),
            })?;
        let replaced = array.record.replace_existing_data(
            &length_key,
            StoredValue::Number(JsNumber::from_f64(f64::from(length))),
        );
        if !replaced {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "array object lost its own data length property",
            });
        }
        let state = array
            .array_state_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "array length update received a non-array object",
            })?;
        state.replace_length(length);
        Ok(())
    }
}
