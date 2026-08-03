/*
 * JavaScript Array.prototype sorting semantics derived from QuickJS.
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

//! `Array.prototype.sort` and `toSorted`.
//!
//! Both run `js_array_sort` (`quickjs.c:43196-43280`): the present,
//! non-`undefined` elements are collected in ascending index order, the
//! collection is sorted, and the result is written back ascending. A user
//! comparator is a call on every comparison and the default comparison's
//! `ToString` can re-enter the interpreter, so suspension is intrinsic here.
//! Upstream sorts inside its comparator callback with `rqsort`; this port runs
//! an iterative merge sort over explicit state instead. The number and order
//! of comparisons is implementation-defined by ECMAScript, but the outcome is
//! pinned: every comparison falls back to the element's original position, so
//! the sort is stable and the final permutation is fixed for any consistent
//! comparator.
//!
//! The pinned oracle fixes the remaining observable details, and each is
//! reproduced:
//!
//! - `undefined` elements never reach the comparator; they move to the end
//!   (`quickjs.c:43233-43236, 43260-43263`).
//! - Holes are skipped during collection and deleted at the tail, so
//!   `[3,,1].sort()` ends with index `2` absent. `toSorted` instead reads
//!   holes as `undefined`, so its result is dense.
//! - A comparator result that is not a Number converts with `ToNumber`, and
//!   `NaN` means `0` (`quickjs.c:43158-43166`).
//! - When no comparator is given, each element's `ToString` is computed at
//!   most once and compared as UTF-16 code units; even identical values are
//!   converted (`quickjs.c:43167-43183`).
//! - With a comparator, a pair whose values share one bit pattern skips the
//!   call entirely (`quickjs.c:43151-43153`), which is observable: a throwing
//!   comparator on `[5,5,5,5]` is never invoked.
//! - The write-back skips `Set` for an element that did not move
//!   (`quickjs.c:43249-43251`), so a setter on such an index is not called.
//! - A comparator that throws aborts before the write-back, leaving the array
//!   unmodified, and a refused tail delete reports `could not delete
//!   property` (`quickjs.c:43264-43266`).

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// One collected element taking part in the sort.
struct SortSlot {
    value: StoredValue,
    /// The cached `ToString` for the default comparison, computed at most
    /// once per element (`quickjs.c:43171-43182`).
    string: Option<JsString>,
    /// The original index, which breaks comparison ties to keep the sort
    /// stable and decides whether the write-back must `Set` the index.
    pos: u64,
}

impl SortSlot {
    fn duplicate(&self) -> Self {
        Self {
            value: self.value.duplicate(),
            string: self.string.clone(),
            pos: self.pos,
        }
    }
}

/// Which stage of the sort a continuation resumes into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArraySortStage {
    /// Awaiting the `length` property read.
    AwaitLength,
    /// Awaiting `ToLength` of the length value.
    AwaitLengthConversion,
    /// Ready to collect the next source element.
    NextRead,
    /// Awaiting an element read that may have entered a getter.
    AwaitRead,
    /// Ready to perform the next merge step.
    NextMergeStep,
    /// Awaiting the comparator call for the current comparison.
    AwaitComparator,
    /// Awaiting `ToNumber` of the comparator's result.
    AwaitComparatorResult,
    /// Awaiting `ToString` of the left element's default key.
    AwaitLeftString,
    /// Awaiting `ToString` of the right element's default key.
    AwaitRightString,
    /// Ready to write the next result element.
    NextWrite,
    /// Awaiting an element write that may have entered a setter.
    AwaitWrite,
    /// Finished.
    Done,
}

/// One in-progress `sort` or `toSorted`.
pub(crate) struct ArraySortContinuation {
    method: ArraySort,
    /// The coerced receiver `sort` writes back into.
    target: StoredValue,
    /// The user comparator, when one was supplied.
    comparator: Option<FunctionId>,
    /// The element count from the single `ToLength` length read.
    length: u64,
    /// The collected elements; while sorting, one of the two merge buffers.
    slots: Vec<SortSlot>,
    /// The second merge buffer.
    scratch: Vec<SortSlot>,
    /// The count of collected `undefined` elements.
    undefined_count: u64,
    /// The next source index to collect.
    next_read: u64,
    /// The merge width of the current pass.
    width: usize,
    /// The start of the run pair being merged.
    run_start: usize,
    /// The left and right read cursors of the current merge.
    left: usize,
    right: usize,
    /// Whether the current merge pass reads `scratch` and writes `slots`.
    read_scratch: bool,
    /// The number of elements taking part in the sort.
    sort_len: usize,
    /// The next destination index to write.
    next_write: u64,
    realm: RealmId,
    stage: ArraySortStage,
    origin: JsStackFrame,
}

impl ArraySortContinuation {
    /// The receiver, the comparator, and both merge buffers' elements.
    pub(crate) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(usize_to_u64(self.slots.len()))
            .saturating_add(usize_to_u64(self.scratch.len()))
    }

    /// Reports the traced roots this continuation retains.
    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        if let Some(comparator) = self.comparator {
            mark(CollectionRoot::Heap(HeapReference::Function(comparator)));
        }
        for slot in self.slots.iter().chain(self.scratch.iter()) {
            trace_stored_value_root(&slot.value, mark);
        }
    }
}

/// Starts one `sort` or `toSorted`.
#[expect(
    clippy::too_many_arguments,
    reason = "one shared entry point carries the method identity alongside the same receiver, arguments, and resumption context every native dispatch takes"
)]
pub(super) fn begin_array_sort(
    runtime: &mut Runtime,
    method: ArraySort,
    realm: RealmId,
    receiver: StoredValue,
    arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    // The comparator is validated before `ToObject(this)`
    // (`quickjs.c:43206-43210`), so `sort.call(null, 5)` reports the function
    // rather than the receiver.
    let mut arguments = arguments;
    let comparator = match arguments.take_first() {
        None | Some(StoredValue::Undefined) => None,
        Some(StoredValue::Function(function)) => Some(function),
        Some(_) => {
            return Err(NativeFailure::Abrupt(PendingException {
                realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::TypeError,
                    message: JsString::from_utf8("not a function")?,
                },
                origin,
            }));
        }
    };
    if matches!(receiver, StoredValue::Undefined | StoredValue::Null) {
        return Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("cannot convert to object")?,
            },
            origin,
        }));
    }
    let state = ArraySortContinuation {
        method,
        target: receiver,
        comparator,
        length: 0,
        slots: Vec::new(),
        scratch: Vec::new(),
        undefined_count: 0,
        next_read: 0,
        width: 1,
        run_start: 0,
        left: 0,
        right: 0,
        read_scratch: false,
        sort_len: 0,
        next_write: 0,
        realm,
        stage: ArraySortStage::AwaitLength,
        origin,
    };
    advance_array_sort(runtime, state, None, return_to, execution_budget)
}

/// Resumes a sort after an awaited read, conversion, call, or write.
#[allow(
    clippy::too_many_lines,
    reason = "the length, collection, merge, and write-back stages form one traced continuation shared by sort and toSorted"
)]
pub(super) fn advance_array_sort(
    runtime: &mut Runtime,
    mut state: ArraySortContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            ArraySortStage::AwaitLength => {
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                charge_sort_lookup(runtime, &state.target, execution_budget)?;
                match read_static_property(runtime, state.realm, &state.target, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        completion = Some(value);
                        state.stage = ArraySortStage::AwaitLengthConversion;
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.stage = ArraySortStage::AwaitLengthConversion;
                        return suspend(
                            state,
                            function,
                            receiver,
                            CallArguments::empty(),
                            return_to,
                        );
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(sort_failure(&state, failure));
                    }
                }
            }
            ArraySortStage::AwaitLengthConversion => {
                let value = take_completion(&mut completion)?;
                let number = operator_to_number(value, state.realm, &state.origin)?;
                state.length = number_to_length(number);
                state.stage = ArraySortStage::NextRead;
            }
            ArraySortStage::NextRead => {
                if state.next_read >= state.length {
                    state.stage = begin_merge(&mut state)?;
                    continue;
                }
                execution_budget.charge_instructions(1)?;
                let index = state.next_read;
                state.next_read = state.next_read.saturating_add(1);
                let key = element_key(index)?;
                charge_sort_lookup(runtime, &state.target, execution_budget)?;
                if !has_property(runtime, state.realm, &state.target, &key)? {
                    // `sort` skips a hole; `toSorted` reads it as `undefined`,
                    // which is what makes its result dense.
                    if matches!(state.method, ArraySort::ToSorted) {
                        state.undefined_count = state.undefined_count.saturating_add(1);
                    }
                    continue;
                }
                match read_static_property(runtime, state.realm, &state.target, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        completion = Some(value);
                        state.stage = ArraySortStage::AwaitRead;
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.stage = ArraySortStage::AwaitRead;
                        return suspend(
                            state,
                            function,
                            receiver,
                            CallArguments::empty(),
                            return_to,
                        );
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(sort_failure(&state, failure));
                    }
                }
            }
            ArraySortStage::AwaitRead => {
                let value = take_completion(&mut completion)?;
                if matches!(value, StoredValue::Undefined) {
                    // `undefined` elements never reach the comparator; they are
                    // counted and move to the end (`quickjs.c:43233-43236`).
                    state.undefined_count = state.undefined_count.saturating_add(1);
                } else {
                    state
                        .slots
                        .try_reserve(1)
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 1,
                        })?;
                    state.slots.push(SortSlot {
                        value,
                        string: None,
                        pos: state.next_read.saturating_sub(1),
                    });
                }
                state.stage = ArraySortStage::NextRead;
            }
            ArraySortStage::NextMergeStep => {
                let sort_len = state.sort_len;
                let mid = state.run_start.saturating_add(state.width).min(sort_len);
                let end = state
                    .run_start
                    .saturating_add(state.width.saturating_mul(2))
                    .min(sort_len);
                if state.left >= mid && state.right >= end {
                    // The run pair is merged; advance to the next pair, or end
                    // the pass by swapping the buffers' roles.
                    state.run_start = state
                        .run_start
                        .saturating_add(state.width.saturating_mul(2));
                    if state.run_start < sort_len {
                        state.left = state.run_start;
                        state.right = state.run_start.saturating_add(state.width).min(sort_len);
                        continue;
                    }
                    state.read_scratch = !state.read_scratch;
                    if state.read_scratch {
                        state.slots.clear();
                    } else {
                        state.scratch.clear();
                    }
                    state.width = state.width.saturating_mul(2);
                    state.run_start = 0;
                    state.left = 0;
                    state.right = state.width.min(sort_len);
                    if state.width >= sort_len {
                        // The source buffer now holds the fully sorted data;
                        // `slots` must hold it for the result phase.
                        if state.read_scratch {
                            std::mem::swap(&mut state.slots, &mut state.scratch);
                        }
                        state.stage = ArraySortStage::NextWrite;
                    }
                    continue;
                }
                if state.left >= mid {
                    take_merge_element(&mut state, false, execution_budget)?;
                    continue;
                }
                if state.right >= end {
                    take_merge_element(&mut state, true, execution_budget)?;
                    continue;
                }
                // A comparison is required. With a comparator, a pair sharing
                // one bit pattern skips the call (`quickjs.c:43151-153`).
                if let Some(comparefn) = state.comparator {
                    let (left_value, right_value) = {
                        let (left_slot, right_slot) = merge_pair(&state);
                        (left_slot.value.duplicate(), right_slot.value.duplicate())
                    };
                    if sort_values_identical(&left_value, &right_value) {
                        apply_comparison(&mut state, 0, execution_budget)?;
                        continue;
                    }
                    let mut values = Vec::new();
                    values
                        .try_reserve_exact(2)
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 2,
                        })?;
                    values.push(left_value);
                    values.push(right_value);
                    state.stage = ArraySortStage::AwaitComparator;
                    return suspend(
                        state,
                        comparefn,
                        StoredValue::Undefined,
                        CallArguments::from_values(values),
                        return_to,
                    );
                }
                // The default comparison converts each element with `ToString`
                // at most once and compares the results as UTF-16 code units.
                let left_needs_string = merge_source(&state, state.left).string.is_none();
                if left_needs_string {
                    let value = merge_source(&state, state.left).value.duplicate();
                    if needs_conversion(&value) {
                        state.stage = ArraySortStage::AwaitLeftString;
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::String,
                            OperatorPrimitiveTarget::ArraySortComparison(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    let string = operator_primitive_to_string(value, state.realm, &state.origin)?;
                    let left = state.left;
                    merge_source_mut(&mut state, left).string = Some(string);
                    continue;
                }
                let right_needs_string = merge_source(&state, state.right).string.is_none();
                if right_needs_string {
                    let value = merge_source(&state, state.right).value.duplicate();
                    if needs_conversion(&value) {
                        state.stage = ArraySortStage::AwaitRightString;
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::String,
                            OperatorPrimitiveTarget::ArraySortComparison(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    let string = operator_primitive_to_string(value, state.realm, &state.origin)?;
                    let right = state.right;
                    merge_source_mut(&mut state, right).string = Some(string);
                    continue;
                }
                let ordering = {
                    let (left_slot, right_slot) = merge_pair(&state);
                    let left_string =
                        left_slot
                            .string
                            .as_ref()
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "a sort left slot lost its comparison string",
                            })?;
                    let right_string =
                        right_slot
                            .string
                            .as_ref()
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "a sort right slot lost its comparison string",
                            })?;
                    compare_code_units(left_string, right_string)
                };
                apply_comparison(&mut state, ordering, execution_budget)?;
            }
            ArraySortStage::AwaitComparator => {
                let value = take_completion(&mut completion)?;
                if let StoredValue::Number(number) = value {
                    let ordering = comparator_ordering(number.as_f64());
                    apply_comparison(&mut state, ordering, execution_budget)?;
                    state.stage = ArraySortStage::NextMergeStep;
                    continue;
                }
                if needs_conversion(&value) {
                    state.stage = ArraySortStage::AwaitComparatorResult;
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::Number,
                        OperatorPrimitiveTarget::ArraySortComparison(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                let number = operator_to_number(value, state.realm, &state.origin)?;
                let ordering = comparator_ordering(number.as_f64());
                apply_comparison(&mut state, ordering, execution_budget)?;
                state.stage = ArraySortStage::NextMergeStep;
            }
            ArraySortStage::AwaitComparatorResult => {
                let value = take_completion(&mut completion)?;
                let number = operator_to_number(value, state.realm, &state.origin)?;
                let ordering = comparator_ordering(number.as_f64());
                apply_comparison(&mut state, ordering, execution_budget)?;
                state.stage = ArraySortStage::NextMergeStep;
            }
            ArraySortStage::AwaitLeftString => {
                let value = take_completion(&mut completion)?;
                let string = operator_primitive_to_string(value, state.realm, &state.origin)?;
                let left = state.left;
                merge_source_mut(&mut state, left).string = Some(string);
                state.stage = ArraySortStage::NextMergeStep;
            }
            ArraySortStage::AwaitRightString => {
                let value = take_completion(&mut completion)?;
                let string = operator_primitive_to_string(value, state.realm, &state.origin)?;
                let right = state.right;
                merge_source_mut(&mut state, right).string = Some(string);
                state.stage = ArraySortStage::NextMergeStep;
            }
            ArraySortStage::NextWrite => {
                if matches!(state.method, ArraySort::ToSorted) {
                    // `toSorted` answers a fresh dense Array: the sorted values
                    // followed by the collected `undefined` elements.
                    let mut values = Vec::new();
                    let total = state
                        .slots
                        .len()
                        .saturating_add(usize::try_from(state.undefined_count).map_err(
                        |_| EngineFault::RuntimeInvariant {
                            message: "a toSorted undefined count exceeded the addressable result",
                        },
                    )?);
                    values
                        .try_reserve(total)
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: total,
                        })?;
                    for slot in &state.slots {
                        values.push(slot.value.duplicate());
                    }
                    for _ in 0..state.undefined_count {
                        values.push(StoredValue::Undefined);
                    }
                    let result = runtime.allocate_array(state.realm, values)?;
                    return Ok(NativeDispatch::Immediate(StoredValue::Object(result)));
                }
                let slot_count = usize_to_u64(state.slots.len());
                let defined_end = slot_count.saturating_add(state.undefined_count);
                if state.next_write >= defined_end {
                    // The sorted prefix and the `undefined` tail are written;
                    // the remaining indices are deleted, which is how holes
                    // stay holes (`quickjs.c:43264-43267`).
                    let mut index = defined_end;
                    while index < state.length {
                        execution_budget.charge_instructions(1)?;
                        let key = element_key(index)?;
                        match delete_static_property(runtime, &state.target, &key)? {
                            PropertyDeleteOutcome::Deleted => {}
                            // Upstream deletes with `JS_PROP_THROW`, so a
                            // refused delete reports `could not delete
                            // property`.
                            PropertyDeleteOutcome::Refused => {
                                return Err(sort_failure(&state, PropertyFailure::NotDeletable));
                            }
                            PropertyDeleteOutcome::Failed(failure) => {
                                return Err(sort_failure(&state, failure));
                            }
                        }
                        index = index.saturating_add(1);
                    }
                    state.stage = ArraySortStage::Done;
                    continue;
                }
                execution_budget.charge_instructions(1)?;
                let index = state.next_write;
                state.next_write = state.next_write.saturating_add(1);
                let value = if index < slot_count {
                    let slot = &state.slots[usize::try_from(index).map_err(|_| {
                        EngineFault::RuntimeInvariant {
                            message: "a sort write index exceeded the addressable domain",
                        }
                    })?];
                    // An element that did not move is not written, so a setter
                    // on its index is not called (`quickjs.c:43249-43251`).
                    if slot.pos == index {
                        continue;
                    }
                    slot.value.duplicate()
                } else {
                    StoredValue::Undefined
                };
                let key = element_key(index)?;
                charge_sort_lookup(runtime, &state.target, execution_budget)?;
                match write_static_property(
                    runtime,
                    state.realm,
                    &state.target,
                    key,
                    value,
                    true,
                    execution_budget,
                )? {
                    PropertyWriteOutcome::Complete => {}
                    PropertyWriteOutcome::Setter {
                        function,
                        receiver,
                        value,
                    } => {
                        state.stage = ArraySortStage::AwaitWrite;
                        let mut values = Vec::new();
                        values.try_reserve_exact(1).map_err(|_| {
                            ExecutionError::AllocationFailed {
                                resource: RuntimeResource::Frames,
                                additional: 1,
                            }
                        })?;
                        values.push(value);
                        return suspend(
                            state,
                            function,
                            receiver,
                            CallArguments::from_values(values),
                            return_to,
                        );
                    }
                    PropertyWriteOutcome::Failed(failure) => {
                        return Err(sort_failure(&state, failure));
                    }
                }
            }
            ArraySortStage::AwaitWrite => {
                take_completion(&mut completion)?;
                state.stage = ArraySortStage::NextWrite;
            }
            ArraySortStage::Done => {
                return Ok(NativeDispatch::Immediate(state.target.duplicate()));
            }
        }
    }
}

/// Sets the merge up once the collection completes.
fn begin_merge(state: &mut ArraySortContinuation) -> Result<ArraySortStage, NativeFailure> {
    state.sort_len = state.slots.len();
    if state.sort_len < 2 {
        // Fewer than two elements means no comparison at all, so no `ToString`
        // and no comparator call happens (`quickjs.c:43241` does not run).
        return Ok(ArraySortStage::NextWrite);
    }
    state
        .scratch
        .try_reserve(state.sort_len)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: state.sort_len,
        })?;
    state.width = 1;
    state.run_start = 0;
    state.left = 0;
    state.right = state.width.min(state.sort_len);
    state.read_scratch = false;
    Ok(ArraySortStage::NextMergeStep)
}

/// Returns the source slot at `index` for the current merge pass.
fn merge_source(state: &ArraySortContinuation, index: usize) -> &SortSlot {
    if state.read_scratch {
        &state.scratch[index]
    } else {
        &state.slots[index]
    }
}

/// Returns the mutable source slot at `index` for the current merge pass.
fn merge_source_mut(state: &mut ArraySortContinuation, index: usize) -> &mut SortSlot {
    if state.read_scratch {
        &mut state.scratch[index]
    } else {
        &mut state.slots[index]
    }
}

/// Returns the two source slots under comparison.
fn merge_pair(state: &ArraySortContinuation) -> (&SortSlot, &SortSlot) {
    (
        merge_source(state, state.left),
        merge_source(state, state.right),
    )
}

/// Moves one source element into the destination buffer.
fn take_merge_element(
    state: &mut ArraySortContinuation,
    left: bool,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    execution_budget.charge_instructions(1)?;
    let index = if left { state.left } else { state.right };
    let slot = merge_source(state, index).duplicate();
    if state.read_scratch {
        state.slots.push(slot);
    } else {
        state.scratch.push(slot);
    }
    if left {
        state.left = state.left.saturating_add(1);
    } else {
        state.right = state.right.saturating_add(1);
    }
    Ok(())
}

/// Applies one comparison outcome to the merge.
///
/// A zero ordering is impossible for two distinct slots: their original
/// positions break the tie, which is what keeps the sort stable
/// (`quickjs.c:43187-43189`).
fn apply_comparison(
    state: &mut ArraySortContinuation,
    ordering: i32,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    execution_budget.charge_instructions(1)?;
    let ordering = if ordering == 0 {
        let (left_slot, right_slot) = merge_pair(state);
        match left_slot.pos.cmp(&right_slot.pos) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Greater => 1,
            // Two slots can never share an original position.
            std::cmp::Ordering::Equal => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "two sort slots shared an original position",
                }
                .into());
            }
        }
    } else {
        ordering
    };
    take_merge_element(state, ordering < 0, execution_budget)
}

/// Returns the sign of a comparator result, with `NaN` meaning `0`
/// (`quickjs.c:43158-43166`).
fn comparator_ordering(value: f64) -> i32 {
    if value > 0.0 {
        1
    } else if value < 0.0 {
        -1
    } else {
        0
    }
}

/// Returns whether two values share `QuickJS`'s exact `JSValue` bit pattern.
///
/// This is the `memcmp` in `js_array_cmp_generic` (`quickjs.c:43151`): an
/// identical pair skips the comparator call entirely, which is observable in
/// the call count. The comparison is bitwise rather than semantic: `+0` and
/// `-0` differ, two NaNs with one payload match, and two equal strings match
/// only when they share one allocation.
fn sort_values_identical(left: &StoredValue, right: &StoredValue) -> bool {
    match (left, right) {
        (StoredValue::Undefined, StoredValue::Undefined)
        | (StoredValue::Null, StoredValue::Null) => true,
        (StoredValue::Boolean(left), StoredValue::Boolean(right)) => left == right,
        (StoredValue::Number(left), StoredValue::Number(right)) => left.same_bits(*right),
        (StoredValue::BigInt(left), StoredValue::BigInt(right)) => Arc::ptr_eq(left, right),
        (StoredValue::String(left), StoredValue::String(right)) => left.shares_allocation(right),
        (StoredValue::Symbol(left), StoredValue::Symbol(right)) => left.is_same_identity(right),
        (StoredValue::Function(left), StoredValue::Function(right)) => left == right,
        (StoredValue::Object(left), StoredValue::Object(right)) => left == right,
        _ => false,
    }
}

/// Compares two strings as UTF-16 code-unit sequences.
///
/// This is `js_string_compare` (`quickjs.c:4616-4633`): lexicographic by code
/// unit, with the shorter string first when one is a prefix of the other.
fn compare_code_units(left: &JsString, right: &JsString) -> i32 {
    let mut left_units = left.code_units();
    let mut right_units = right.code_units();
    loop {
        match (left_units.next(), right_units.next()) {
            (None, None) => return 0,
            (None, Some(_)) => return -1,
            (Some(_), None) => return 1,
            (Some(left_unit), Some(right_unit)) => {
                if left_unit != right_unit {
                    return if left_unit < right_unit { -1 } else { 1 };
                }
            }
        }
    }
}

/// Suspends into a call that resumes this continuation.
fn suspend(
    state: ArraySortContinuation,
    function: FunctionId,
    receiver: StoredValue,
    arguments: CallArguments,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::ArraySort(Box::new(state)));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments,
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

/// Builds the exception a failed property operation reports.
fn sort_failure(state: &ArraySortContinuation, failure: PropertyFailure) -> NativeFailure {
    match property_exception_at(state.realm, state.origin.clone(), None, failure) {
        Ok(exception) => NativeFailure::Abrupt(exception),
        Err(error) => error.into(),
    }
}

/// Charges one property lookup, tolerating a primitive receiver.
fn charge_sort_lookup(
    runtime: &Runtime,
    base: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    if base.heap_reference().is_none() {
        execution_budget.charge_instructions(1)?;
        return Ok(());
    }
    charge_heap_property_lookup(runtime, base, execution_budget)
}

/// Returns whether a value needs a resumable `ToPrimitive`.
const fn needs_conversion(value: &StoredValue) -> bool {
    matches!(value, StoredValue::Function(_) | StoredValue::Object(_))
}

/// Returns the property key for one element index.
fn element_key(index: u64) -> Result<PropertyKey, NativeFailure> {
    let index = u32::try_from(index).map_err(|_| EngineFault::RuntimeInvariant {
        message: "array sort index exceeded the array-index domain",
    })?;
    let index = ArrayIndex::new(index).ok_or(EngineFault::RuntimeInvariant {
        message: "array sort index reached the non-index sentinel",
    })?;
    Ok(PropertyKey::from_index(index))
}

/// Extracts the awaited completion value.
fn take_completion(completion: &mut Option<StoredValue>) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        NativeFailure::Execution(
            EngineFault::RuntimeInvariant {
                message: "an array sort resumed without its awaited completion",
            }
            .into(),
        )
    })
}
