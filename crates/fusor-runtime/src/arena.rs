//! Runtime-owned typed generational storage.
//!
//! The arena is private until the runtime heap is wired to it. Keeping the
//! implementation here allows that work to use recoverable allocation without
//! prematurely exposing storage details as public API.

use std::cmp::Ordering;
use std::collections::TryReserveError;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::mem;

/// Opaque identity shared by every arena owned by one live runtime.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RuntimeIdentity(usize);

impl RuntimeIdentity {
    pub(crate) const fn from_address(address: usize) -> Self {
        Self(address)
    }
}

/// A typed handle into an [`Arena`].
///
/// Only this module can construct IDs. The marker type prevents a handle for
/// one runtime arena from being accepted by another arena with the same value
/// type.
pub(crate) struct Id<K> {
    runtime: RuntimeIdentity,
    index: usize,
    generation: u32,
    marker: PhantomData<fn() -> K>,
}

impl<K> Id<K> {
    const fn new(runtime: RuntimeIdentity, index: usize, generation: u32) -> Self {
        Self {
            runtime,
            index,
            generation,
            marker: PhantomData,
        }
    }

    /// The never-allocated zero identity, used by snapshot restoration for
    /// records whose owning arena does not exist yet (realm-less restore,
    /// §8.2).
    pub(crate) const ZERO: Self = Self {
        runtime: RuntimeIdentity(0),
        index: 0,
        generation: 0,
        marker: PhantomData,
    };

    pub(crate) const fn index(self) -> usize {
        self.index
    }

    pub(crate) const fn generation(self) -> u32 {
        self.generation
    }
}

impl<K> Copy for Id<K> {}

impl<K> Clone for Id<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> PartialEq for Id<K> {
    fn eq(&self, other: &Self) -> bool {
        self.runtime == other.runtime
            && self.index == other.index
            && self.generation == other.generation
    }
}

impl<K> Eq for Id<K> {}

impl<K> PartialOrd for Id<K> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<K> Ord for Id<K> {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.runtime.0, self.index, self.generation).cmp(&(
            other.runtime.0,
            other.index,
            other.generation,
        ))
    }
}

impl<K> Hash for Id<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.runtime.hash(state);
        self.index.hash(state);
        self.generation.hash(state);
    }
}

impl<K> fmt::Debug for Id<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Id")
            .field("runtime", &self.runtime)
            .field("index", &self.index)
            .field("generation", &self.generation)
            .finish()
    }
}

enum SlotState<T> {
    Occupied(T),
    Vacant { next: Option<usize> },
    Retired,
}

struct Slot<T> {
    generation: u32,
    state: SlotState<T>,
}

/// Runtime-local storage with typed, generation-checked IDs.
///
/// Vacant slots carry the next free index directly, so reuse and removal are
/// iterative and allocate no auxiliary free-list nodes.
pub(crate) struct Arena<K, T> {
    runtime: RuntimeIdentity,
    slots: Vec<Slot<T>>,
    free_head: Option<usize>,
    live_len: usize,
    free_len: usize,
    retired_len: usize,
    marker: PhantomData<fn() -> K>,
}

impl<K, T> Arena<K, T> {
    /// Rebuilds a live identity from its arena index (snapshot
    /// restoration: records insert in encode order, so indices match).
    pub(crate) const fn id_from_index(&self, index: usize) -> Id<K> {
        Id {
            runtime: self.runtime,
            index,
            generation: 0,
            marker: PhantomData,
        }
    }

