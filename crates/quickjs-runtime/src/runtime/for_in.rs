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

//! Bounded for-in iterator snapshots, prototype scans, and primitive boxing.

use super::{
    ForInAdvance, ForInIterator, ForInSnapshot, HeapObject, HeapReference, JsString, KeyPhases,
    ObjectId, PropertyKey, RealmId, Runtime, RuntimeResource, StoredValue, check_execution_limit,
    for_in_snapshot_work_upper_bound, usize_to_u64,
};

impl Runtime {
    pub(crate) fn allocate_for_in_iterator(
        &mut self,
        realm: RealmId,
        value: StoredValue,
    ) -> Result<(ObjectId, u64), crate::ExecutionError> {
        if matches!(value, StoredValue::Symbol(_)) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "for-in Symbol boxing is not implemented",
            }
            .into());
        }

        let needs_wrapper = matches!(
            value,
            StoredValue::Boolean(_) | StoredValue::Number(_) | StoredValue::String(_)
        );
        let additional_objects = 1_u64.saturating_add(u64::from(needs_wrapper));
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(additional_objects),
        )?;
        self.objects
            .try_reserve(usize::try_from(additional_objects).unwrap_or(usize::MAX))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: usize::try_from(additional_objects).unwrap_or(usize::MAX),
            })?;

        let collection_pending = self.collection_pending;
        let (current, temporary_wrapper) = match value {
            StoredValue::Undefined | StoredValue::Null => (None, None),
            StoredValue::Boolean(value) => {
                let wrapper = self.allocate_boxed_boolean(realm, value)?;
                (Some(HeapReference::Object(wrapper)), Some(wrapper))
            }
            StoredValue::Number(value) => {
                let wrapper = self.allocate_boxed_number(realm, value)?;
                (Some(HeapReference::Object(wrapper)), Some(wrapper))
            }
            StoredValue::String(value) => {
                let wrapper = self.allocate_boxed_string(realm, value)?;
                (Some(HeapReference::Object(wrapper)), Some(wrapper))
            }
            StoredValue::Function(function) => (Some(HeapReference::Function(function)), None),
            StoredValue::Object(object) => (Some(HeapReference::Object(object)), None),
            StoredValue::Symbol(_) => unreachable!("Symbol was rejected before heap mutation"),
        };

        let (snapshot, snapshot_work) = match current {
            Some(reference) => match self.try_for_in_snapshot(reference, 0) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.rollback_for_in_wrapper(temporary_wrapper, collection_pending);
                    return Err(error);
                }
            },
            None => (ForInSnapshot::empty(), 1),
        };
        let snapshot_len = snapshot.len();
        let Ok(iterator) =
            self.objects
                .try_insert(HeapObject::for_in_iterator(ForInIterator::new(
                    current, snapshot,
                )))
        else {
            self.rollback_for_in_wrapper(temporary_wrapper, collection_pending);
            return Err(crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            });
        };
        self.for_in_entries = self
            .for_in_entries
            .saturating_add(usize_to_u64(snapshot_len));
        self.collection_pending = true;
        Ok((iterator, snapshot_work))
    }

    /// Returns an O(1) upper bound for the work performed by
    /// [`Self::allocate_for_in_iterator`].
    ///
    /// The VM charges this preview before it removes the source value from the
    /// operand stack or permits snapshot construction to scan and sort keys.
    pub(crate) fn preview_for_in_iterator_work(
        &self,
        value: &StoredValue,
    ) -> Result<u64, crate::ExecutionError> {
        if matches!(value, StoredValue::Symbol(_)) {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "for-in Symbol boxing is not implemented",
            }
            .into());
        }

        let needs_wrapper = matches!(
            value,
            StoredValue::Boolean(_) | StoredValue::Number(_) | StoredValue::String(_)
        );
        let additional_objects = 1_u64.saturating_add(u64::from(needs_wrapper));
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(additional_objects),
        )?;
        if matches!(value, StoredValue::String(_)) {
            check_execution_limit(
                RuntimeResource::ObjectProperties,
                self.limits.max_object_properties,
                self.object_properties.saturating_add(1),
            )?;
        }

        match value {
            StoredValue::Undefined | StoredValue::Null => Ok(1),
            StoredValue::Boolean(_) | StoredValue::Number(_) => {
                Ok(for_in_snapshot_work_upper_bound(0, None))
            }
            StoredValue::String(value) => {
                Ok(for_in_snapshot_work_upper_bound(1, Some(value.len())))
            }
            StoredValue::Function(function) => {
                Ok(self.preview_for_in_snapshot_work(HeapReference::Function(*function))?)
            }
            StoredValue::Object(object) => {
                Ok(self.preview_for_in_snapshot_work(HeapReference::Object(*object))?)
            }
            StoredValue::Symbol(_) => unreachable!("Symbol was rejected before work preview"),
        }
    }

    /// Returns an O(1) upper bound for one state transition performed by
    /// [`Self::advance_for_in_iterator`].
    ///
    /// No snapshot, cursor, or visited-key state is changed by this preview.
    pub(crate) fn preview_for_in_advance_work(
        &self,
        iterator: ObjectId,
    ) -> Result<u64, crate::ExecutionError> {
        let object = self
            .objects
            .get(iterator)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "for-in iterator",
                index: iterator.index(),
                generation: iterator.generation(),
            })?;
        let state = object
            .for_in_state()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "for-in next received a non-iterator object",
            })?;
        let Some(current) = state.current() else {
            return Ok(1);
        };
        if let Some(candidate) = state.candidate() {
            if state.has_visited(candidate.key()) {
                return Ok(1);
            }
            check_execution_limit(
                RuntimeResource::ForInEntries,
                self.limits.max_for_in_entries,
                self.for_in_entries.saturating_add(1),
            )?;
            let growth_work = state.visited_growth_work();
            if !candidate.enumerable() {
                return Ok(growth_work);
            }
            return Ok(growth_work.saturating_add(
                self.preview_for_in_property_scan_work(current, candidate.key())?
                    .saturating_sub(1),
            ));
        }

        let Some(prototype) = self.object_record(current)?.prototype() else {
            return Ok(usize_to_u64(state.snapshot_len()).saturating_add(1));
        };
        Ok(self
            .preview_for_in_snapshot_work(prototype)?
            .saturating_add(usize_to_u64(state.snapshot_len())))
    }

    pub(crate) fn advance_for_in_iterator(
        &mut self,
        iterator: ObjectId,
    ) -> Result<ForInAdvance, crate::ExecutionError> {
        let (current, candidate, visited, visited_growth_work, previous_snapshot_len) = {
            let object = self
                .objects
                .get(iterator)
                .ok_or(crate::EngineFault::StaleHeapEdge {
                    edge: "for-in iterator",
                    index: iterator.index(),
                    generation: iterator.generation(),
                })?;
            let state = object
                .for_in_state()
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "for-in next received a non-iterator object",
                })?;
            let candidate = state.candidate().cloned();
            let visited = candidate
                .as_ref()
                .is_some_and(|candidate| state.has_visited(candidate.key()));
            (
                state.current(),
                candidate,
                visited,
                state.visited_growth_work(),
                state.snapshot_len(),
            )
        };

        let Some(current) = current else {
            return Ok(ForInAdvance::Done { work: 1 });
        };

        if let Some(candidate) = candidate {
            if visited {
                self.for_in_state_mut(iterator)?.advance_candidate();
                return Ok(ForInAdvance::Continue { work: 1 });
            }

            check_execution_limit(
                RuntimeResource::ForInEntries,
                self.limits.max_for_in_entries,
                self.for_in_entries.saturating_add(1),
            )?;
            let inserted = self
                .for_in_state_mut(iterator)?
                .try_mark_visited(candidate.key().clone())
                .map_err(|_| crate::ExecutionError::AllocationFailed {
                    resource: RuntimeResource::ForInEntries,
                    additional: 1,
                })?;
            if !inserted {
                return Err(crate::EngineFault::RuntimeInvariant {
                    message: "for-in visited-key insertion contradicted its prior lookup",
                }
                .into());
            }
            self.for_in_entries = self.for_in_entries.saturating_add(1);
            self.for_in_state_mut(iterator)?.advance_candidate();

            if !candidate.enumerable() {
                return Ok(ForInAdvance::Continue {
                    work: visited_growth_work,
                });
            }
            let (exists, scanned) = self.for_in_own_property_exists(current, candidate.key())?;
            let work = visited_growth_work.saturating_add(usize_to_u64(scanned));
            return Ok(if exists {
                ForInAdvance::Yield {
                    key: candidate.key().clone(),
                    work,
                }
            } else {
                ForInAdvance::Continue { work }
            });
        }

        let prototype = self.object_record(current)?.prototype();
        let Some(prototype) = prototype else {
            let released = self
                .for_in_state_mut(iterator)?
                .replace_current(None, ForInSnapshot::empty());
            debug_assert_eq!(released, previous_snapshot_len);
            self.for_in_entries = self.for_in_entries.saturating_sub(usize_to_u64(released));
            return Ok(ForInAdvance::Done {
                work: usize_to_u64(released).saturating_add(1),
            });
        };

        let (snapshot, snapshot_work) =
            self.try_for_in_snapshot(prototype, previous_snapshot_len)?;
        let snapshot_len = snapshot.len();
        let released = self
            .for_in_state_mut(iterator)?
            .replace_current(Some(prototype), snapshot);
        debug_assert_eq!(released, previous_snapshot_len);
        self.for_in_entries = self
            .for_in_entries
            .saturating_sub(usize_to_u64(released))
            .saturating_add(usize_to_u64(snapshot_len));
        Ok(ForInAdvance::Continue {
            work: snapshot_work.saturating_add(usize_to_u64(released)),
        })
    }

    fn preview_for_in_snapshot_work(
        &self,
        reference: HeapReference,
    ) -> Result<u64, crate::EngineFault> {
        let string_length = match reference {
            HeapReference::Function(_) => None,
            HeapReference::Object(object) => self.boxed_string(object)?.map(JsString::len),
        };
        let property_count = self.object_record(reference)?.property_count();
        Ok(for_in_snapshot_work_upper_bound(
            property_count,
            string_length,
        ))
    }

    fn preview_for_in_property_scan_work(
        &self,
        reference: HeapReference,
        key: &PropertyKey,
    ) -> Result<u64, crate::EngineFault> {
        if let HeapReference::Object(object) = reference
            && let Some(string) = self.boxed_string(object)?
            && key
                .as_index()
                .is_some_and(|index| index.get() < string.len())
        {
            return Ok(2);
        }
        Ok(usize_to_u64(self.object_record(reference)?.property_count()).saturating_add(1))
    }

    pub(crate) fn is_for_in_iterator(&self, object: ObjectId) -> Result<bool, crate::EngineFault> {
        self.objects
            .get(object)
            .map(|object| object.for_in_state().is_some())
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
    }

    fn for_in_state_mut(
        &mut self,
        iterator: ObjectId,
    ) -> Result<&mut ForInIterator, crate::EngineFault> {
        self.objects
            .get_mut(iterator)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "for-in iterator",
                index: iterator.index(),
                generation: iterator.generation(),
            })?
            .for_in_state_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "for-in next received a non-iterator object",
            })
    }

    pub(crate) fn try_for_in_snapshot(
        &self,
        reference: HeapReference,
        replacing: usize,
    ) -> Result<(ForInSnapshot, u64), crate::ExecutionError> {
        self.try_own_key_snapshot(reference, replacing, KeyPhases::FOR_IN)
    }

    /// Builds an ordered own-key snapshot for one object, charging it against
    /// the same accounting `for-in` uses.
    ///
    /// The snapshot is a value, so a caller may run arbitrary JavaScript
    /// between two keys without observing later shape mutations, which is what
    /// `Object.keys` and `[[OwnPropertyKeys]]` consumers require.
    pub(crate) fn try_own_key_snapshot(
        &self,
        reference: HeapReference,
        replacing: usize,
        phases: KeyPhases,
    ) -> Result<(ForInSnapshot, u64), crate::ExecutionError> {
        let string_length = match reference {
            HeapReference::Function(_) => None,
            HeapReference::Object(object) => self.boxed_string(object)?.map(JsString::len),
        };
        let record = self.object_record(reference)?;
        let count = record.own_key_candidate_count(string_length, phases);
        let observed = self
            .for_in_entries
            .saturating_sub(usize_to_u64(replacing))
            .saturating_add(usize_to_u64(count));
        check_execution_limit(
            RuntimeResource::ForInEntries,
            self.limits.max_for_in_entries,
            observed,
        )?;
        let snapshot = record
            .try_own_key_snapshot(string_length, phases)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ForInEntries,
                additional: count,
            })?;
        // Snapshot construction performs two count passes and separate numeric
        // and atom-key passes before its conservatively charged sort.
        let work = usize_to_u64(record.property_count())
            .saturating_mul(4)
            .saturating_add(usize_to_u64(snapshot.len()))
            .saturating_add(snapshot.sort_work())
            .saturating_add(1);
        Ok((snapshot, work))
    }

    fn for_in_own_property_exists(
        &self,
        reference: HeapReference,
        key: &PropertyKey,
    ) -> Result<(bool, usize), crate::EngineFault> {
        if let HeapReference::Object(object) = reference
            && let Some(string) = self.boxed_string(object)?
            && key
                .as_index()
                .is_some_and(|index| index.get() < string.len())
        {
            return Ok((true, 1));
        }
        Ok(self
            .object_record(reference)?
            .has_own_property_with_scan(key))
    }

    fn rollback_for_in_wrapper(&mut self, wrapper: Option<ObjectId>, collection_pending: bool) {
        let Some(wrapper) = wrapper else {
            return;
        };
        if let Some(object) = self.objects.remove(wrapper) {
            self.object_properties = self
                .object_properties
                .saturating_sub(usize_to_u64(object.record.property_count()));
        }
        self.collection_pending = collection_pending;
    }
}
