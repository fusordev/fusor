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
    Atom, JsNumber, JsString, PropertyKey, PropertyLayout, PropertyLayoutKind,
    ids::FunctionId,
    value::{HeapReference, StoredValue},
};

#[derive(Clone)]
struct ShapeProperty {
    key: PropertyKey,
    layout: PropertyLayout,
}

enum PropertySlot {
    Data(StoredValue),
    Accessor {
        getter: Option<FunctionId>,
        setter: Option<FunctionId>,
    },
}

pub(crate) enum OwnProperty {
    Data {
        layout: PropertyLayout,
        value: StoredValue,
    },
    Accessor {
        layout: PropertyLayout,
        getter: Option<FunctionId>,
        setter: Option<FunctionId>,
    },
}

impl OwnProperty {
    pub(crate) const fn layout(&self) -> PropertyLayout {
        match self {
            Self::Data { layout, .. } | Self::Accessor { layout, .. } => *layout,
        }
    }

    pub(crate) fn duplicate(&self) -> Self {
        match self {
            Self::Data { layout, value } => Self::Data {
                layout: *layout,
                value: value.duplicate(),
            },
            Self::Accessor {
                layout,
                getter,
                setter,
            } => Self::Accessor {
                layout: *layout,
                getter: *getter,
                setter: *setter,
            },
        }
    }

    fn into_parts(self) -> (PropertyLayout, PropertySlot) {
        match self {
            Self::Data { layout, value } => (layout, PropertySlot::Data(value)),
            Self::Accessor {
                layout,
                getter,
                setter,
            } => (layout, PropertySlot::Accessor { getter, setter }),
        }
    }
}