    /// Restores one snapshot record at its recorded arena index (§8.3).
    ///
    /// Indices above the tail are padded with reusable vacant slots, so
    /// restored records keep their encoded identity space stable for
    /// cross-references; a record at or below the tail must land on a
    /// vacant slot (a recorded hole). Returns `None` when the index is
    /// occupied or the padding allocation fails — never panics.
    pub(crate) fn restore_insert(&mut self, index: usize, value: T) -> Option<Id<K>> {
        if index >= self.slots.len() {
            self.slots.try_reserve(index - self.slots.len()).ok()?;
            while self.slots.len() < index {
                let hole = self.slots.len();
                self.slots.push(Slot {
                    generation: 0,
                    state: SlotState::Vacant {
                        next: self.free_head,
                    },
                });
                self.free_head = Some(hole);
                self.free_len += 1;
            }
            self.slots.push(Slot {
                generation: 0,
                state: SlotState::Occupied(value),
            });
            self.live_len += 1;
            return Some(Id::new(self.runtime, index, 0));
        }

        let next = match &self.slots.get(index)?.state {
            SlotState::Vacant { next } => *next,
            _ => return None,
        };
        // Unlink the slot from the free chain (padded holes only, so the
        // walk stays short).
        let mut previous: Option<usize> = None;
        let mut cursor = self.free_head;
        while let Some(current) = cursor {
            if current == index {
                break;
            }
            previous = Some(current);
            cursor = match &self.slots.get(current)?.state {
                SlotState::Vacant { next } => *next,
                _ => return None,
            };
        }
        if cursor != Some(index) {
            return None;
        }
        match previous {
            None => self.free_head = next,
            Some(previous) => match &mut self.slots[previous].state {
                SlotState::Vacant { next: link } => *link = next,
                _ => return None,
            },
        }
        let slot = &mut self.slots[index];
        slot.generation = 0;
        slot.state = SlotState::Occupied(value);
        self.free_len -= 1;
        self.live_len += 1;
        Some(Id::new(self.runtime, index, 0))
    }

    /// True when slots `0..end` are all occupied first-generation records
    /// (the snapshot realm-prefix precondition, §8.2): a freed or reused
    /// realm record would break the deterministic rebuild.
    pub(crate) fn is_pristine_prefix(&self, end: usize) -> bool {
        if self.slots.len() < end {
            return false;
        }
        self.slots[..end]
            .iter()
            .all(|slot| slot.generation == 0 && matches!(slot.state, SlotState::Occupied(_)))
    }

    /// True when every slot is occupied first-generation (a churn-free
    /// arena, required for arenas whose records are rebuilt by replay
    /// rather than gap-encoded, e.g. the realm table).
    pub(crate) fn is_dense_pristine(&self) -> bool {
        self.live_len == self.slots.len() && self.is_pristine_prefix(self.slots.len())
    }

    pub(crate) const fn new(runtime: RuntimeIdentity) -> Self {
        Self {
            runtime,
            slots: Vec::new(),
            free_head: None,
            live_len: 0,
            free_len: 0,
            retired_len: 0,
            marker: PhantomData,
        }
    }

