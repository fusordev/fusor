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
}

pub(crate) struct HeapObject {
    pub(crate) record: ObjectRecord,
    pub(crate) public_roots: u32,
}