pub(crate) struct ObjectRecord {
    prototype: Option<HeapReference>,
    extensible: bool,
    shape: Arc<Vec<ShapeProperty>>,
    slots: Vec<PropertySlot>,
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
        self.slots.iter().filter_map(|slot| match slot {
            PropertySlot::Data(value) => Some(value),
            PropertySlot::Accessor { .. } => None,
        })
    }

    pub(crate) fn accessor_functions(&self) -> impl Iterator<Item = FunctionId> + '_ {
        self.slots.iter().flat_map(|slot| {
            let (getter, setter) = match slot {
                PropertySlot::Data(_) => (None, None),
                PropertySlot::Accessor { getter, setter } => (*getter, *setter),
            };
            [getter, setter].into_iter().flatten()
        })
    }

    pub(crate) fn own_property(&self, key: &PropertyKey) -> Option<OwnProperty> {
        let index = self
            .shape
            .iter()
            .position(|property| property.key == *key)?;
        let property = &self.shape[index];
        match &self.slots[index] {
            PropertySlot::Data(value) => {
                debug_assert_eq!(property.layout.kind(), PropertyLayoutKind::Data);
                Some(OwnProperty::Data {
                    layout: property.layout,
                    value: value.duplicate(),
                })
            }
            PropertySlot::Accessor { getter, setter } => {
                debug_assert_eq!(property.layout.kind(), PropertyLayoutKind::Accessor);
                Some(OwnProperty::Accessor {
                    layout: property.layout,
                    getter: *getter,
                    setter: *setter,
                })
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn own_data_property(
        &self,
        key: &PropertyKey,
    ) -> Option<(PropertyLayout, StoredValue)> {
        match self.own_property(key)? {
            OwnProperty::Data { layout, value } => Some((layout, value)),
            OwnProperty::Accessor { .. } => None,
        }
    }

    pub(crate) fn replace_existing_data(&mut self, key: &PropertyKey, value: StoredValue) -> bool {
        let Some(index) = self.shape.iter().position(|property| property.key == *key) else {
            return false;
        };
        match &mut self.slots[index] {
            PropertySlot::Data(existing) => {
                debug_assert_eq!(self.shape[index].layout.kind(), PropertyLayoutKind::Data);
                *existing = value;
                true
            }
            PropertySlot::Accessor { .. } => false,
        }
    }

    #[allow(
        dead_code,
        reason = "the data-specific layout mutation remains available to later descriptor paths"
    )]
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
        if !matches!(self.slots[index], PropertySlot::Data(_)) {
            return None;
        }
        let shape = Arc::get_mut(&mut self.shape)
            .expect("object shape Arc is private and uniquely owned before shape interning");
        debug_assert_eq!(shape[index].layout.kind(), PropertyLayoutKind::Data);
        Some(std::mem::replace(&mut shape[index].layout, layout))
    }

    pub(crate) fn replace_existing_with_data(
        &mut self,
        key: &PropertyKey,
        layout: PropertyLayout,
        value: StoredValue,
    ) -> Option<OwnProperty> {
        debug_assert_eq!(layout.kind(), PropertyLayoutKind::Data);
        self.replace_existing_property(key, OwnProperty::Data { layout, value })
    }

    pub(crate) fn replace_existing_with_accessor(
        &mut self,
        key: &PropertyKey,
        layout: PropertyLayout,
        getter: Option<FunctionId>,
        setter: Option<FunctionId>,
    ) -> Option<OwnProperty> {
        debug_assert_eq!(layout.kind(), PropertyLayoutKind::Accessor);
        self.replace_existing_property(
            key,
            OwnProperty::Accessor {
                layout,
                getter,
                setter,
            },
        )
    }

    pub(crate) fn restore_existing_property(
        &mut self,
        key: &PropertyKey,
        property: OwnProperty,
    ) -> Option<OwnProperty> {
        self.replace_existing_property(key, property)
    }

    fn replace_existing_property(
        &mut self,
        key: &PropertyKey,
        replacement: OwnProperty,
    ) -> Option<OwnProperty> {
        let index = self
            .shape
            .iter()
            .position(|property| property.key == *key)?;
        let previous = match &self.slots[index] {
            PropertySlot::Data(value) => OwnProperty::Data {
                layout: self.shape[index].layout,
                value: value.duplicate(),
            },
            PropertySlot::Accessor { getter, setter } => OwnProperty::Accessor {
                layout: self.shape[index].layout,
                getter: *getter,
                setter: *setter,
            },
        };
        let (layout, slot) = replacement.into_parts();
        debug_assert_eq!(
            layout.kind(),
            match &slot {
                PropertySlot::Data(_) => PropertyLayoutKind::Data,
                PropertySlot::Accessor { .. } => PropertyLayoutKind::Accessor,
            }
        );
        let shape = Arc::get_mut(&mut self.shape)
            .expect("object shape Arc is private and uniquely owned before shape interning");
        shape[index].layout = layout;
        self.slots[index] = slot;
        Some(previous)
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
        self.slots.push(PropertySlot::Data(value));
        Ok(())
    }

    pub(crate) fn append_accessor(
        &mut self,
        key: PropertyKey,
        layout: PropertyLayout,
        getter: Option<FunctionId>,
        setter: Option<FunctionId>,
    ) -> Result<(), TryReserveError> {
        debug_assert_eq!(layout.kind(), PropertyLayoutKind::Accessor);
        debug_assert!(self.shape.iter().all(|property| property.key != key));

        self.slots.try_reserve(1)?;
        let shape = Arc::get_mut(&mut self.shape)
            .expect("object shape Arc is private and uniquely owned before shape interning");
        shape.try_reserve(1)?;

        shape.push(ShapeProperty { key, layout });
        self.slots.push(PropertySlot::Accessor { getter, setter });
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
        if !matches!(self.slots.last(), Some(PropertySlot::Data(_))) {
            return None;
        }
        let shape = Arc::get_mut(&mut self.shape)
            .expect("object shape Arc is private and uniquely owned before shape interning");
        let property = shape.pop()?;
        debug_assert_eq!(property.layout.kind(), PropertyLayoutKind::Data);
        match self.slots.pop()? {
            PropertySlot::Data(value) => Some(value),
            PropertySlot::Accessor { .. } => {
                unreachable!("the last slot was checked as a data slot")
            }
        }
    }
}

#[allow(
    dead_code,
    reason = "all primitive wrapper payloads are defined together so later intrinsic families reuse one typed object representation"
)]
#[derive(Clone)]
pub(crate) enum BoxedPrimitive {
    Boolean(bool),
    Number(JsNumber),
    String(JsString),
    Symbol(Atom),
}

#[allow(
    dead_code,
    reason = "typed wrapper inspection lands before every primitive intrinsic consumes it"
)]
impl BoxedPrimitive {
    #[must_use]
    pub(crate) const fn as_boolean(&self) -> Option<bool> {
        match self {
            Self::Boolean(value) => Some(*value),
            Self::Number(_) | Self::String(_) | Self::Symbol(_) => None,
        }
    }

    #[must_use]
    pub(crate) const fn as_number(&self) -> Option<JsNumber> {
        match self {
            Self::Number(value) => Some(*value),
            Self::Boolean(_) | Self::String(_) | Self::Symbol(_) => None,
        }
    }