    /// Reserves enough new backing storage for `additional` insertions.
    ///
    /// Reusable vacant slots count toward the reservation. Retired slots do
    /// not, because their generations can never safely be reused.
    pub(crate) fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.slots
            .try_reserve(additional.saturating_sub(self.free_len))
    }

    /// Inserts a value, returning allocation failure without mutating the arena.
    pub(crate) fn try_insert(&mut self, value: T) -> Result<Id<K>, TryReserveError> {
        if let Some(index) = self.free_head {
            let slot = &mut self.slots[index];
            let SlotState::Vacant { next } = slot.state else {
                unreachable!("the arena free list only contains vacant slots");
            };

            slot.state = SlotState::Occupied(value);
            self.free_head = next;
            self.free_len -= 1;
            self.live_len += 1;
            return Ok(Id::new(self.runtime, index, slot.generation));
        }

        self.slots.try_reserve(1)?;
        let index = self.slots.len();
        self.slots.push(Slot {
            generation: 0,
            state: SlotState::Occupied(value),
        });
        self.live_len += 1;
        Ok(Id::new(self.runtime, index, 0))
    }

    pub(crate) fn contains(&self, id: Id<K>) -> bool {
        self.get(id).is_some()
    }

    pub(crate) fn get(&self, id: Id<K>) -> Option<&T> {
        if id.runtime != self.runtime {
            return None;
        }
        let slot = self.slots.get(id.index)?;
        if slot.generation != id.generation {
            return None;
        }

        match &slot.state {
            SlotState::Occupied(value) => Some(value),
            SlotState::Vacant { .. } | SlotState::Retired => None,
        }
    }

    pub(crate) fn get_mut(&mut self, id: Id<K>) -> Option<&mut T> {
        if id.runtime != self.runtime {
            return None;
        }
        let slot = self.slots.get_mut(id.index)?;
        if slot.generation != id.generation {
            return None;
        }

        match &mut slot.state {
            SlotState::Occupied(value) => Some(value),
            SlotState::Vacant { .. } | SlotState::Retired => None,
        }
    }

    /// Removes a live value and invalidates its ID.
    ///
    /// A reusable slot advances to the next generation. Advancing past
    /// `u32::MAX` would make a previously issued ID valid again, so that slot is
    /// permanently retired instead.
    pub(crate) fn remove(&mut self, id: Id<K>) -> Option<T> {
        if id.runtime != self.runtime {
            return None;
        }
        let slot = self.slots.get_mut(id.index)?;
        if slot.generation != id.generation {
            return None;
        }

        let state = mem::replace(&mut slot.state, SlotState::Retired);
        let SlotState::Occupied(value) = state else {
            slot.state = state;
            return None;
        };

        self.live_len -= 1;
        if let Some(next_generation) = slot.generation.checked_add(1) {
            slot.generation = next_generation;
            slot.state = SlotState::Vacant {
                next: self.free_head,
            };
            self.free_head = Some(id.index);
            self.free_len += 1;
        } else {
            self.retired_len += 1;
        }

        Some(value)
    }

    pub(crate) const fn len(&self) -> usize {
        self.live_len
    }

    #[cfg(test)]
    pub(crate) const fn is_empty(&self) -> bool {
        self.live_len == 0
    }

    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.slots.capacity()
    }

    #[cfg(test)]
    pub(crate) const fn free_len(&self) -> usize {
        self.free_len
    }

    #[cfg(test)]
    pub(crate) const fn retired_len(&self) -> usize {
        self.retired_len
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (Id<K>, &T)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| match &slot.state {
                SlotState::Occupied(value) => {
                    Some((Id::new(self.runtime, index, slot.generation), value))
                }
                SlotState::Vacant { .. } | SlotState::Retired => None,
            })
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = (Id<K>, &mut T)> {
        self.slots
            .iter_mut()
            .enumerate()
            .filter_map(|(index, slot)| match &mut slot.state {
                SlotState::Occupied(value) => {
                    Some((Id::new(self.runtime, index, slot.generation), value))
                }
                SlotState::Vacant { .. } | SlotState::Retired => None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    enum Realm {}
    enum Function {}

    const FIRST_RUNTIME: RuntimeIdentity = RuntimeIdentity::from_address(1);
    const SECOND_RUNTIME: RuntimeIdentity = RuntimeIdentity::from_address(2);

    #[test]
    fn ids_are_typed_and_stale_ids_do_not_alias_reused_slots() {
        let mut realms = Arena::<Realm, _>::new(FIRST_RUNTIME);
        let first = realms.try_insert("first").expect("first slot");

        assert_eq!(realms.remove(first), Some("first"));
        assert!(!realms.contains(first));

        let second = realms.try_insert("second").expect("reused slot");
        assert_eq!(second.index(), first.index());
        assert_eq!(second.generation(), first.generation() + 1);
        assert_eq!(realms.get(first), None);
        assert_eq!(realms.remove(first), None);
        assert_eq!(realms.get(second), Some(&"second"));

        let functions = Arena::<Function, ()>::new(FIRST_RUNTIME);
        assert!(functions.is_empty());
    }

    #[test]
    fn same_marker_ids_do_not_cross_runtime_arenas() {
        let mut first = Arena::<Realm, _>::new(FIRST_RUNTIME);
        let mut second = Arena::<Realm, _>::new(SECOND_RUNTIME);
        let first_id = first.try_insert("first").expect("first slot");
        let second_id = second.try_insert("second").expect("second slot");

        assert_eq!(first_id.index(), second_id.index());
        assert_eq!(first_id.generation(), second_id.generation());
        assert_ne!(first_id, second_id);
        assert_eq!(first.get(second_id), None);
        assert_eq!(first.remove(second_id), None);
        assert_eq!(second.get(first_id), None);
        assert_eq!(second.remove(first_id), None);
        assert_eq!(first.get(first_id), Some(&"first"));
        assert_eq!(second.get(second_id), Some(&"second"));
    }

    #[test]
    fn free_slots_are_reused_iteratively_in_last_removed_first_order() {
        let mut arena = Arena::<Realm, _>::new(FIRST_RUNTIME);
        let first = arena.try_insert(10).expect("first slot");
        let second = arena.try_insert(20).expect("second slot");
        let third = arena.try_insert(30).expect("third slot");

        assert_eq!(arena.remove(first), Some(10));
        assert_eq!(arena.remove(third), Some(30));
        assert_eq!(arena.free_len(), 2);

        let replacement_for_third = arena.try_insert(31).expect("reuse third");
        let replacement_for_first = arena.try_insert(11).expect("reuse first");

        assert_eq!(replacement_for_third.index(), third.index());
        assert_eq!(replacement_for_first.index(), first.index());
        assert_eq!(arena.get(second), Some(&20));
        assert_eq!(arena.free_len(), 0);
    }

    #[test]
    fn removing_the_last_generation_retires_the_slot() {
        let mut arena = Arena::<Realm, _>::new(FIRST_RUNTIME);
        let original = arena.try_insert("old").expect("initial slot");
        arena.slots[original.index()].generation = u32::MAX;
        let last = Id::new(FIRST_RUNTIME, original.index(), u32::MAX);

        assert_eq!(arena.remove(last), Some("old"));
        assert!(!arena.contains(last));
        assert_eq!(arena.retired_len(), 1);
        assert_eq!(arena.free_len(), 0);

        let next = arena.try_insert("new").expect("new slot");
        assert_ne!(next.index(), last.index());
    }

    #[test]
    fn reserve_accounts_for_reusable_slots_and_insertions_remain_fallible() {
        let mut arena = Arena::<Realm, _>::new(FIRST_RUNTIME);
        arena.try_reserve(3).expect("initial reserve");
        assert!(arena.capacity() >= 3);

        let first = arena.try_insert(1).expect("reserved insertion");
        let second = arena.try_insert(2).expect("reserved insertion");
        let _third = arena.try_insert(3).expect("reserved insertion");
        assert_eq!(arena.remove(first), Some(1));
        assert_eq!(arena.remove(second), Some(2));

        let capacity = arena.capacity();
        arena
            .try_reserve(2)
            .expect("free slots satisfy reservation");
        assert_eq!(arena.capacity(), capacity);

        let mut empty = Arena::<Realm, ()>::new(FIRST_RUNTIME);
        assert!(empty.try_reserve(usize::MAX).is_err());
        assert!(empty.is_empty());
    }

    #[test]
    fn mutable_access_and_iteration_only_visit_live_matching_generations() {
        let mut arena = Arena::<Realm, _>::new(FIRST_RUNTIME);
        let first = arena.try_insert(1).expect("first slot");
        let removed = arena.try_insert(2).expect("removed slot");
        let third = arena.try_insert(3).expect("third slot");
        assert_eq!(arena.remove(removed), Some(2));

        *arena.get_mut(first).expect("live mutable value") = 10;
        assert!(arena.get_mut(removed).is_none());

        for (_, value) in arena.iter_mut() {
            *value += 1;
        }

        let entries: Vec<_> = arena
            .iter()
            .map(|(id, value)| (id.index(), *value))
            .collect();
        assert_eq!(entries, vec![(first.index(), 11), (third.index(), 4)]);
        assert_eq!(arena.len(), 2);
        assert!(!arena.is_empty());
    }

    #[test]
    fn restore_insert_pads_holes_and_keeps_recorded_indices() {
        let mut arena = Arena::<Realm, _>::new(FIRST_RUNTIME);
        let first = arena.restore_insert(0, "first").expect("index 0");
        let fourth = arena.restore_insert(3, "fourth").expect("index 3");

        assert_eq!((first.index(), fourth.index()), (0, 3));
        assert_eq!(arena.get(first), Some(&"first"));
        assert_eq!(arena.get(fourth), Some(&"fourth"));
        assert_eq!(arena.len(), 2);

        let reused = arena.try_insert("hole filler").expect("hole reuse");
        assert!(
            reused.index() < 3,
            "allocation reuses a padded hole, not the tail"
        );
        assert_eq!(arena.get(reused), Some(&"hole filler"));
        assert_eq!(arena.len(), 3);

        assert!(
            arena.restore_insert(0, "clash").is_none(),
            "an occupied index rejects a second restore"
        );
        let far = arena
            .restore_insert(10, "far")
            .expect("pads up to index 10");
        assert_eq!(far.index(), 10);
        assert_eq!(arena.get(far), Some(&"far"));
        assert_eq!(arena.len(), 4);
    }
}
