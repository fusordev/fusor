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
    collections::{HashMap, HashSet, TryReserveError},
    hash::{Hash, Hasher},
    sync::Arc,
};

use crate::ids::ObjectId;
use crate::{
    ArrayIndex, Atom, AtomKind, JsBigInt, JsNumber, JsString, PropertyKey, PropertyLayout,
    PropertyLayoutKind,
    atom::WeakAtom,
    ids::{BindingCellId, FunctionId, RealmId},
    value::{HeapReference, StoredValue},
};

/// A key with ECMAScript language identity that does not keep that identity
/// alive.
#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) enum WeakKey {
    Symbol(WeakAtom),
    Function(FunctionId),
    Object(ObjectId),
}

impl WeakKey {
    pub(crate) fn from_value(value: &StoredValue) -> Option<Self> {
        match value {
            StoredValue::Symbol(symbol) if symbol.kind() == AtomKind::Symbol => {
                Some(Self::Symbol(WeakAtom::from_atom(symbol)))
            }
            StoredValue::Function(function) => Some(Self::Function(*function)),
            StoredValue::Object(object) => Some(Self::Object(*object)),
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_) => None,
        }
    }

    pub(crate) const fn heap_reference(&self) -> Option<HeapReference> {
        match self {
            Self::Symbol(_) => None,
            Self::Function(function) => Some(HeapReference::Function(*function)),
            Self::Object(object) => Some(HeapReference::Object(*object)),
        }
    }

    pub(crate) const fn symbol(&self) -> Option<&WeakAtom> {
        match self {
            Self::Symbol(symbol) => Some(symbol),
            Self::Function(_) | Self::Object(_) => None,
        }
    }
}

#[derive(Clone)]
struct ShapeProperty {
    key: PropertyKey,
    layout: PropertyLayout,
}

/// The outcome of removing one own property.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PropertyDeletion {
    /// The object had no own property with the requested key.
    Missing,
    /// The own property exists but forbids reconfiguration.
    NotConfigurable,
    /// The own property was removed.
    Deleted,
}

/// Which `[[OwnPropertyKeys]]` phases an own-key snapshot emits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct KeyPhases {
    indices: bool,
    strings: bool,
    symbols: bool,
}

impl KeyPhases {
    /// The index and string phases, which is what `for-in` enumerates.
    ///
    /// `for-in` never visits symbol keys, so the symbol phase is excluded
    /// rather than filtered by the caller.
    pub(crate) const FOR_IN: Self = Self {
        indices: true,
        strings: true,
        symbols: false,
    };

    /// The index and string phases, which is what `Object.keys`,
    /// `Object.getOwnPropertyNames`, and `JSON.stringify` observe.
    pub(crate) const STRING_KEYS: Self = Self {
        indices: true,
        strings: true,
        symbols: false,
    };

    /// The symbol phase only, which is what
    /// `Object.getOwnPropertySymbols` projects.
    pub(crate) const SYMBOL_KEYS: Self = Self {
        indices: false,
        strings: false,
        symbols: true,
    };

    /// All three `[[OwnPropertyKeys]]` phases, used by `Reflect.ownKeys`.
    pub(crate) const ALL: Self = Self {
        indices: true,
        strings: true,
        symbols: true,
    };
}

/// Which atom-keyed phase `push_atom_keys` appends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AtomKeyPhase {
    String,
    Symbol,
}

/// The integrity level `Object.seal` and `Object.freeze` apply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IntegrityLevel {
    /// Every own property becomes non-configurable.
    Sealed,
    /// Every own property becomes non-configurable, and every data property
    /// also becomes non-writable.
    Frozen,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArrayIteratorKind {
    Key,
    Value,
    KeyAndValue,
}

pub(crate) struct ArrayIterator {
    iterated: Option<StoredValue>,
    kind: ArrayIteratorKind,
    next: u32,
}

impl ArrayIterator {
    pub(crate) const fn new(iterated: StoredValue, kind: ArrayIteratorKind) -> Self {
        Self {
            iterated: Some(iterated),
            kind,
            next: 0,
        }
    }

    pub(crate) const fn iterated(&self) -> Option<&StoredValue> {
        self.iterated.as_ref()
    }

    pub(crate) const fn kind(&self) -> ArrayIteratorKind {
        self.kind
    }

    pub(crate) const fn next(&self) -> u32 {
        self.next
    }

    pub(crate) fn advance(&mut self) {
        self.next = self.next.saturating_add(1);
    }

    pub(crate) fn finish(&mut self) {
        self.iterated = None;
    }
}

pub(crate) struct StringIterator {
    iterated: Option<JsString>,
    next: u32,
}

/// Hashable `SameValueZero` projection used by `Map`'s lookup index.
///
/// The ordered entry vector remains the semantic source of truth. This key is
/// deliberately private to the index so hash-table layout cannot affect
/// observable iteration order.
#[derive(Clone, Eq, PartialEq)]
enum MapKey {
    Undefined,
    Null,
    Boolean(bool),
    Number(u64),
    BigInt(Arc<JsBigInt>),
    String(JsString),
    Symbol(Atom),
    Function(FunctionId),
    Object(ObjectId),
}

impl MapKey {
    fn from_value(value: &StoredValue) -> Self {
        match value {
            StoredValue::Undefined => Self::Undefined,
            StoredValue::Null => Self::Null,
            StoredValue::Boolean(value) => Self::Boolean(*value),
            StoredValue::Number(value) => {
                let value = value.as_f64();
                let bits = if value == 0.0 {
                    0_f64.to_bits()
                } else if value.is_nan() {
                    f64::NAN.to_bits()
                } else {
                    value.to_bits()
                };
                Self::Number(bits)
            }
            StoredValue::BigInt(value) => Self::BigInt(Arc::clone(value)),
            StoredValue::String(value) => Self::String(value.clone()),
            StoredValue::Symbol(value) => Self::Symbol(value.clone()),
            StoredValue::Function(value) => Self::Function(*value),
            StoredValue::Object(value) => Self::Object(*value),
        }
    }
}

impl Hash for MapKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        core::mem::discriminant(self).hash(state);
        match self {
            Self::Undefined | Self::Null => {}
            Self::Boolean(value) => value.hash(state),
            Self::Number(value) => value.hash(state),
            Self::BigInt(value) => value.hash(state),
            Self::String(value) => value.hash(state),
            Self::Symbol(value) => value.hash(state),
            Self::Function(value) => value.hash(state),
            Self::Object(value) => value.hash(state),
        }
    }
}

pub(crate) struct MapEntry {
    key: StoredValue,
    value: StoredValue,
    live: bool,
}

impl MapEntry {
    pub(crate) const fn key(&self) -> &StoredValue {
        &self.key
    }

    pub(crate) const fn value(&self) -> &StoredValue {
        &self.value
    }

    pub(crate) const fn is_live(&self) -> bool {
        self.live
    }
}

/// Ordered `[[MapData]]` plus an average-sublinear `SameValueZero` index.
///
/// Deletion leaves a tombstone in `entries`; this is what lets active Map
/// iterators observe later appends without snapshots or iterator registries.
pub(crate) struct MapState {
    entries: Vec<MapEntry>,
    index: HashMap<MapKey, usize>,
    live: usize,
}

pub(crate) enum MapSetOutcome {
    Inserted,
    Updated,
}

