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

//! Realm-local frozen Arrays for tagged-template site constants.

use super::{
    ArrayIndex, ArrayState, HeapObject, HeapReference, InstalledTemplateElement, JsNumber,
    ObjectId, ObjectRecord, PredefinedAtom, PropertyKey, PropertyLayout, RealmId, Runtime,
    RuntimeResource, StoredValue, check_execution_limit, stale_heap_reference, usize_to_u64,
};

fn build_template_records(
    prototype: HeapReference,
    length_key: PropertyKey,
    length: u32,
    elements: &[InstalledTemplateElement],
) -> Result<(ObjectRecord, ObjectRecord), crate::ExecutionError> {
    let raw_property_count = elements.len().saturating_add(1);
    let cooked_property_count = elements.len().saturating_add(2);
    let length_value = StoredValue::Number(JsNumber::from_f64(f64::from(length)));
    let frozen_length = PropertyLayout::data(false, false, false);
    let frozen_element = PropertyLayout::data(false, true, false);

    let mut raw_record = ObjectRecord::empty(Some(prototype));
    raw_record
        .try_reserve_data(raw_property_count)
        .map_err(|_| crate::ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: raw_property_count,
        })?;
    raw_record
        .append_data(length_key.clone(), frozen_length, length_value.duplicate())
        .map_err(|_| crate::ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: raw_property_count,
        })?;

    let mut cooked_record = ObjectRecord::empty(Some(prototype));
    cooked_record
        .try_reserve_data(cooked_property_count)
        .map_err(|_| crate::ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: cooked_property_count,
        })?;
    cooked_record
        .append_data(length_key, frozen_length, length_value)
        .map_err(|_| crate::ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: cooked_property_count,
        })?;

    for (index, element) in elements.iter().enumerate() {
        let index = u32::try_from(index).map_err(|_| crate::EngineFault::RuntimeInvariant {
            message: "verified template element index exceeds u32",
        })?;
        let index = ArrayIndex::new(index).ok_or(crate::EngineFault::RuntimeInvariant {
            message: "verified template element index is not canonical",
        })?;
        let key = PropertyKey::from_index(index);
        raw_record
            .append_data(
                key.clone(),
                frozen_element,
                StoredValue::String(element.raw.clone()),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: raw_property_count,
            })?;
        cooked_record
            .append_data(
                key,
                frozen_element,
                element
                    .cooked
                    .as_ref()
                    .map_or(StoredValue::Undefined, |value| {
                        StoredValue::String(value.clone())
                    }),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: cooked_property_count,
            })?;
    }
    Ok((raw_record, cooked_record))
}

impl Runtime {
    /// Materializes the two frozen Arrays required by `GetTemplateObject`.
    pub(crate) fn allocate_template_object(
        &mut self,
        realm: RealmId,
        elements: &[InstalledTemplateElement],
    ) -> Result<ObjectId, crate::ExecutionError> {
        let length =
            u32::try_from(elements.len()).map_err(|_| crate::ExecutionError::LimitExceeded {
                resource: RuntimeResource::ObjectProperties,
                limit: u64::from(u32::MAX),
                observed: usize_to_u64(elements.len()),
            })?;
        let property_count = elements
            .len()
            .checked_mul(2)
            .and_then(|count| count.checked_add(3))
            .ok_or(crate::ExecutionError::LimitExceeded {
                resource: RuntimeResource::ObjectProperties,
                limit: self.limits.max_object_properties,
                observed: u64::MAX,
            })?;
        let prototype = self.realm_array_prototype(realm)?;
        let prototype = HeapReference::Object(prototype);
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
        }
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(2),
        )?;
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties
                .saturating_add(usize_to_u64(property_count)),
        )?;
        self.objects
            .try_reserve(2)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 2,
            })?;

        let cooked_property_count = elements.len().saturating_add(2);
        let length_key = self.predefined_property_key(PredefinedAtom::Length);
        let raw_key = self.predefined_property_key(PredefinedAtom::Raw);
        let (mut raw_record, mut cooked_record) =
            build_template_records(prototype, length_key, length, elements)?;
        raw_record.prevent_extensions();
        let raw_object = self
            .insert_heap_object(HeapObject::array(raw_record, ArrayState::sparse(length)))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 2,
            })?;

        if cooked_record
            .append_data(
                raw_key,
                PropertyLayout::data(false, false, false),
                StoredValue::Object(raw_object),
            )
            .is_err()
        {
            let _ = self.objects.remove(raw_object);
            return Err(crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: cooked_property_count,
            });
        }
        cooked_record.prevent_extensions();
        let Ok(template) =
            self.insert_heap_object(HeapObject::array(cooked_record, ArrayState::sparse(length)))
        else {
            let _ = self.objects.remove(raw_object);
            return Err(crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            });
        };
        self.object_properties = self
            .object_properties
            .saturating_add(usize_to_u64(property_count));
        self.collection_pending = true;
        Ok(template)
    }
}