    #[must_use]
    pub(crate) const fn as_string(&self) -> Option<&JsString> {
        match self {
            Self::String(value) => Some(value),
            Self::Boolean(_) | Self::Number(_) | Self::Symbol(_) => None,
        }
    }

    #[must_use]
    pub(crate) fn string_code_unit_at(&self, index: u32) -> Option<u16> {
        self.as_string()?.code_unit_at(index)
    }

    #[must_use]
    pub(crate) const fn as_symbol(&self) -> Option<&Atom> {
        match self {
            Self::Symbol(value) => Some(value),
            Self::Boolean(_) | Self::Number(_) | Self::String(_) => None,
        }
    }
}

pub(crate) enum HeapObjectKind {
    Ordinary,
    BoxedPrimitive(BoxedPrimitive),
}

impl HeapObjectKind {
    #[must_use]
    pub(crate) const fn boxed_primitive(&self) -> Option<&BoxedPrimitive> {
        match self {
            Self::Ordinary => None,
            Self::BoxedPrimitive(value) => Some(value),
        }
    }
}

pub(crate) struct HeapObject {
    kind: HeapObjectKind,
    pub(crate) record: ObjectRecord,
    pub(crate) public_roots: u32,
}

impl HeapObject {
    #[must_use]
    pub(crate) const fn ordinary(record: ObjectRecord) -> Self {
        Self {
            kind: HeapObjectKind::Ordinary,
            record,
            public_roots: 0,
        }
    }

    #[must_use]
    pub(crate) const fn with_boxed_primitive(record: ObjectRecord, value: BoxedPrimitive) -> Self {
        Self {
            kind: HeapObjectKind::BoxedPrimitive(value),
            record,
            public_roots: 0,
        }
    }

    #[must_use]
    #[allow(
        dead_code,
        reason = "kind inspection supports class-sensitive object behavior beyond the first Boolean consumer"
    )]
    pub(crate) const fn kind(&self) -> &HeapObjectKind {
        &self.kind
    }

    #[must_use]
    pub(crate) const fn boxed_primitive(&self) -> Option<&BoxedPrimitive> {
        self.kind.boxed_primitive()
    }
}

#[cfg(test)]
mod tests {
    use super::{BoxedPrimitive, HeapObject, HeapObjectKind, ObjectRecord, OwnProperty};
    use crate::{
        ArrayIndex, AtomLimits, AtomTable, JsNumber, JsString, PredefinedAtom, PropertyKey,
        PropertyLayout,
        arena::{Arena, RuntimeIdentity},
        ids::FunctionMarker,
        value::StoredValue,
    };

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

    #[test]
    fn accessor_slots_have_typed_lookup_and_trace_both_function_edges() {
        let mut functions = Arena::<FunctionMarker, ()>::new(RuntimeIdentity::from_address(7));
        let getter = functions.try_insert(()).expect("getter");
        let setter = functions.try_insert(()).expect("setter");
        let key = PropertyKey::from_index(ArrayIndex::new(8).expect("array index"));
        let layout = PropertyLayout::accessor(true, false);
        let mut record = ObjectRecord::empty(None);

        record
            .append_accessor(key.clone(), layout, Some(getter), Some(setter))
            .expect("accessor");

        assert_eq!(record.property_count(), 1);
        assert!(record.own_data_property(&key).is_none());
        assert!(matches!(
            record.own_property(&key),
            Some(OwnProperty::Accessor {
                layout: actual,
                getter: Some(read_hook),
                setter: Some(write_hook),
            }) if actual == layout && read_hook == getter && write_hook == setter
        ));
        assert_eq!(record.values().count(), 0);
        assert_eq!(
            record.accessor_functions().collect::<Vec<_>>(),
            vec![getter, setter]
        );
    }