impl MapState {
    pub(crate) fn empty() -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::new(),
            live: 0,
        }
    }

    pub(crate) const fn len(&self) -> usize {
        self.live
    }

    pub(crate) const fn retained_len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn entry(&self, index: usize) -> Option<&MapEntry> {
        self.entries.get(index)
    }

    pub(crate) fn retained_values(&self) -> impl Iterator<Item = &StoredValue> {
        self.entries
            .iter()
            .flat_map(|entry| [&entry.key, &entry.value])
    }

    pub(crate) fn get(&self, key: &StoredValue) -> Option<&StoredValue> {
        self.index
            .get(&MapKey::from_value(key))
            .and_then(|index| self.entries.get(*index))
            .filter(|entry| entry.live)
            .map(MapEntry::value)
    }

    pub(crate) fn contains_key(&self, key: &StoredValue) -> bool {
        self.index.contains_key(&MapKey::from_value(key))
    }

    pub(crate) fn try_set(
        &mut self,
        key: StoredValue,
        value: StoredValue,
    ) -> Result<MapSetOutcome, TryReserveError> {
        let index_key = MapKey::from_value(&key);
        if let Some(index) = self.index.get(&index_key).copied() {
            if let Some(entry) = self.entries.get_mut(index) {
                entry.value = value;
                return Ok(MapSetOutcome::Updated);
            }
            debug_assert!(false, "Map lookup index must name an entry");
        }
        self.entries.try_reserve(1)?;
        self.index.try_reserve(1)?;
        let index = self.entries.len();
        self.entries.push(MapEntry {
            key,
            value,
            live: true,
        });
        self.index.insert(index_key, index);
        self.live = self.live.saturating_add(1);
        Ok(MapSetOutcome::Inserted)
    }

    pub(crate) fn delete(&mut self, key: &StoredValue) -> bool {
        let Some(index) = self.index.remove(&MapKey::from_value(key)) else {
            return false;
        };
        let Some(entry) = self.entries.get_mut(index) else {
            debug_assert!(false, "Map lookup index must name an entry");
            return false;
        };
        entry.key = StoredValue::Undefined;
        entry.value = StoredValue::Undefined;
        entry.live = false;
        self.live = self.live.saturating_sub(1);
        true
    }

    pub(crate) fn clear(&mut self) {
        for entry in &mut self.entries {
            entry.key = StoredValue::Undefined;
            entry.value = StoredValue::Undefined;
            entry.live = false;
        }
        self.index.clear();
        self.live = 0;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MapIteratorKind {
    Key,
    Value,
    KeyAndValue,
}

pub(crate) struct MapIterator {
    iterated: Option<ObjectId>,
    kind: MapIteratorKind,
    next: usize,
}

/// Ordered `[[SetData]]` backed by the same audited `SameValueZero` table as
/// `Map`, while retaining a distinct heap brand and exposing values only.
pub(crate) struct SetState {
    data: MapState,
}

impl SetState {
    pub(crate) fn empty() -> Self {
        Self {
            data: MapState::empty(),
        }
    }

    pub(crate) fn try_with_capacity(capacity: usize) -> Result<Self, TryReserveError> {
        let mut entries = Vec::new();
        entries.try_reserve_exact(capacity)?;
        let mut index = HashMap::new();
        index.try_reserve(capacity)?;
        Ok(Self {
            data: MapState {
                entries,
                index,
                live: 0,
            },
        })
    }

    pub(crate) const fn len(&self) -> usize {
        self.data.len()
    }

    pub(crate) const fn retained_len(&self) -> usize {
        self.data.retained_len()
    }

    pub(crate) fn entry(&self, index: usize) -> Option<&MapEntry> {
        self.data.entry(index)
    }

    pub(crate) fn retained_values(&self) -> impl Iterator<Item = &StoredValue> {
        self.data
            .entries
            .iter()
            .filter(|entry| entry.live)
            .map(|entry| &entry.key)
    }

    pub(crate) fn contains(&self, value: &StoredValue) -> bool {
        self.data.contains_key(value)
    }

    pub(crate) fn try_add(&mut self, value: StoredValue) -> Result<MapSetOutcome, TryReserveError> {
        self.data.try_set(value, StoredValue::Undefined)
    }

    pub(crate) fn delete(&mut self, value: &StoredValue) -> bool {
        self.data.delete(value)
    }

    pub(crate) fn clear(&mut self) {
        self.data.clear();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SetIteratorKind {
    Value,
    KeyAndValue,
}

pub(crate) struct SetIterator {
    iterated: Option<ObjectId>,
    kind: SetIteratorKind,
    next: usize,
}

/// Non-enumerable `[[WeakMapData]]` indexed by non-owning language identities.
pub(crate) struct WeakMapState {
    entries: HashMap<WeakKey, StoredValue>,
}

impl WeakMapState {
    pub(crate) fn empty() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn get(&self, key: &StoredValue) -> Option<&StoredValue> {
        WeakKey::from_value(key).and_then(|key| self.entries.get(&key))
    }

    pub(crate) fn contains_key(&self, key: &StoredValue) -> bool {
        WeakKey::from_value(key).is_some_and(|key| self.entries.contains_key(&key))
    }

    pub(crate) fn try_set(
        &mut self,
        key: &StoredValue,
        value: StoredValue,
    ) -> Result<MapSetOutcome, TryReserveError> {
        let key = WeakKey::from_value(key).expect("WeakMap keys are validated before storage");
        if let Some(entry) = self.entries.get_mut(&key) {
            *entry = value;
            return Ok(MapSetOutcome::Updated);
        }
        self.entries.try_reserve(1)?;
        self.entries.insert(key, value);
        Ok(MapSetOutcome::Inserted)
    }

    pub(crate) fn delete(&mut self, key: &StoredValue) -> bool {
        WeakKey::from_value(key).is_some_and(|key| self.entries.remove(&key).is_some())
    }

    pub(crate) fn ephemeron_entries(&self) -> impl Iterator<Item = (&WeakKey, &StoredValue)> {
        self.entries.iter()
    }

    pub(crate) fn retain_keys(&mut self, mut keep: impl FnMut(&WeakKey) -> bool) -> usize {
        let previous = self.entries.len();
        self.entries.retain(|key, _| keep(key));
        previous.saturating_sub(self.entries.len())
    }
}

/// Non-enumerable `[[WeakSetData]]` indexed by non-owning language identities.
pub(crate) struct WeakSetState {
    entries: HashSet<WeakKey>,
}

/// The non-owning `[[WeakRefTarget]]` slot of one `WeakRef` instance.
pub(crate) struct WeakRefState {
    target: Option<WeakKey>,
}

impl WeakRefState {
    pub(crate) fn new(target: &StoredValue) -> Self {
        Self {
            target: Some(WeakKey::from_value(target).expect("WeakRef targets are validated")),
        }
    }

    pub(crate) const fn target(&self) -> Option<&WeakKey> {
        self.target.as_ref()
    }

    pub(crate) fn clear(&mut self) {
        self.target = None;
    }
}

/// One `FinalizationRegistry` cell. Targets and unregister tokens are weak;
/// held values remain strong until unregister or cleanup removes the cell.
pub(crate) struct FinalizationCell {
    target: Option<WeakKey>,
    held_value: StoredValue,
    unregister_token: Option<WeakKey>,
}

impl FinalizationCell {
    pub(crate) const fn target(&self) -> Option<&WeakKey> {
        self.target.as_ref()
    }

    pub(crate) const fn unregister_token(&self) -> Option<&WeakKey> {
        self.unregister_token.as_ref()
    }

    pub(crate) fn clear_target(&mut self) {
        self.target = None;
    }

    pub(crate) fn clear_unregister_token(&mut self) {
        self.unregister_token = None;
    }
}

/// `[[Realm]]`, `[[CleanupCallback]]`, and ordered `[[Cells]]` for one
/// `FinalizationRegistry` instance.
pub(crate) struct FinalizationRegistryState {
    realm: RealmId,
    cleanup_callback: FunctionId,
    cells: Vec<FinalizationCell>,
    cleanup_pending: bool,
}

impl FinalizationRegistryState {
    pub(crate) const fn new(realm: RealmId, cleanup_callback: FunctionId) -> Self {
        Self {
            realm,
            cleanup_callback,
            cells: Vec::new(),
            cleanup_pending: false,
        }
    }

    pub(crate) const fn realm(&self) -> RealmId {
        self.realm
    }

    pub(crate) const fn cleanup_callback(&self) -> FunctionId {
        self.cleanup_callback
    }

    pub(crate) const fn cleanup_pending(&self) -> bool {
        self.cleanup_pending
    }

    pub(crate) fn set_cleanup_pending(&mut self, pending: bool) {
        self.cleanup_pending = pending;
    }

    pub(crate) fn len(&self) -> usize {
        self.cells.len()
    }

    pub(crate) fn cells(&self) -> impl Iterator<Item = &FinalizationCell> {
        self.cells.iter()
    }

    pub(crate) fn cells_mut(&mut self) -> impl Iterator<Item = &mut FinalizationCell> {
        self.cells.iter_mut()
    }

    pub(crate) fn held_values(&self) -> impl Iterator<Item = &StoredValue> {
        self.cells.iter().map(|cell| &cell.held_value)
    }

    pub(crate) fn try_register(
        &mut self,
        target: &StoredValue,
        held_value: StoredValue,
        unregister_token: Option<&StoredValue>,
    ) -> Result<(), TryReserveError> {
        self.cells.try_reserve(1)?;
        self.cells.push(FinalizationCell {
            target: Some(
                WeakKey::from_value(target).expect("FinalizationRegistry targets are validated"),
            ),
            held_value,
            unregister_token: unregister_token.map(|token| {
                WeakKey::from_value(token)
                    .expect("FinalizationRegistry unregister tokens are validated")
            }),
        });
        Ok(())
    }

    pub(crate) fn unregister(&mut self, token: &StoredValue) -> usize {
        let token = WeakKey::from_value(token)
            .expect("FinalizationRegistry unregister tokens are validated");
        let previous = self.cells.len();
        self.cells
            .retain(|cell| cell.unregister_token.as_ref() != Some(&token));
        previous.saturating_sub(self.cells.len())
    }

    pub(crate) fn take_cleanup_value(&mut self) -> Option<StoredValue> {
        let position = self.cells.iter().position(|cell| cell.target.is_none())?;
        Some(self.cells.remove(position).held_value)
    }

    pub(crate) fn has_cleanup_cell(&self) -> bool {
        self.cells.iter().any(|cell| cell.target.is_none())
    }
}

impl WeakSetState {
    pub(crate) fn empty() -> Self {
        Self {
            entries: HashSet::new(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn contains(&self, value: &StoredValue) -> bool {
        WeakKey::from_value(value).is_some_and(|key| self.entries.contains(&key))
    }

    pub(crate) fn try_add(
        &mut self,
        value: &StoredValue,
    ) -> Result<MapSetOutcome, TryReserveError> {
        let key = WeakKey::from_value(value).expect("WeakSet values are validated before storage");
        if self.entries.contains(&key) {
            return Ok(MapSetOutcome::Updated);
        }
        self.entries.try_reserve(1)?;
        self.entries.insert(key);
        Ok(MapSetOutcome::Inserted)
    }

    pub(crate) fn delete(&mut self, value: &StoredValue) -> bool {
        WeakKey::from_value(value).is_some_and(|key| self.entries.remove(&key))
    }

    pub(crate) fn retain_keys(&mut self, mut keep: impl FnMut(&WeakKey) -> bool) -> usize {
        let previous = self.entries.len();
        self.entries.retain(|key| keep(key));
        previous.saturating_sub(self.entries.len())
    }
}

impl SetIterator {
    pub(crate) const fn new(iterated: ObjectId, kind: SetIteratorKind) -> Self {
        Self {
            iterated: Some(iterated),
            kind,
            next: 0,
        }
    }

    pub(crate) const fn iterated(&self) -> Option<ObjectId> {
        self.iterated
    }

    pub(crate) const fn kind(&self) -> SetIteratorKind {
        self.kind
    }

    pub(crate) const fn next(&self) -> usize {
        self.next
    }

    pub(crate) fn advance(&mut self) {
        self.next = self.next.saturating_add(1);
    }

    pub(crate) fn finish(&mut self) {
        self.iterated = None;
    }
}

impl MapIterator {
    pub(crate) const fn new(iterated: ObjectId, kind: MapIteratorKind) -> Self {
        Self {
            iterated: Some(iterated),
            kind,
            next: 0,
        }
    }

    pub(crate) const fn iterated(&self) -> Option<ObjectId> {
        self.iterated
    }

    pub(crate) const fn kind(&self) -> MapIteratorKind {
        self.kind
    }

    pub(crate) const fn next(&self) -> usize {
        self.next
    }

    pub(crate) fn advance(&mut self) {
        self.next = self.next.saturating_add(1);
    }

    pub(crate) fn finish(&mut self) {
        self.iterated = None;
    }
}

impl StringIterator {
    pub(crate) const fn new(iterated: JsString) -> Self {
        Self {
            iterated: Some(iterated),
            next: 0,
        }
    }

    pub(crate) const fn iterated(&self) -> Option<&JsString> {
        self.iterated.as_ref()
    }

    pub(crate) const fn next(&self) -> u32 {
        self.next
    }

    pub(crate) fn set_next(&mut self, next: u32) {
        self.next = next;
    }

    pub(crate) fn finish(&mut self) {
        self.iterated = None;
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ArrayTruncation {
    final_length: u32,
    blocked_index: Option<ArrayIndex>,
    removed: usize,
}

impl ArrayTruncation {
    pub(crate) const fn final_length(self) -> u32 {
        self.final_length
    }

    pub(crate) const fn blocked_index(self) -> Option<ArrayIndex> {
        self.blocked_index
    }

    pub(crate) const fn removed(self) -> usize {
        self.removed
    }
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

    /// Clears the extensible bit, matching `QuickJS`'s `JS_PreventExtensions`
    /// (`quickjs.c:8923`). The operation is idempotent and never restores
    /// extensibility: ECMAScript has no `[[PreventExtensions]]` inverse.
    pub(crate) const fn prevent_extensions(&mut self) {
        self.extensible = false;
    }

    /// Returns whether every own property forbids reconfiguration, which is
    /// the own-property half of `Object.isSealed`.
    pub(crate) fn own_properties_are_sealed(&self) -> bool {
        self.shape
            .iter()
            .all(|property| !property.layout.is_configurable())
    }

    /// Returns whether every own property forbids reconfiguration and every
    /// data property forbids assignment, the own-property half of
    /// `Object.isFrozen`.
    pub(crate) fn own_properties_are_frozen(&self) -> bool {
        self.shape.iter().all(|property| {
            !property.layout.is_configurable() && property.layout.writable() != Some(true)
        })
    }

    /// Applies `Object.seal`'s attribute clamp to every own property.
    pub(crate) fn seal_own_properties(&mut self) {
        let shape = Arc::get_mut(&mut self.shape)
            .expect("object shape Arc is private and uniquely owned before shape interning");
        for property in shape.iter_mut() {
            property.layout = property.layout.sealed();
        }
    }

    /// Applies `Object.freeze`'s attribute clamp to every own property.
    ///
    /// Accessor properties keep their getter and setter; only their
    /// `configurable` attribute is cleared, matching ECMAScript's
    /// `SetIntegrityLevel` and `QuickJS`'s `js_object_seal` (`quickjs.c:40549`).
    pub(crate) fn freeze_own_properties(&mut self) {
        let shape = Arc::get_mut(&mut self.shape)
            .expect("object shape Arc is private and uniquely owned before shape interning");
        for property in shape.iter_mut() {
            property.layout = property.layout.frozen();
        }
    }

    /// Removes one own property, compacting the shape and slot vectors in
    /// lockstep so their indices stay aligned.
    ///
    /// Returns [`PropertyDeletion::Missing`] when the key is absent,
    /// [`PropertyDeletion::NotConfigurable`] when the property forbids
    /// deletion, and [`PropertyDeletion::Deleted`] after removal. Deletion
    /// preserves the relative order of the surviving properties, which
    /// `[[OwnPropertyKeys]]` observes for string and symbol keys.
    pub(crate) fn delete_own_property(&mut self, key: &PropertyKey) -> PropertyDeletion {
        let Some(index) = self.shape.iter().position(|property| property.key == *key) else {
            return PropertyDeletion::Missing;
        };
        if !self.shape[index].layout.is_configurable() {
            return PropertyDeletion::NotConfigurable;
        }
        let shape = Arc::get_mut(&mut self.shape)
            .expect("object shape Arc is private and uniquely owned before shape interning");
        shape.remove(index);
        self.slots.remove(index);
        PropertyDeletion::Deleted
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

    /// Counts the keys `try_own_key_snapshot` would emit for the same arguments.
    pub(crate) fn own_key_candidate_count(
        &self,
        string_length: Option<u32>,
        phases: KeyPhases,
    ) -> usize {
        let virtual_indices = if phases.indices {
            string_length.unwrap_or(0)
        } else {
            0
        };
        let ordinary = self.shape.iter().filter(|property| {
            if let Some(index) = property.key.as_index() {
                return phases.indices && index.get() >= virtual_indices;
            }
            property
                .key
                .as_atom()
                .is_some_and(|atom| match atom.kind() {
                    AtomKind::String => phases.strings,
                    AtomKind::Symbol | AtomKind::GlobalSymbol => phases.symbols,
                    // Private names are not property keys; `[[OwnPropertyKeys]]`
                    // never reports them.
                    AtomKind::Private => false,
                })
        });
        usize::try_from(virtual_indices)
            .unwrap_or(usize::MAX)
            .saturating_add(ordinary.count())
    }

    /// Builds an ordered own-key snapshot.
    ///
    /// Keys are emitted in ECMAScript `[[OwnPropertyKeys]]` order: array
    /// indices in ascending numeric order, then string keys in property
    /// creation order, then symbol keys in property creation order. Each phase
    /// can be excluded, which lets `for-in` reuse the operation while keeping
    /// its own index-plus-string projection. `string_length` synthesizes the
    /// virtual indices of a boxed `String` wrapper ahead of its own indices.
    pub(crate) fn try_own_key_snapshot(
        &self,
        string_length: Option<u32>,
        phases: KeyPhases,
    ) -> Result<ForInSnapshot, TryReserveError> {
        let virtual_indices = if phases.indices {
            string_length.unwrap_or(0)
        } else {
            0
        };
        let capacity = self.own_key_candidate_count(string_length, phases);
        let mut candidates = Vec::new();
        candidates.try_reserve_exact(capacity)?;

        let mut sort_work = 0;
        if phases.indices {
            for index in 0..virtual_indices {
                candidates.push(ForInCandidate {
                    key: PropertyKey::from_index(
                        ArrayIndex::new(index).expect(
                            "QuickJS String length cannot contain the non-index u32 maximum",
                        ),
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
            sort_work = conservative_sort_work(candidates.len());
            candidates.sort_unstable_by_key(|candidate| {
                candidate
                    .key
                    .as_index()
                    .expect("only array-index candidates precede the string-key phase")
            });
        }
        if phases.strings {
            self.push_atom_keys(&mut candidates, AtomKeyPhase::String);
        }
        if phases.symbols {
            self.push_atom_keys(&mut candidates, AtomKeyPhase::Symbol);
        }
        Ok(ForInSnapshot {
            candidates,
            sort_work,
        })
    }

    /// Appends one atom-keyed phase in property creation order.
    fn push_atom_keys(&self, candidates: &mut Vec<ForInCandidate>, phase: AtomKeyPhase) {
        for property in self.shape.iter() {
            let matches = property.key.as_atom().is_some_and(|atom| {
                let kind = atom.kind();
                match phase {
                    AtomKeyPhase::String => kind == AtomKind::String,
                    AtomKeyPhase::Symbol => {
                        matches!(kind, AtomKind::Symbol | AtomKind::GlobalSymbol)
                    }
                }
            });
            if matches {
                candidates.push(ForInCandidate {
                    key: property.key.clone(),
                    enumerable: property.layout.is_enumerable(),
                });
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

    pub(crate) fn append_dense_array_data(
        &mut self,
        elements: Vec<StoredValue>,
    ) -> Result<(), TryReserveError> {
        self.try_reserve_data(elements.len())?;
        let shape = Arc::get_mut(&mut self.shape)
            .expect("object shape Arc is private and uniquely owned before shape interning");
        let start = shape
            .iter()
            .filter_map(|property| property.key.as_index())
            .map(ArrayIndex::get)
            .max()
            .map_or(0, |index| index.saturating_add(1));
        debug_assert_eq!(start, 0, "dense array construction starts without indices");
        for (offset, value) in elements.into_iter().enumerate() {
            let index = u32::try_from(offset)
                .expect("the caller preflights dense array length into the u32 domain");
            shape.push(ShapeProperty {
                key: PropertyKey::from_index(
                    ArrayIndex::new(index)
                        .expect("dense array construction never emits the length sentinel"),
                ),
                layout: PropertyLayout::data(true, true, true),
            });
            self.slots.push(PropertySlot::Data(value));
        }
        Ok(())
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

    pub(crate) fn truncate_array_indices(&mut self, requested_length: u32) -> ArrayTruncation {
        let blocked_index = self
            .shape
            .iter()
            .filter_map(|property| {
                let index = property.key.as_index()?;
                (index.get() >= requested_length && !property.layout.is_configurable())
                    .then_some(index)
            })
            .max();
        let final_length =
            blocked_index.map_or(requested_length, |index| index.get().saturating_add(1));
        let shape = Arc::get_mut(&mut self.shape)
            .expect("object shape Arc is private and uniquely owned before shape interning");
        let original_length = shape.len();
        let mut retained = 0_usize;
        for current in 0..original_length {
            let remove = shape[current]
                .key
                .as_index()
                .is_some_and(|index| index.get() >= final_length);
            if remove {
                debug_assert!(shape[current].layout.is_configurable());
            } else {
                if retained != current {
                    shape.swap(retained, current);
                    self.slots.swap(retained, current);
                }
                retained = retained.saturating_add(1);
            }
        }
        shape.truncate(retained);
        self.slots.truncate(retained);
        let removed = original_length.saturating_sub(retained);
        ArrayTruncation {
            final_length,
            blocked_index,
            removed,
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
    BigInt(Arc<JsBigInt>),
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
            Self::Number(_) | Self::BigInt(_) | Self::String(_) | Self::Symbol(_) => None,
        }
    }

    #[must_use]
    pub(crate) const fn as_number(&self) -> Option<JsNumber> {
        match self {
            Self::Number(value) => Some(*value),
            Self::Boolean(_) | Self::BigInt(_) | Self::String(_) | Self::Symbol(_) => None,
        }
    }

    /// Returns the wrapped `BigInt`, or `None` for another payload.
    #[must_use]
    pub(crate) const fn as_bigint(&self) -> Option<&Arc<JsBigInt>> {
        match self {
            Self::BigInt(value) => Some(value),
            Self::Boolean(_) | Self::Number(_) | Self::String(_) | Self::Symbol(_) => None,
        }
    }

    #[must_use]
    pub(crate) const fn as_string(&self) -> Option<&JsString> {
        match self {
            Self::String(value) => Some(value),
            Self::Boolean(_) | Self::Number(_) | Self::BigInt(_) | Self::Symbol(_) => None,
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
            Self::Boolean(_) | Self::Number(_) | Self::BigInt(_) | Self::String(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ArrayState {
    length: u32,
}

impl ArrayState {
    pub(crate) const fn new(length: u32) -> Self {
        Self { length }
    }

    pub(crate) const fn length(self) -> u32 {
        self.length
    }

    pub(crate) fn replace_length(&mut self, length: u32) -> u32 {
        std::mem::replace(&mut self.length, length)
    }
}

pub(crate) struct ArgumentsState {
    parameter_map: Vec<Option<BindingCellId>>,
}

impl ArgumentsState {
    pub(crate) const fn unmapped() -> Self {
        Self {
            parameter_map: Vec::new(),
        }
    }

    pub(crate) fn mapped(parameter_map: Vec<Option<BindingCellId>>) -> Self {
        Self { parameter_map }
    }

    pub(crate) fn cell(&self, index: u32) -> Option<BindingCellId> {
        self.parameter_map.get(index as usize).copied().flatten()
    }

    pub(crate) fn cells(&self) -> impl Iterator<Item = BindingCellId> + '_ {
        self.parameter_map.iter().copied().flatten()
    }

    pub(crate) fn detach(&mut self, index: u32) -> Option<BindingCellId> {
        self.parameter_map
            .get_mut(index as usize)
            .and_then(Option::take)
    }

    pub(crate) fn mapping_len(&self) -> usize {
        self.parameter_map.len()
    }
}

pub(crate) enum HeapObjectKind {
    Ordinary,
    /// An ordinary or exotic arguments object with `[[ParameterMap]]` state.
    Arguments(ArgumentsState),
    /// An ordinary null-prototype object with the `[[IsRawJSON]]` slot.
    RawJson,
    Array(ArrayState),
    Error,
    /// An ECMAScript Promise object with its specification-level internal slots.
    Promise(PromiseState),
    BoxedPrimitive(BoxedPrimitive),
    ForInIterator(ForInIterator),
    ArrayIterator(ArrayIterator),
    StringIterator(StringIterator),
    /// An ECMAScript Map object with ordered `[[MapData]]`.
    Map(MapState),
    MapIterator(MapIterator),
    /// An ECMAScript Set object with ordered `[[SetData]]`.
    Set(SetState),
    SetIterator(SetIterator),
    /// An ECMAScript `WeakMap` object with ephemeron `[[WeakMapData]]`.
    WeakMap(WeakMapState),
    /// An ECMAScript `WeakSet` object with weak `[[WeakSetData]]`.
    WeakSet(WeakSetState),
    /// An ECMAScript `WeakRef` object with non-owning `[[WeakRefTarget]]`.
    WeakRef(WeakRefState),
    /// An ECMAScript `FinalizationRegistry` with strongly held cleanup state.
    FinalizationRegistry(FinalizationRegistryState),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromiseReactionKind {
    Fulfill,
    Reject,
}

pub(crate) struct PromiseCapability {
    pub(crate) promise: StoredValue,
    pub(crate) resolve: FunctionId,
    pub(crate) reject: FunctionId,
}

impl Clone for PromiseCapability {
    fn clone(&self) -> Self {
        Self {
            promise: self.promise.duplicate(),
            resolve: self.resolve,
            reject: self.reject,
        }
    }
}

#[derive(Clone)]
pub(crate) struct PromiseReaction {
    pub(crate) kind: PromiseReactionKind,
    pub(crate) target: PromiseReactionTarget,
}

#[derive(Clone)]
pub(crate) enum PromiseReactionTarget {
    Then {
        handler: Option<FunctionId>,
        capability: PromiseCapability,
    },
    AsyncFunction {
        activation: ObjectId,
    },
    AsyncGenerator {
        generator: ObjectId,
    },
    ArrayFromAsync {
        operation: ObjectId,
    },
}

pub(crate) enum PromiseState {
    Pending {
        fulfill_reactions: Vec<PromiseReaction>,
        reject_reactions: Vec<PromiseReaction>,
        is_handled: bool,
    },
    Fulfilled(StoredValue),
    Rejected {
        reason: StoredValue,
        is_handled: bool,
    },
}

impl PromiseState {
    #[must_use]
    pub(crate) const fn pending() -> Self {
        Self::Pending {
            fulfill_reactions: Vec::new(),
            reject_reactions: Vec::new(),
            is_handled: false,
        }
    }
}

impl HeapObjectKind {
    #[must_use]
    pub(crate) const fn boxed_primitive(&self) -> Option<&BoxedPrimitive> {
        match self {
            Self::Ordinary
            | Self::Arguments(_)
            | Self::RawJson
            | Self::Array(_)
            | Self::Error
            | Self::Promise(_)
            | Self::ForInIterator(_)
            | Self::ArrayIterator(_)
            | Self::StringIterator(_)
            | Self::Map(_)
            | Self::MapIterator(_)
            | Self::Set(_)
            | Self::SetIterator(_)
            | Self::WeakMap(_)
            | Self::WeakSet(_)
            | Self::WeakRef(_)
            | Self::FinalizationRegistry(_) => None,
            Self::BoxedPrimitive(value) => Some(value),
        }
    }

    pub(crate) const fn array(&self) -> Option<&ArrayState> {
        match self {
            Self::Array(state) => Some(state),
            Self::Ordinary
            | Self::Arguments(_)
            | Self::RawJson
            | Self::Error
            | Self::Promise(_)
            | Self::BoxedPrimitive(_)
            | Self::ForInIterator(_)
            | Self::ArrayIterator(_)
            | Self::StringIterator(_)
            | Self::Map(_)
            | Self::MapIterator(_)
            | Self::Set(_)
            | Self::SetIterator(_)
            | Self::WeakMap(_)
            | Self::WeakSet(_)
            | Self::WeakRef(_)
            | Self::FinalizationRegistry(_) => None,
        }
    }

    pub(crate) const fn array_mut(&mut self) -> Option<&mut ArrayState> {
        match self {
            Self::Array(state) => Some(state),
            Self::Ordinary
            | Self::Arguments(_)
            | Self::RawJson
            | Self::Error
            | Self::Promise(_)
            | Self::BoxedPrimitive(_)
            | Self::ForInIterator(_)
            | Self::ArrayIterator(_)
            | Self::StringIterator(_)
            | Self::Map(_)
            | Self::MapIterator(_)
            | Self::Set(_)
            | Self::SetIterator(_)
            | Self::WeakMap(_)
            | Self::WeakSet(_)
            | Self::WeakRef(_)
            | Self::FinalizationRegistry(_) => None,
        }
    }

    pub(crate) const fn for_in_iterator(&self) -> Option<&ForInIterator> {
        match self {
            Self::ForInIterator(iterator) => Some(iterator),
            Self::Ordinary
            | Self::Arguments(_)
            | Self::RawJson
            | Self::Array(_)
            | Self::Error
            | Self::Promise(_)
            | Self::BoxedPrimitive(_)
            | Self::ArrayIterator(_)
            | Self::StringIterator(_)
            | Self::Map(_)
            | Self::MapIterator(_)
            | Self::Set(_)
            | Self::SetIterator(_)
            | Self::WeakMap(_)
            | Self::WeakSet(_)
            | Self::WeakRef(_)
            | Self::FinalizationRegistry(_) => None,
        }
    }

    pub(crate) const fn for_in_iterator_mut(&mut self) -> Option<&mut ForInIterator> {
        match self {
            Self::ForInIterator(iterator) => Some(iterator),
            Self::Ordinary
            | Self::Arguments(_)
            | Self::RawJson
            | Self::Array(_)
            | Self::Error
            | Self::Promise(_)
            | Self::BoxedPrimitive(_)
            | Self::ArrayIterator(_)
            | Self::StringIterator(_)
            | Self::Map(_)
            | Self::MapIterator(_)
            | Self::Set(_)
            | Self::SetIterator(_)
            | Self::WeakMap(_)
            | Self::WeakSet(_)
            | Self::WeakRef(_)
            | Self::FinalizationRegistry(_) => None,
        }
    }

    pub(crate) const fn array_iterator(&self) -> Option<&ArrayIterator> {
        match self {
            Self::ArrayIterator(iterator) => Some(iterator),
            Self::Ordinary
            | Self::Arguments(_)
            | Self::RawJson
            | Self::Array(_)
            | Self::Error
            | Self::Promise(_)
            | Self::BoxedPrimitive(_)
            | Self::ForInIterator(_)
            | Self::StringIterator(_)
            | Self::Map(_)
            | Self::MapIterator(_)
            | Self::Set(_)
            | Self::SetIterator(_)
            | Self::WeakMap(_)
            | Self::WeakSet(_)
            | Self::WeakRef(_)
            | Self::FinalizationRegistry(_) => None,
        }
    }

    pub(crate) const fn array_iterator_mut(&mut self) -> Option<&mut ArrayIterator> {
        match self {
            Self::ArrayIterator(iterator) => Some(iterator),
            Self::Ordinary
            | Self::Arguments(_)
            | Self::RawJson
            | Self::Array(_)
            | Self::Error
            | Self::Promise(_)
            | Self::BoxedPrimitive(_)
            | Self::ForInIterator(_)
            | Self::StringIterator(_)
            | Self::Map(_)
            | Self::MapIterator(_)
            | Self::Set(_)
            | Self::SetIterator(_)
            | Self::WeakMap(_)
            | Self::WeakSet(_)
            | Self::WeakRef(_)
            | Self::FinalizationRegistry(_) => None,
        }
    }

    pub(crate) const fn string_iterator(&self) -> Option<&StringIterator> {
        match self {
            Self::StringIterator(iterator) => Some(iterator),
            Self::Ordinary
            | Self::Arguments(_)
            | Self::RawJson
            | Self::Array(_)
            | Self::Error
            | Self::Promise(_)
            | Self::BoxedPrimitive(_)
            | Self::ForInIterator(_)
            | Self::ArrayIterator(_)
            | Self::Map(_)
            | Self::MapIterator(_)
            | Self::Set(_)
            | Self::SetIterator(_)
            | Self::WeakMap(_)
            | Self::WeakSet(_)
            | Self::WeakRef(_)
            | Self::FinalizationRegistry(_) => None,
        }
    }

    pub(crate) const fn string_iterator_mut(&mut self) -> Option<&mut StringIterator> {
        match self {
            Self::StringIterator(iterator) => Some(iterator),
            Self::Ordinary
            | Self::Arguments(_)
            | Self::RawJson
            | Self::Array(_)
            | Self::Error
            | Self::Promise(_)
            | Self::BoxedPrimitive(_)
            | Self::ForInIterator(_)
            | Self::ArrayIterator(_)
            | Self::Map(_)
            | Self::MapIterator(_)
            | Self::Set(_)
            | Self::SetIterator(_)
            | Self::WeakMap(_)
            | Self::WeakSet(_)
            | Self::WeakRef(_)
            | Self::FinalizationRegistry(_) => None,
        }
    }

    pub(crate) const fn map(&self) -> Option<&MapState> {
        match self {
            Self::Map(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) const fn map_mut(&mut self) -> Option<&mut MapState> {
        match self {
            Self::Map(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) const fn map_iterator(&self) -> Option<&MapIterator> {
        match self {
            Self::MapIterator(iterator) => Some(iterator),
            _ => None,
        }
    }

    pub(crate) const fn map_iterator_mut(&mut self) -> Option<&mut MapIterator> {
        match self {
            Self::MapIterator(iterator) => Some(iterator),
            _ => None,
        }
    }

    pub(crate) const fn set(&self) -> Option<&SetState> {
        match self {
            Self::Set(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) const fn set_mut(&mut self) -> Option<&mut SetState> {
        match self {
            Self::Set(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) const fn set_iterator(&self) -> Option<&SetIterator> {
        match self {
            Self::SetIterator(iterator) => Some(iterator),
            _ => None,
        }
    }

    pub(crate) const fn set_iterator_mut(&mut self) -> Option<&mut SetIterator> {
        match self {
            Self::SetIterator(iterator) => Some(iterator),
            _ => None,
        }
    }

    pub(crate) const fn weak_map(&self) -> Option<&WeakMapState> {
        match self {
            Self::WeakMap(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) const fn weak_map_mut(&mut self) -> Option<&mut WeakMapState> {
        match self {
            Self::WeakMap(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) const fn weak_set(&self) -> Option<&WeakSetState> {
        match self {
            Self::WeakSet(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) const fn weak_set_mut(&mut self) -> Option<&mut WeakSetState> {
        match self {
            Self::WeakSet(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) const fn weak_ref(&self) -> Option<&WeakRefState> {
        match self {
            Self::WeakRef(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) const fn weak_ref_mut(&mut self) -> Option<&mut WeakRefState> {
        match self {
            Self::WeakRef(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) const fn finalization_registry(&self) -> Option<&FinalizationRegistryState> {
        match self {
            Self::FinalizationRegistry(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) const fn finalization_registry_mut(
        &mut self,
    ) -> Option<&mut FinalizationRegistryState> {
        match self {
            Self::FinalizationRegistry(state) => Some(state),
            _ => None,
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
    pub(crate) const fn arguments(record: ObjectRecord) -> Self {
        Self {
            kind: HeapObjectKind::Arguments(ArgumentsState::unmapped()),
            record,
            public_roots: 0,
        }
    }

    #[must_use]
    pub(crate) fn mapped_arguments(
        record: ObjectRecord,
        parameter_map: Vec<Option<BindingCellId>>,
    ) -> Self {
        Self {
            kind: HeapObjectKind::Arguments(ArgumentsState::mapped(parameter_map)),
            record,
            public_roots: 0,
        }
    }

    #[must_use]
    pub(crate) const fn raw_json(record: ObjectRecord) -> Self {
        Self {
            kind: HeapObjectKind::RawJson,
            record,
            public_roots: 0,
        }
    }

    #[must_use]
    pub(crate) const fn array(record: ObjectRecord, state: ArrayState) -> Self {
        Self {
            kind: HeapObjectKind::Array(state),
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
    pub(crate) const fn promise(record: ObjectRecord) -> Self {
        Self {
            kind: HeapObjectKind::Promise(PromiseState::pending()),
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
    pub(crate) const fn array_iterator(record: ObjectRecord, iterator: ArrayIterator) -> Self {
        Self {
            kind: HeapObjectKind::ArrayIterator(iterator),
            record,
            public_roots: 0,
        }
    }

    #[must_use]
    pub(crate) const fn string_iterator(record: ObjectRecord, iterator: StringIterator) -> Self {
        Self {
            kind: HeapObjectKind::StringIterator(iterator),
            record,
            public_roots: 0,
        }
    }

    #[must_use]
    pub(crate) const fn map(record: ObjectRecord, state: MapState) -> Self {
        Self {
            kind: HeapObjectKind::Map(state),
            record,
            public_roots: 0,
        }
    }

    #[must_use]
    pub(crate) const fn map_iterator(record: ObjectRecord, iterator: MapIterator) -> Self {
        Self {
            kind: HeapObjectKind::MapIterator(iterator),
            record,
            public_roots: 0,
        }
    }

    #[must_use]
    pub(crate) const fn set(record: ObjectRecord, state: SetState) -> Self {
        Self {
            kind: HeapObjectKind::Set(state),
            record,
            public_roots: 0,
        }
    }

    #[must_use]
    pub(crate) const fn set_iterator(record: ObjectRecord, iterator: SetIterator) -> Self {
        Self {
            kind: HeapObjectKind::SetIterator(iterator),
            record,
            public_roots: 0,
        }
    }

    #[must_use]
    pub(crate) const fn weak_map(record: ObjectRecord, state: WeakMapState) -> Self {
        Self {
            kind: HeapObjectKind::WeakMap(state),
            record,
            public_roots: 0,
        }
    }

    #[must_use]
    pub(crate) const fn weak_set(record: ObjectRecord, state: WeakSetState) -> Self {
        Self {
            kind: HeapObjectKind::WeakSet(state),
            record,
            public_roots: 0,
        }
    }

    #[must_use]
    pub(crate) const fn weak_ref(record: ObjectRecord, state: WeakRefState) -> Self {
        Self {
            kind: HeapObjectKind::WeakRef(state),
            record,
            public_roots: 0,
        }
    }

    #[must_use]
    pub(crate) const fn finalization_registry(
        record: ObjectRecord,
        state: FinalizationRegistryState,
    ) -> Self {
        Self {
            kind: HeapObjectKind::FinalizationRegistry(state),
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
    pub(crate) const fn is_error(&self) -> bool {
        matches!(self.kind, HeapObjectKind::Error)
    }

    #[must_use]
    pub(crate) const fn is_promise(&self) -> bool {
        matches!(self.kind, HeapObjectKind::Promise(_))
    }

    pub(crate) const fn promise_state(&self) -> Option<&PromiseState> {
        match &self.kind {
            HeapObjectKind::Promise(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) const fn promise_state_mut(&mut self) -> Option<&mut PromiseState> {
        match &mut self.kind {
            HeapObjectKind::Promise(state) => Some(state),
            _ => None,
        }
    }

    #[must_use]
    pub(crate) const fn is_arguments(&self) -> bool {
        matches!(self.kind, HeapObjectKind::Arguments(_))
    }

    pub(crate) fn arguments_cell(&self, index: u32) -> Option<BindingCellId> {
        match &self.kind {
            HeapObjectKind::Arguments(state) => state.cell(index),
            HeapObjectKind::Ordinary
            | HeapObjectKind::RawJson
            | HeapObjectKind::Array(_)
            | HeapObjectKind::Error
            | HeapObjectKind::Promise(_)
            | HeapObjectKind::BoxedPrimitive(_)
            | HeapObjectKind::ForInIterator(_)
            | HeapObjectKind::ArrayIterator(_)
            | HeapObjectKind::StringIterator(_)
            | HeapObjectKind::Map(_)
            | HeapObjectKind::MapIterator(_)
            | HeapObjectKind::Set(_)
            | HeapObjectKind::SetIterator(_)
            | HeapObjectKind::WeakMap(_)
            | HeapObjectKind::WeakSet(_)
            | HeapObjectKind::WeakRef(_)
            | HeapObjectKind::FinalizationRegistry(_) => None,
        }
    }

    pub(crate) fn arguments_cells(&self) -> impl Iterator<Item = BindingCellId> + '_ {
        match &self.kind {
            HeapObjectKind::Arguments(state) => Some(state.cells()),
            HeapObjectKind::Ordinary
            | HeapObjectKind::RawJson
            | HeapObjectKind::Array(_)
            | HeapObjectKind::Error
            | HeapObjectKind::Promise(_)
            | HeapObjectKind::BoxedPrimitive(_)
            | HeapObjectKind::ForInIterator(_)
            | HeapObjectKind::ArrayIterator(_)
            | HeapObjectKind::StringIterator(_)
            | HeapObjectKind::Map(_)
            | HeapObjectKind::MapIterator(_)
            | HeapObjectKind::Set(_)
            | HeapObjectKind::SetIterator(_)
            | HeapObjectKind::WeakMap(_)
            | HeapObjectKind::WeakSet(_)
            | HeapObjectKind::WeakRef(_)
            | HeapObjectKind::FinalizationRegistry(_) => None,
        }
        .into_iter()
        .flatten()
    }

    pub(crate) fn detach_arguments_cell(&mut self, index: u32) -> Option<BindingCellId> {
        match &mut self.kind {
            HeapObjectKind::Arguments(state) => state.detach(index),
            HeapObjectKind::Ordinary
            | HeapObjectKind::RawJson
            | HeapObjectKind::Array(_)
            | HeapObjectKind::Error
            | HeapObjectKind::Promise(_)
            | HeapObjectKind::BoxedPrimitive(_)
            | HeapObjectKind::ForInIterator(_)
            | HeapObjectKind::ArrayIterator(_)
            | HeapObjectKind::StringIterator(_)
            | HeapObjectKind::Map(_)
            | HeapObjectKind::MapIterator(_)
            | HeapObjectKind::Set(_)
            | HeapObjectKind::SetIterator(_)
            | HeapObjectKind::WeakMap(_)
            | HeapObjectKind::WeakSet(_)
            | HeapObjectKind::WeakRef(_)
            | HeapObjectKind::FinalizationRegistry(_) => None,
        }
    }

    pub(crate) fn arguments_mapping_len(&self) -> usize {
        match &self.kind {
            HeapObjectKind::Arguments(state) => state.mapping_len(),
            HeapObjectKind::Ordinary
            | HeapObjectKind::RawJson
            | HeapObjectKind::Array(_)
            | HeapObjectKind::Error
            | HeapObjectKind::Promise(_)
            | HeapObjectKind::BoxedPrimitive(_)
            | HeapObjectKind::ForInIterator(_)
            | HeapObjectKind::ArrayIterator(_)
            | HeapObjectKind::StringIterator(_)
            | HeapObjectKind::Map(_)
            | HeapObjectKind::MapIterator(_)
            | HeapObjectKind::Set(_)
            | HeapObjectKind::SetIterator(_)
            | HeapObjectKind::WeakMap(_)
            | HeapObjectKind::WeakSet(_)
            | HeapObjectKind::WeakRef(_)
            | HeapObjectKind::FinalizationRegistry(_) => 0,
        }
    }

    #[must_use]
    pub(crate) const fn is_raw_json(&self) -> bool {
        matches!(self.kind, HeapObjectKind::RawJson)
    }

    #[must_use]
    pub(crate) const fn boxed_primitive(&self) -> Option<&BoxedPrimitive> {
        self.kind.boxed_primitive()
    }

    #[must_use]
    pub(crate) const fn array_state(&self) -> Option<&ArrayState> {
        self.kind.array()
    }

    #[must_use]
    pub(crate) const fn array_state_mut(&mut self) -> Option<&mut ArrayState> {
        self.kind.array_mut()
    }

    #[must_use]
    pub(crate) const fn map_state(&self) -> Option<&MapState> {
        self.kind.map()
    }

    #[must_use]
    pub(crate) const fn map_state_mut(&mut self) -> Option<&mut MapState> {
        self.kind.map_mut()
    }

    #[must_use]
    pub(crate) const fn map_iterator_state(&self) -> Option<&MapIterator> {
        self.kind.map_iterator()
    }

    #[must_use]
    pub(crate) const fn map_iterator_state_mut(&mut self) -> Option<&mut MapIterator> {
        self.kind.map_iterator_mut()
    }

    #[must_use]
    pub(crate) const fn set_state(&self) -> Option<&SetState> {
        self.kind.set()
    }

    #[must_use]
    pub(crate) const fn set_state_mut(&mut self) -> Option<&mut SetState> {
        self.kind.set_mut()
    }

    #[must_use]
    pub(crate) const fn set_iterator_state(&self) -> Option<&SetIterator> {
        self.kind.set_iterator()
    }

    #[must_use]
    pub(crate) const fn set_iterator_state_mut(&mut self) -> Option<&mut SetIterator> {
        self.kind.set_iterator_mut()
    }

    #[must_use]
    pub(crate) const fn weak_map_state(&self) -> Option<&WeakMapState> {
        self.kind.weak_map()
    }

    #[must_use]
    pub(crate) const fn weak_map_state_mut(&mut self) -> Option<&mut WeakMapState> {
        self.kind.weak_map_mut()
    }

    #[must_use]
    pub(crate) const fn weak_set_state(&self) -> Option<&WeakSetState> {
        self.kind.weak_set()
    }

    #[must_use]
    pub(crate) const fn weak_set_state_mut(&mut self) -> Option<&mut WeakSetState> {
        self.kind.weak_set_mut()
    }

    #[must_use]
    pub(crate) const fn weak_ref_state(&self) -> Option<&WeakRefState> {
        self.kind.weak_ref()
    }

    #[must_use]
    pub(crate) const fn weak_ref_state_mut(&mut self) -> Option<&mut WeakRefState> {
        self.kind.weak_ref_mut()
    }

    #[must_use]
    pub(crate) const fn finalization_registry_state(&self) -> Option<&FinalizationRegistryState> {
        self.kind.finalization_registry()
    }

    #[must_use]
    pub(crate) const fn finalization_registry_state_mut(
        &mut self,
    ) -> Option<&mut FinalizationRegistryState> {
        self.kind.finalization_registry_mut()
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
    pub(crate) const fn array_iterator_state(&self) -> Option<&ArrayIterator> {
        self.kind.array_iterator()
    }

    #[must_use]
    pub(crate) const fn array_iterator_state_mut(&mut self) -> Option<&mut ArrayIterator> {
        self.kind.array_iterator_mut()
    }

    #[must_use]
    pub(crate) const fn string_iterator_state(&self) -> Option<&StringIterator> {
        self.kind.string_iterator()
    }

    #[must_use]
    pub(crate) const fn string_iterator_state_mut(&mut self) -> Option<&mut StringIterator> {
        self.kind.string_iterator_mut()
    }

    #[must_use]
    pub(crate) fn for_in_entry_count(&self) -> usize {
        self.for_in_state().map_or(0, ForInIterator::entry_count)
    }

    #[must_use]
    pub(crate) fn map_entry_count(&self) -> usize {
        self.map_state().map_or(0, MapState::retained_len)
    }

    #[must_use]
    pub(crate) fn set_entry_count(&self) -> usize {
        self.set_state().map_or(0, SetState::retained_len)
    }

    #[must_use]
    pub(crate) fn weak_collection_entry_count(&self) -> usize {
        self.weak_map_state()
            .map_or(0, WeakMapState::len)
            .saturating_add(self.weak_set_state().map_or(0, WeakSetState::len))
            .saturating_add(
                self.finalization_registry_state()
                    .map_or(0, FinalizationRegistryState::len),
            )
    }

    pub(crate) fn map_retained_values(&self) -> impl Iterator<Item = &StoredValue> {
        self.map_state()
            .into_iter()
            .flat_map(MapState::retained_values)
    }

    pub(crate) fn set_retained_values(&self) -> impl Iterator<Item = &StoredValue> {
        self.set_state()
            .into_iter()
            .flat_map(SetState::retained_values)
    }

    pub(crate) fn finalization_retained_values(&self) -> impl Iterator<Item = &StoredValue> {
        self.finalization_registry_state()
            .into_iter()
            .flat_map(FinalizationRegistryState::held_values)
    }

    #[must_use]
    pub(crate) const fn map_iterator_current(&self) -> Option<ObjectId> {
        match self.kind.map_iterator() {
            Some(iterator) => iterator.iterated(),
            None => None,
        }
    }

    #[must_use]
    pub(crate) const fn set_iterator_current(&self) -> Option<ObjectId> {
        match self.kind.set_iterator() {
            Some(iterator) => iterator.iterated(),
            None => None,
        }
    }

    #[must_use]
    pub(crate) const fn for_in_current(&self) -> Option<HeapReference> {
        match self.kind.for_in_iterator() {
            Some(iterator) => iterator.current(),
            None => None,
        }
    }

    #[must_use]
    pub(crate) fn array_iterator_current(&self) -> Option<HeapReference> {
        self.kind
            .array_iterator()
            .and_then(ArrayIterator::iterated)
            .and_then(StoredValue::heap_reference)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ArrayState, BoxedPrimitive, HeapObject, HeapObjectKind, KeyPhases, ObjectRecord,
        OwnProperty,
    };
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

        let snapshot = record
            .try_own_key_snapshot(Some(2), KeyPhases::FOR_IN)
            .expect("snapshot");
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
    fn raw_json_heap_object_preserves_its_internal_brand() {
        let object = HeapObject::raw_json(ObjectRecord::empty(None));

        assert!(matches!(object.kind(), HeapObjectKind::RawJson));
        assert!(object.is_raw_json());
        assert!(!object.is_error());
        assert!(object.boxed_primitive().is_none());
        assert_eq!(object.public_roots, 0);
    }

    #[test]
    fn array_heap_object_preserves_a_typed_length_brand() {
        let mut object = HeapObject::array(ObjectRecord::empty(None), ArrayState::new(7));

        assert!(matches!(object.kind(), HeapObjectKind::Array(_)));
        assert_eq!(
            object.array_state().copied().map(ArrayState::length),
            Some(7)
        );
        object
            .array_state_mut()
            .expect("array state")
            .replace_length(11);
        assert_eq!(
            object.array_state().copied().map(ArrayState::length),
            Some(11)
        );
        assert!(object.boxed_primitive().is_none());
        assert!(object.for_in_state().is_none());
        assert_eq!(object.public_roots, 0);
    }

    #[test]
    fn array_index_truncation_stops_at_the_highest_non_configurable_index() {
        let mut record = ObjectRecord::empty(None);
        for (index, configurable) in [(1, true), (3, false), (5, true)] {
            record
                .append_data(
                    PropertyKey::from_index(ArrayIndex::new(index).expect("array index")),
                    PropertyLayout::data(true, true, configurable),
                    StoredValue::Number(JsNumber::from_i32(
                        i32::try_from(index).expect("small fixture index"),
                    )),
                )
                .expect("array property");
        }

        let truncation = record.truncate_array_indices(1);

        assert_eq!(truncation.final_length(), 4);
        assert_eq!(truncation.blocked_index().map(ArrayIndex::get), Some(3));
        assert_eq!(truncation.removed(), 1);
        assert!(
            record
                .own_property(&PropertyKey::from_index(ArrayIndex::new(1).expect("index")))
                .is_some()
        );
        assert!(
            record
                .own_property(&PropertyKey::from_index(ArrayIndex::new(3).expect("index")))
                .is_some()
        );
        assert!(
            record
                .own_property(&PropertyKey::from_index(ArrayIndex::new(5).expect("index")))
                .is_none()
        );
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
