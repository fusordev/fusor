/*
 * Ordinary JavaScript object storage derived from QuickJS.
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

use std::{collections::TryReserveError, sync::Arc};

use crate::{
    PropertyKey, PropertyLayout, PropertyLayoutKind,
    value::{HeapReference, StoredValue},
};

#[derive(Clone)]
struct ShapeProperty {
    key: PropertyKey,
    layout: PropertyLayout,
}

pub(crate) struct ObjectRecord {
    prototype: Option<HeapReference>,
    extensible: bool,
    shape: Arc<Vec<ShapeProperty>>,
    slots: Vec<StoredValue>,
}

impl ObjectRecord {
    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "object shapes are Arc-owned by project contract but remain runtime-local"
    )]
    pub(crate) fn empty(prototype: Option<HeapReference>) -> Self {
        Self {
            prototype,
            extensible: true,
            shape: Arc::new(Vec::new()),
            slots: Vec::new(),
        }
    }

    pub(crate) const fn prototype(&self) -> Option<HeapReference> {
        self.prototype
    }

    pub(crate) fn replace_prototype(
        &mut self,
        prototype: Option<HeapReference>,
    ) -> Option<HeapReference> {
        std::mem::replace(&mut self.prototype, prototype)
    }

    pub(crate) const fn is_extensible(&self) -> bool {
        self.extensible
    }

    pub(crate) fn property_count(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &StoredValue> {
        self.slots.iter()
    }

    pub(crate) fn own_data_property(
        &self,
        key: &PropertyKey,
    ) -> Option<(PropertyLayout, StoredValue)> {
        let index = self
            .shape
            .iter()
            .position(|property| property.key == *key)?;
        let property = &self.shape[index];
        debug_assert_eq!(property.layout.kind(), PropertyLayoutKind::Data);
        Some((property.layout, self.slots[index].duplicate()))
    }

    pub(crate) fn replace_existing_data(&mut self, key: &PropertyKey, value: StoredValue) -> bool {
        let Some(index) = self.shape.iter().position(|property| property.key == *key) else {
            return false;
        };
        debug_assert_eq!(self.shape[index].layout.kind(), PropertyLayoutKind::Data);
        self.slots[index] = value;
        true
    }

    pub(crate) fn replace_existing_data_layout(
        &mut self,
        key: &PropertyKey,
        layout: PropertyLayout,
    ) -> Option<PropertyLayout> {
        debug_assert_eq!(layout.kind(), PropertyLayoutKind::Data);
        let index = self
            .shape
            .iter()
            .position(|property| property.key == *key)?;
        let shape = Arc::get_mut(&mut self.shape)
            .expect("object shape Arc is private and uniquely owned before shape interning");
        debug_assert_eq!(shape[index].layout.kind(), PropertyLayoutKind::Data);
        Some(std::mem::replace(&mut shape[index].layout, layout))
    }

    pub(crate) fn append_data(
        &mut self,
        key: PropertyKey,
        layout: PropertyLayout,
        value: StoredValue,
    ) -> Result<(), TryReserveError> {
        debug_assert_eq!(layout.kind(), PropertyLayoutKind::Data);
        debug_assert!(self.shape.iter().all(|property| property.key != key));

        self.slots.try_reserve(1)?;
        let shape = Arc::get_mut(&mut self.shape)
            .expect("object shape Arc is private and uniquely owned before shape interning");
        shape.try_reserve(1)?;

        shape.push(ShapeProperty { key, layout });
        self.slots.push(value);
        Ok(())
    }

    pub(crate) fn try_reserve_data(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.slots.try_reserve(additional)?;
        Arc::get_mut(&mut self.shape)
            .expect("object shape Arc is private and uniquely owned before shape interning")
            .try_reserve(additional)
    }

    pub(crate) fn pop_last_data(&mut self, key: &PropertyKey) -> Option<StoredValue> {
        if self
            .shape
            .last()
            .is_none_or(|property| property.key != *key)
        {
            return None;
        }
        let shape = Arc::get_mut(&mut self.shape)
            .expect("object shape Arc is private and uniquely owned before shape interning");
        let property = shape.pop()?;
        debug_assert_eq!(property.layout.kind(), PropertyLayoutKind::Data);
        self.slots.pop()
    }
}

pub(crate) struct HeapObject {
    pub(crate) record: ObjectRecord,
    pub(crate) public_roots: u32,
}

#[cfg(test)]
mod tests {
    use super::ObjectRecord;
    use crate::{ArrayIndex, PropertyKey, PropertyLayout, value::StoredValue};

    #[test]
    fn replacing_a_data_layout_preserves_value_and_can_be_rolled_back() {
        let key = PropertyKey::from_index(ArrayIndex::new(7).expect("array index"));
        let original = PropertyLayout::data(false, false, true);
        let replacement = PropertyLayout::data(true, true, true);
        let mut record = ObjectRecord::empty(None);
        record
            .append_data(key.clone(), original, StoredValue::Boolean(true))
            .expect("property");

        assert_eq!(
            record.replace_existing_data_layout(&key, replacement),
            Some(original)
        );
        let (layout, value) = record.own_data_property(&key).expect("updated property");
        assert_eq!(layout, replacement);
        assert!(matches!(value, StoredValue::Boolean(true)));

        assert_eq!(
            record.replace_existing_data_layout(&key, original),
            Some(replacement)
        );
        assert_eq!(
            record.own_data_property(&key).expect("restored property").0,
            original
        );
    }
}