    #[test]
    fn accessor_slot_can_be_replaced_with_data_and_restored_without_count_change() {
        let mut functions = Arena::<FunctionMarker, ()>::new(RuntimeIdentity::from_address(9));
        let getter = functions.try_insert(()).expect("getter");
        let key = PropertyKey::from_index(ArrayIndex::new(9).expect("array index"));
        let accessor_layout = PropertyLayout::accessor(false, true);
        let data_layout = PropertyLayout::data(true, true, true);
        let mut record = ObjectRecord::empty(None);
        record
            .append_accessor(key.clone(), accessor_layout, Some(getter), None)
            .expect("accessor");

        let previous = record
            .replace_existing_with_data(&key, data_layout, StoredValue::Undefined)
            .expect("existing accessor");
        assert!(matches!(
            previous,
            OwnProperty::Accessor {
                layout,
                getter: Some(actual_getter),
                setter: None,
            } if layout == accessor_layout && actual_getter == getter
        ));
        assert!(matches!(
            record.own_property(&key),
            Some(OwnProperty::Data {
                layout,
                value: StoredValue::Undefined,
            }) if layout == data_layout
        ));

        let replaced = record
            .restore_existing_property(&key, previous)
            .expect("data replacement");
        assert!(matches!(
            replaced,
            OwnProperty::Data {
                layout,
                value: StoredValue::Undefined,
            } if layout == data_layout
        ));
        assert_eq!(record.property_count(), 1);
        assert!(matches!(
            record.own_property(&key),
            Some(OwnProperty::Accessor {
                layout,
                getter: Some(actual_getter),
                setter: None,
            }) if layout == accessor_layout && actual_getter == getter
        ));
    }

    #[test]
    fn accessor_halves_merge_by_replacing_one_typed_slot() {
        let mut functions = Arena::<FunctionMarker, ()>::new(RuntimeIdentity::from_address(10));
        let getter = functions.try_insert(()).expect("getter");
        let setter = functions.try_insert(()).expect("setter");
        let key = PropertyKey::from_index(ArrayIndex::new(10).expect("array index"));
        let original_layout = PropertyLayout::accessor(false, true);
        let replacement_layout = PropertyLayout::accessor(true, true);
        let mut record = ObjectRecord::empty(None);
        record
            .append_accessor(key.clone(), original_layout, Some(getter), None)
            .expect("getter");

        let previous = record
            .replace_existing_with_accessor(&key, replacement_layout, Some(getter), Some(setter))
            .expect("existing getter");

        assert!(matches!(
            previous,
            OwnProperty::Accessor {
                layout,
                getter: Some(actual_getter),
                setter: None,
            } if layout == original_layout && actual_getter == getter
        ));
        assert_eq!(record.property_count(), 1);
        assert!(matches!(
            record.own_property(&key),
            Some(OwnProperty::Accessor {
                layout,
                getter: Some(read_function),
                setter: Some(write_function),
            }) if layout == replacement_layout
                && read_function == getter
                && write_function == setter
        ));
    }

    #[test]
    fn ordinary_heap_object_has_no_boxed_primitive_payload() {
        let object = HeapObject::ordinary(ObjectRecord::empty(None));

        assert!(matches!(object.kind(), HeapObjectKind::Ordinary));
        assert!(object.boxed_primitive().is_none());
        assert_eq!(object.public_roots, 0);
    }

    #[test]
    fn boolean_wrapper_preserves_its_typed_payload() {
        let object = HeapObject::with_boxed_primitive(
            ObjectRecord::empty(None),
            BoxedPrimitive::Boolean(true),
        );

        assert!(matches!(
            object.kind(),
            HeapObjectKind::BoxedPrimitive(BoxedPrimitive::Boolean(true))
        ));
        let payload = object.boxed_primitive().expect("boxed primitive");
        assert_eq!(payload.as_boolean(), Some(true));
        assert!(payload.as_number().is_none());
        assert!(payload.as_string().is_none());
        assert!(payload.as_symbol().is_none());
    }

    #[test]
    fn primitive_wrapper_payload_variants_have_typed_read_only_accessors() {
        let number = BoxedPrimitive::Number(JsNumber::from_f64(-0.0));
        assert_eq!(
            number.as_number().expect("number").as_f64().to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(number.as_boolean(), None);

        let text = JsString::from_utf8("wrapper").expect("string");
        let string = BoxedPrimitive::String(text.clone());
        assert_eq!(string.as_string(), Some(&text));
        assert_eq!(
            string.string_code_unit_at(0),
            Some(u16::from(b'w')),
            "String exotic indexing reads UTF-16 code units from the branded payload"
        );
        assert_eq!(string.string_code_unit_at(text.len()), None);
        assert!(string.as_symbol().is_none());

        let atoms = AtomTable::new(AtomLimits::default()).expect("atom table");
        let iterator = atoms.predefined(PredefinedAtom::SymbolIterator);
        let symbol = BoxedPrimitive::Symbol(iterator.clone());
        assert!(
            symbol
                .as_symbol()
                .is_some_and(|payload| payload.is_same_identity(&iterator))
        );
        assert!(symbol.as_string().is_none());
    }
}
