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

use std::{
    collections::{HashSet, TryReserveError},
    sync::Arc,
};

use crate::{
    ArrayIndex, Atom, AtomKind, JsNumber, JsString, PropertyKey, PropertyLayout,
    PropertyLayoutKind,
    ids::FunctionId,
    value::{HeapReference, StoredValue},
};

#[derive(Clone)]
struct ShapeProperty {
    key: PropertyKey,
    layout: PropertyLayout,
}

#[derive(Clone)]
pub(crate) struct ForInCandidate {
    key: PropertyKey,
    enumerable: bool,
}

impl ForInCandidate {
    pub(crate) fn key(&self) -> &PropertyKey {
        &self.key
    }

    pub(crate) const fn enumerable(&self) -> bool {
        self.enumerable
    }
}

pub(crate) struct ForInSnapshot {
    candidates: Vec<ForInCandidate>,
    sort_work: u64,
}

impl ForInSnapshot {
    pub(crate) const fn empty() -> Self {
        Self {
            candidates: Vec::new(),
            sort_work: 0,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.candidates.len()
    }

    pub(crate) fn get(&self, index: usize) -> Option<&ForInCandidate> {
        self.candidates.get(index)
    }

    pub(crate) const fn sort_work(&self) -> u64 {
        self.sort_work
    }
}

pub(crate) struct ForInIterator {
    current: Option<HeapReference>,
    snapshot: ForInSnapshot,
    next: usize,
    visited: HashSet<PropertyKey>,
}

impl ForInIterator {
    pub(crate) fn new(current: Option<HeapReference>, snapshot: ForInSnapshot) -> Self {
        Self {
            current,
            snapshot,
            next: 0,
            visited: HashSet::new(),
        }
    }

    pub(crate) const fn current(&self) -> Option<HeapReference> {
        self.current
    }

    pub(crate) fn candidate(&self) -> Option<&ForInCandidate> {
        self.snapshot.get(self.next)
    }

    pub(crate) fn advance_candidate(&mut self) {
        self.next = self.next.saturating_add(1);
    }

    pub(crate) fn has_visited(&self, key: &PropertyKey) -> bool {
        self.visited.contains(key)
    }

    pub(crate) fn visited_growth_work(&self) -> u64 {
        if self.visited.len() < self.visited.capacity() {
            return 1;
        }
        u64::try_from(self.visited.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1)
    }

    pub(crate) fn try_mark_visited(&mut self, key: PropertyKey) -> Result<bool, TryReserveError> {
        if self.visited.contains(&key) {
            return Ok(false);
        }
        self.visited.try_reserve(1)?;
        Ok(self.visited.insert(key))
    }

    pub(crate) fn replace_current(
        &mut self,
        current: Option<HeapReference>,
        snapshot: ForInSnapshot,
    ) -> usize {
        let previous = std::mem::replace(&mut self.snapshot, snapshot);
        self.current = current;
        self.next = 0;
        previous.len()
    }

    pub(crate) fn entry_count(&self) -> usize {
        self.snapshot.len().saturating_add(self.visited.len())
    }

    pub(crate) fn snapshot_len(&self) -> usize {
        self.snapshot.len()
    }
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

    pub(crate) fn has_own_property_with_scan(&self, key: &PropertyKey) -> (bool, usize) {
        let mut scanned = 0_usize;
        for property in self.shape.iter() {
            scanned = scanned.saturating_add(1);
            if property.key == *key {
                return (true, scanned);
            }
        }
        (false, scanned)
    }

    pub(crate) fn for_in_candidate_count(&self, string_length: Option<u32>) -> usize {
        let virtual_indices = string_length.unwrap_or(0);
        let ordinary = self.shape.iter().filter(|property| {
            property
                .key
                .as_index()
                .is_some_and(|index| index.get() >= virtual_indices)
                || property
                    .key
                    .as_atom()
                    .is_some_and(|atom| atom.kind() == AtomKind::String)
        });
        usize::try_from(virtual_indices)
            .unwrap_or(usize::MAX)
            .saturating_add(ordinary.count())
    }

    pub(crate) fn try_for_in_snapshot(
        &self,
        string_length: Option<u32>,
    ) -> Result<ForInSnapshot, TryReserveError> {
        let virtual_indices = string_length.unwrap_or(0);
        let capacity = self.for_in_candidate_count(string_length);
        let mut candidates = Vec::new();
        candidates.try_reserve_exact(capacity)?;

        for index in 0..virtual_indices {
            candidates.push(ForInCandidate {
                key: PropertyKey::from_index(
                    ArrayIndex::new(index)
                        .expect("QuickJS String length cannot contain the non-index u32 maximum"),
                ),
                enumerable: true,
            });
        }
        for property in self.shape.iter() {
            if let Some(index) = property.key.as_index()
                && index.get() >= virtual_indices
            {
                candidates.push(ForInCandidate {
                    key: property.key.clone(),
                    enumerable: property.layout.is_enumerable(),
                });
            }
        }
        let sort_work = conservative_sort_work(candidates.len());
        candidates.sort_unstable_by_key(|candidate| {
            candidate
                .key
                .as_index()
                .expect("only array-index candidates precede the string-key phase")
        });
        for property in self.shape.iter() {
            if property
                .key
                .as_atom()
                .is_some_and(|atom| atom.kind() == AtomKind::String)
            {
                candidates.push(ForInCandidate {
                    key: property.key.clone(),
                    enumerable: property.layout.is_enumerable(),
                });
            }
        }
        Ok(ForInSnapshot {
            candidates,
            sort_work,
        })
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

fn conservative_sort_work(entries: usize) -> u64 {
    let entries = u64::try_from(entries).unwrap_or(u64::MAX);
    if entries <= 1 {
        return 0;
    }
    let levels = u64::from(u64::BITS - (entries - 1).leading_zeros());
    entries.saturating_mul(levels).saturating_mul(2)
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
    Error,
    BoxedPrimitive(BoxedPrimitive),
    ForInIterator(ForInIterator),
}

impl HeapObjectKind {
    #[must_use]
    pub(crate) const fn boxed_primitive(&self) -> Option<&BoxedPrimitive> {
        match self {
            Self::Ordinary | Self::Error | Self::ForInIterator(_) => None,
            Self::BoxedPrimitive(value) => Some(value),
        }
    }

    pub(crate) const fn for_in_iterator(&self) -> Option<&ForInIterator> {
        match self {
            Self::ForInIterator(iterator) => Some(iterator),
            Self::Ordinary | Self::Error | Self::BoxedPrimitive(_) => None,
        }
    }

    pub(crate) const fn for_in_iterator_mut(&mut self) -> Option<&mut ForInIterator> {
        match self {
            Self::ForInIterator(iterator) => Some(iterator),
            Self::Ordinary | Self::Error | Self::BoxedPrimitive(_) => None,
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
    pub(crate) const fn error(record: ObjectRecord) -> Self {
        Self {
            kind: HeapObjectKind::Error,
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
    pub(crate) fn for_in_iterator(iterator: ForInIterator) -> Self {
        Self {
            kind: HeapObjectKind::ForInIterator(iterator),
            record: ObjectRecord::empty(None),
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
    pub(crate) const fn is_error(&self) -> bool {
        matches!(self.kind, HeapObjectKind::Error)
    }

    #[must_use]
    pub(crate) const fn boxed_primitive(&self) -> Option<&BoxedPrimitive> {
        self.kind.boxed_primitive()
    }

    #[must_use]
    pub(crate) const fn for_in_state(&self) -> Option<&ForInIterator> {
        self.kind.for_in_iterator()
    }

    #[must_use]
    pub(crate) const fn for_in_state_mut(&mut self) -> Option<&mut ForInIterator> {
        self.kind.for_in_iterator_mut()
    }

    #[must_use]
    pub(crate) fn for_in_entry_count(&self) -> usize {
        self.for_in_state().map_or(0, ForInIterator::entry_count)
    }

    #[must_use]
    pub(crate) const fn for_in_current(&self) -> Option<HeapReference> {
        match self.kind.for_in_iterator() {
            Some(iterator) => iterator.current(),
            None => None,
        }
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
    fn for_in_snapshot_orders_indices_before_inserted_strings_and_excludes_symbols() {
        let mut atoms = AtomTable::try_new(AtomLimits::default()).expect("atoms");
        let mut record = ObjectRecord::empty(None);
        let string_key = |atoms: &mut AtomTable, text: &str| {
            atoms
                .property_key_from_string(&JsString::from_utf8(text).expect("string"))
                .expect("property key")
        };

        record
            .append_data(
                string_key(&mut atoms, "b"),
                PropertyLayout::data(true, true, true),
                StoredValue::Undefined,
            )
            .expect("b");
        record
            .append_data(
                PropertyKey::from_index(ArrayIndex::new(5).expect("index")),
                PropertyLayout::data(true, true, true),
                StoredValue::Undefined,
            )
            .expect("5");
        record
            .append_data(
                PropertyKey::from_index(ArrayIndex::new(2).expect("index")),
                PropertyLayout::data(true, false, true),
                StoredValue::Undefined,
            )
            .expect("2");
        record
            .append_data(
                string_key(&mut atoms, "a"),
                PropertyLayout::data(true, true, true),
                StoredValue::Undefined,
            )
            .expect("a");
        let symbol_description = JsString::from_utf8("hidden").expect("description");
        let symbol = atoms
            .new_unique_symbol(Some(&symbol_description))
            .expect("symbol");
        record
            .append_data(
                atoms.property_key_from_symbol(&symbol).expect("symbol key"),
                PropertyLayout::data(true, true, true),
                StoredValue::Undefined,
            )
            .expect("symbol");

        let snapshot = record.try_for_in_snapshot(Some(2)).expect("snapshot");
        let keys = (0..snapshot.len())
            .map(|index| {
                let candidate = snapshot.get(index).expect("candidate");
                let name = candidate.key().as_index().map_or_else(
                    || {
                        candidate
                            .key()
                            .as_atom()
                            .and_then(crate::Atom::description)
                            .expect("string atom")
                            .to_utf8_lossy()
                            .expect("UTF-8")
                    },
                    |index| index.get().to_string(),
                );
                (name, candidate.enumerable())
            })
            .collect::<Vec<_>>();

        assert_eq!(
            keys,
            vec![
                ("0".to_owned(), true),
                ("1".to_owned(), true),
                ("2".to_owned(), false),
                ("5".to_owned(), true),
                ("b".to_owned(), true),
                ("a".to_owned(), true),
            ]
        );
        assert_eq!(snapshot.sort_work(), 16);
    }

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
        assert!(!object.is_error());
        assert!(object.boxed_primitive().is_none());
        assert_eq!(object.public_roots, 0);
    }

    #[test]
    fn error_heap_object_preserves_its_internal_brand() {
        let object = HeapObject::error(ObjectRecord::empty(None));

        assert!(matches!(object.kind(), HeapObjectKind::Error));
        assert!(object.is_error());
        assert!(object.boxed_primitive().is_none());
        assert!(object.for_in_state().is_none());
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
