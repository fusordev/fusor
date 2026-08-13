/*
 * JavaScript Array sorting semantics derived from QuickJS.
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

//! Stable, resumable `Array.prototype.sort` and `toSorted`.
//!
//! Both methods first collect every value required by `SortIndexedProperties`,
//! then run a bottom-up stable merge sort. A comparison is an explicit
//! suspension boundary: a user comparator can call arbitrary JavaScript and a
//! default comparison can enter `ToString` twice. The merge cursor and scratch
//! list live in the traced continuation, so neither Rust recursion nor an
//! untraced host callback participates in JavaScript execution.
//!
//! `sort` uses the specification's skip-holes mode, writes the sorted values
//! back, then deletes the remaining indices. `toSorted` allocates its result
//! before reading an element and uses read-through-holes, so its result is dense.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// One non-`undefined` value participating in comparisons.
struct SortItem {
    value: StoredValue,
    /// `QuickJS` caches one default string conversion per source position.
    text: Option<JsString>,
}

impl SortItem {
    fn duplicate(&self) -> Self {
        Self {
            value: self.value.duplicate(),
            text: self.text.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArraySortStage {
    AwaitLength,
    AwaitLengthConversion,
    NextRead,
    AwaitPresence,
    AwaitRead,
    NextMerge,
    AwaitComparator,
    AwaitLeftString,
    AwaitRightString,
    NextWrite,
    AwaitWrite,
    NextDelete,
    AwaitDelete,
    Done,
}

/// The comparison relation selected by the public sorting method.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SortComparison {
    /// `Array.prototype.sort` stringifies values when no comparator exists.
    Lexicographic,
    /// Typed arrays use `CompareTypedArrayElements`, which compares their
    /// numeric element values directly when no comparator exists.
    TypedArray(TypedArrayElementType),
}

/// The publication destination after the common `SortIndexedProperties`
/// collection and merge work has completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SortOutput {
    /// An Array method either writes back to the receiver or creates a fresh
    /// dense Array for `toSorted`.
    Array { destination: Option<ObjectId> },
    /// A typed-array method writes directly to its receiver or an already
    /// allocated same-type copy.
    TypedArray {
        destination: ObjectId,
        element: TypedArrayElementType,
    },
}

/// One in-progress `SortIndexedProperties` invocation and result publication.
pub(crate) struct ArraySortContinuation {
    method: ArraySort,
    target: StoredValue,
    comparator: Option<FunctionId>,
    skip_holes: bool,
    comparison: SortComparison,
    output: SortOutput,
    length: u64,
    next_read: u64,
    items: Vec<SortItem>,
    scratch: Vec<SortItem>,
    undefined_count: u64,
    width: usize,
    merge_start: usize,
    left: usize,
    left_end: usize,
    right: usize,
    right_end: usize,
    next_write: u64,
    left_string: Option<JsString>,
    realm: RealmId,
    stage: ArraySortStage,
    origin: JsStackFrame,
}

impl ArraySortContinuation {
    pub(crate) fn retained_values(&self) -> u64 {
        1_u64
            .saturating_add(u64::from(self.comparator.is_some()))
            .saturating_add(match self.output {
                SortOutput::Array { destination } => u64::from(destination.is_some()),
                SortOutput::TypedArray { .. } => 1,
            })
            .saturating_add(usize_to_u64(self.items.len()))
            .saturating_add(usize_to_u64(self.scratch.len()))
    }

    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        if let Some(comparator) = self.comparator {
            mark(CollectionRoot::Heap(HeapReference::Function(comparator)));
        }
        match self.output {
            SortOutput::Array {
                destination: Some(destination),
            }
            | SortOutput::TypedArray { destination, .. } => {
                mark(CollectionRoot::Heap(HeapReference::Object(destination)));
            }
            SortOutput::Array { destination: None } => {}
        }
        for item in self.items.iter().chain(&self.scratch) {
            trace_stored_value_root(&item.value, mark);
        }
    }
}

/// Starts `sort` or `toSorted`, validating the comparator before `ToObject`.
#[expect(
    clippy::too_many_arguments,
    reason = "native dispatch supplies the method, realm, call values, return target, origin, and shared budget"
)]
pub(super) fn begin_array_sort(
    runtime: &mut Runtime,
    method: ArraySort,
    realm: RealmId,
    receiver: StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let comparator = match arguments.take_first_or_undefined() {
        StoredValue::Undefined => None,
        StoredValue::Function(function) => Some(function),
        _ => return Err(sort_type_error(realm, &origin, "not a function")),
    };
    let target = match to_object_value(runtime, realm, receiver, origin.clone())? {
        Ok(target) => target,
        Err(exception) => return Err(NativeFailure::Abrupt(exception)),
    };
    let state = ArraySortContinuation {
        method,
        target,
        comparator,
        skip_holes: !method.copies(),
        comparison: SortComparison::Lexicographic,
        output: SortOutput::Array { destination: None },
        length: 0,
        next_read: 0,
        items: Vec::new(),
        scratch: Vec::new(),
        undefined_count: 0,
        width: 1,
        merge_start: 0,
        left: 0,
        left_end: 0,
        right: 0,
        right_end: 0,
        next_write: 0,
        left_string: None,
        realm,
        stage: ArraySortStage::AwaitLength,
        origin,
    };
    advance_array_sort(runtime, state, None, return_to, execution_budget)
}

/// Starts a `%TypedArray%.prototype.sort` or `.toSorted` operation after the
/// caller has selected its concrete method. Comparator validation deliberately
/// precedes typed-array validation, matching the public algorithms.
#[expect(
    clippy::too_many_arguments,
    reason = "the typed entry preserves its receiver, comparator, same-type result, and standard call context"
)]
pub(super) fn begin_typed_array_sort(
    runtime: &mut Runtime,
    source: ObjectId,
    copies: bool,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let comparator = match arguments.take_first_or_undefined() {
        StoredValue::Undefined => None,
        StoredValue::Function(function) => Some(function),
        _ => return Err(sort_type_error(realm, &origin, "not a function")),
    };
    let (source_state, length) = typed_array_require_in_bounds(runtime, source, realm, &origin)?;
    let element = source_state.element();
    let destination = if copies {
        typed_array_create_same_type(runtime, realm, element, length, &origin)?
    } else {
        source
    };
    let state = ArraySortContinuation {
        method: if copies {
            ArraySort::ToSorted
        } else {
            ArraySort::Sort
        },
        target: StoredValue::Object(source),
        comparator,
        skip_holes: false,
        comparison: SortComparison::TypedArray(element),
        output: SortOutput::TypedArray {
            destination,
            element,
        },
        length: usize_to_u64(length),
        next_read: 0,
        items: Vec::new(),
        scratch: Vec::new(),
        undefined_count: 0,
        width: 1,
        merge_start: 0,
        left: 0,
        left_end: 0,
        right: 0,
        right_end: 0,
        next_write: 0,
        left_string: None,
        realm,
        stage: ArraySortStage::NextRead,
        origin,
    };
    advance_array_sort(runtime, state, None, return_to, execution_budget)
}

/// Advances collection, stable merging, comparison, and result publication.
#[allow(
    clippy::too_many_lines,
    clippy::needless_continue,
    reason = "the explicit stages keep every accessor, comparator, conversion, write, and deletion suspension in specification order"
)]
pub(super) fn advance_array_sort(
    runtime: &mut Runtime,
    mut state: ArraySortContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    macro_rules! await_get {
        ($operation:expr) => {
            match $operation? {
                GetContinuationDispatch::Ready {
                    state: resumed,
                    value,
                } => {
                    state = resumed;
                    completion = Some(value);
                    continue;
                }
                GetContinuationDispatch::Suspended(dispatch) => return Ok(dispatch),
            }
        };
    }
    loop {
        match state.stage {
            ArraySortStage::AwaitLength => {
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                charge_sort_lookup(runtime, &state.target, execution_budget)?;
                state.stage = ArraySortStage::AwaitLengthConversion;
                let dispatch = begin_value_get(
                    runtime,
                    &state.target,
                    key,
                    None,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                await_get!(continue_get_state_after(
                    dispatch,
                    state,
                    array_sort_continuation,
                    "Array sort length Get produced a structured result",
                ));
            }
            ArraySortStage::AwaitLengthConversion => {
                let value = take_sort_completion(&mut completion)?;
                if needs_sort_conversion(&value) {
                    return convert_sort_value(
                        runtime,
                        state,
                        value,
                        OperatorPrimitiveHint::Number,
                        return_to,
                        execution_budget,
                    );
                }
                state.length =
                    number_to_length(operator_to_number(value, state.realm, &state.origin)?);
                if state.method.copies() {
                    allocate_sorted_destination(runtime, &mut state)?;
                }
                state.stage = ArraySortStage::NextRead;
            }
            ArraySortStage::NextRead => {
                if state.next_read >= state.length {
                    prepare_sort(&mut state)?;
                    continue;
                }
                execution_budget.charge_instructions(1)?;
                let index = state.next_read;
                state.next_read = state.next_read.saturating_add(1);
                let key = sort_element_key(runtime, index)?;
                if state.skip_holes {
                    charge_sort_lookup(runtime, &state.target, execution_budget)?;
                    state.stage = ArraySortStage::AwaitPresence;
                    let dispatch = begin_value_has(
                        runtime,
                        &state.target,
                        key,
                        state.realm,
                        return_to,
                        state.origin.clone(),
                        execution_budget,
                    )?;
                    await_get!(continue_get_state_after(
                        dispatch,
                        state,
                        array_sort_continuation,
                        "Array sort HasProperty produced a structured result",
                    ));
                }
                await_get!(begin_array_sort_element_get(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                ));
            }
            ArraySortStage::AwaitPresence => {
                if !runtime.to_boolean(&take_sort_completion(&mut completion)?)? {
                    state.stage = ArraySortStage::NextRead;
                    continue;
                }
                await_get!(begin_array_sort_element_get(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                ));
            }
            ArraySortStage::AwaitRead => {
                append_sort_value(&mut state, take_sort_completion(&mut completion)?)?;
                state.stage = ArraySortStage::NextRead;
            }
            ArraySortStage::NextMerge => match next_merge_action(&mut state) {
                MergeAction::Complete => {
                    state.next_write = 0;
                    state.stage = ArraySortStage::NextWrite;
                }
                MergeAction::Take { index, from_left } => {
                    execution_budget.charge_instructions(1)?;
                    append_merged_item(&mut state, index, from_left)?;
                }
                MergeAction::Compare => {
                    execution_budget.charge_instructions(1)?;
                    if let Some(comparator) = state.comparator {
                        let mut arguments = Vec::new();
                        arguments.try_reserve_exact(2).map_err(|_| {
                            ExecutionError::AllocationFailed {
                                resource: RuntimeResource::Frames,
                                additional: 2,
                            }
                        })?;
                        arguments.push(state.items[state.left].value.duplicate());
                        arguments.push(state.items[state.right].value.duplicate());
                        state.stage = ArraySortStage::AwaitComparator;
                        return suspend_sort(
                            state,
                            comparator,
                            StoredValue::Undefined,
                            arguments,
                            return_to,
                        );
                    }
                    match state.comparison {
                        SortComparison::Lexicographic => {
                            state.stage = ArraySortStage::AwaitLeftString;
                        }
                        SortComparison::TypedArray(element) => {
                            let take_left = compare_typed_array_items(
                                &state.items[state.left].value,
                                &state.items[state.right].value,
                                element,
                            )?;
                            finish_sort_comparison(&mut state, take_left)?;
                        }
                    }
                }
            },
            ArraySortStage::AwaitComparator => {
                let value = take_sort_completion(&mut completion)?;
                if needs_sort_conversion(&value) {
                    return convert_sort_value(
                        runtime,
                        state,
                        value,
                        OperatorPrimitiveHint::Number,
                        return_to,
                        execution_budget,
                    );
                }
                let compared = operator_to_number(value, state.realm, &state.origin)?.as_f64();
                finish_sort_comparison(&mut state, compared <= 0.0 || compared.is_nan())?;
            }
            ArraySortStage::AwaitLeftString => {
                if state.items[state.left].text.is_none() {
                    if let Some(value) = completion.take() {
                        state.items[state.left].text = Some(operator_primitive_to_string(
                            value,
                            state.realm,
                            &state.origin,
                        )?);
                    } else {
                        let value = state.items[state.left].value.duplicate();
                        return convert_sort_value(
                            runtime,
                            state,
                            value,
                            OperatorPrimitiveHint::String,
                            return_to,
                            execution_budget,
                        );
                    }
                }
                state.left_string.clone_from(&state.items[state.left].text);
                state.stage = ArraySortStage::AwaitRightString;
            }
            ArraySortStage::AwaitRightString => {
                if state.items[state.right].text.is_none() {
                    if let Some(value) = completion.take() {
                        state.items[state.right].text = Some(operator_primitive_to_string(
                            value,
                            state.realm,
                            &state.origin,
                        )?);
                    } else {
                        let value = state.items[state.right].value.duplicate();
                        return convert_sort_value(
                            runtime,
                            state,
                            value,
                            OperatorPrimitiveHint::String,
                            return_to,
                            execution_budget,
                        );
                    }
                }
                let left = state
                    .left_string
                    .take()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "array sort lost its left comparison string",
                    })?;
                let right = state.items[state.right].text.as_ref().ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "array sort lost its right comparison string",
                    },
                )?;
                execution_budget.charge_instructions(
                    u64::from(left.len())
                        .saturating_add(u64::from(right.len()))
                        .saturating_add(1),
                )?;
                let take_left = left <= *right;
                finish_sort_comparison(&mut state, take_left)?;
            }
            ArraySortStage::NextWrite => {
                let sortable = usize_to_u64(state.items.len());
                let item_count = sortable.saturating_add(state.undefined_count);
                if state.next_write >= item_count {
                    state.stage = match state.output {
                        SortOutput::Array { destination: None } => ArraySortStage::NextDelete,
                        SortOutput::Array {
                            destination: Some(_),
                        }
                        | SortOutput::TypedArray { .. } => ArraySortStage::Done,
                    };
                    continue;
                }
                execution_budget.charge_instructions(1)?;
                let index = state.next_write;
                let value = if index < sortable {
                    state.items[usize::try_from(index).map_err(|_| {
                        EngineFault::RuntimeInvariant {
                            message: "array sort write index exceeded the item list",
                        }
                    })?]
                    .value
                    .duplicate()
                } else {
                    StoredValue::Undefined
                };
                match state.output {
                    SortOutput::Array {
                        destination: Some(destination),
                    } => {
                        define_sorted_element(runtime, destination, index, value)?;
                        state.next_write = state.next_write.saturating_add(1);
                    }
                    SortOutput::Array { destination: None } => {
                        let key = sort_element_key(runtime, index)?;
                        let name =
                            property_key_name(&key).ok_or(EngineFault::RuntimeInvariant {
                                message: "an array sort index has no diagnostic name",
                            })?;
                        let reference =
                            state
                                .target
                                .heap_reference()
                                .ok_or(EngineFault::RuntimeInvariant {
                                    message: "Array sort ToObject result is not an object",
                                })?;
                        charge_sort_lookup(runtime, &state.target, execution_budget)?;
                        state.stage = ArraySortStage::AwaitWrite;
                        let dispatch = begin_internal_set(
                            runtime,
                            reference,
                            key,
                            name,
                            value,
                            state.target.duplicate(),
                            true,
                            false,
                            state.realm,
                            return_to,
                            state.origin.clone(),
                            execution_budget,
                        )?;
                        await_get!(continue_get_state_after(
                            dispatch,
                            state,
                            array_sort_continuation,
                            "Array sort element [[Set]] produced a structured result",
                        ));
                    }
                    SortOutput::TypedArray {
                        destination,
                        element,
                    } => {
                        typed_array_sorted_store(runtime, destination, index, value, element)?;
                        state.next_write = state.next_write.saturating_add(1);
                    }
                }
            }
            ArraySortStage::AwaitWrite => {
                let _ = take_sort_completion(&mut completion)?;
                state.next_write = state.next_write.saturating_add(1);
                state.stage = ArraySortStage::NextWrite;
            }
            ArraySortStage::NextDelete => {
                if state.next_write >= state.length {
                    state.stage = ArraySortStage::Done;
                    continue;
                }
                execution_budget.charge_instructions(1)?;
                let key = sort_element_key(runtime, state.next_write)?;
                let reference =
                    state
                        .target
                        .heap_reference()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "Array sort ToObject result is not an object",
                        })?;
                charge_sort_lookup(runtime, &state.target, execution_budget)?;
                state.stage = ArraySortStage::AwaitDelete;
                let dispatch = begin_internal_delete(
                    runtime,
                    reference,
                    key,
                    true,
                    false,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                await_get!(continue_get_state_after(
                    dispatch,
                    state,
                    array_sort_continuation,
                    "Array sort element [[Delete]] produced a structured result",
                ));
            }
            ArraySortStage::AwaitDelete => {
                let _ = take_sort_completion(&mut completion)?;
                state.next_write = state.next_write.saturating_add(1);
                state.stage = ArraySortStage::NextDelete;
            }
            ArraySortStage::Done => {
                let result = match state.output {
                    SortOutput::Array {
                        destination: Some(destination),
                    }
                    | SortOutput::TypedArray { destination, .. } => {
                        StoredValue::Object(destination)
                    }
                    SortOutput::Array { destination: None } => state.target.duplicate(),
                };
                return Ok(NativeDispatch::Immediate(result));
            }
        }
    }
}

fn append_sort_value(
    state: &mut ArraySortContinuation,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    if matches!(value, StoredValue::Undefined) {
        state.undefined_count = state.undefined_count.saturating_add(1);
        return Ok(());
    }
    state
        .items
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    state.items.push(SortItem { value, text: None });
    Ok(())
}

/// Applies the default `CompareTypedArrayElements` ordering and returns
/// whether the left item should win the stable merge. Comparator calls use the
/// common `ToNumber` path in `advance_array_sort` instead.
#[expect(
    clippy::float_cmp,
    reason = "ECMA-262 CompareTypedArrayElements requires exact IEEE-754 equality, including signed zero"
)]
fn compare_typed_array_items(
    left: &StoredValue,
    right: &StoredValue,
    element: TypedArrayElementType,
) -> Result<bool, NativeFailure> {
    match (left, right, element.is_bigint()) {
        (StoredValue::Number(left), StoredValue::Number(right), false) => {
            let left = left.as_f64();
            let right = right.as_f64();
            if left.is_nan() {
                return Ok(right.is_nan());
            }
            if right.is_nan() {
                return Ok(true);
            }
            if left == right {
                if left == 0.0 && right == 0.0 {
                    return Ok(left.is_sign_negative() || !right.is_sign_negative());
                }
                return Ok(true);
            }
            Ok(left < right)
        }
        (StoredValue::BigInt(left), StoredValue::BigInt(right), true) => Ok(left <= right),
        _ => Err(EngineFault::RuntimeInvariant {
            message: "TypedArray sort collected a value with the wrong content type",
        }
        .into()),
    }
}

/// Publishes a collected typed-array value without another observable
/// conversion. The source and destination content types were fixed by the
/// initial typed-array validation; an out-of-bounds source after a comparator
/// is the specified no-op integer-indexed write outcome.
fn typed_array_sorted_store(
    runtime: &mut Runtime,
    destination: ObjectId,
    index: u64,
    value: StoredValue,
    element: TypedArrayElementType,
) -> Result<(), NativeFailure> {
    let index = usize::try_from(index).map_err(|_| EngineFault::RuntimeInvariant {
        message: "TypedArray sort output index exceeded the implementation range",
    })?;
    let outcome = match (value, element.is_bigint()) {
        (StoredValue::Number(value), false) => runtime.typed_array_store_index(
            destination,
            index,
            TypedArrayElementValue::Number(value),
        )?,
        (StoredValue::BigInt(value), true) => runtime.typed_array_store_index(
            destination,
            index,
            TypedArrayElementValue::BigInt(value.as_ref()),
        )?,
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "TypedArray sort published a value with the wrong content type",
            }
            .into());
        }
    };
    if outcome == TypedArrayStoreOutcome::ContentTypeMismatch {
        return Err(EngineFault::RuntimeInvariant {
            message: "TypedArray sort destination changed its content type",
        }
        .into());
    }
    Ok(())
}

fn prepare_sort(state: &mut ArraySortContinuation) -> Result<(), NativeFailure> {
    state
        .scratch
        .try_reserve_exact(state.items.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: state.items.len(),
        })?;
    if state.items.len() < 2 {
        state.stage = ArraySortStage::NextWrite;
        return Ok(());
    }
    state.width = 1;
    prepare_merge_run(state, 0);
    state.stage = ArraySortStage::NextMerge;
    Ok(())
}

enum MergeAction {
    Complete,
    Take { index: usize, from_left: bool },
    Compare,
}

fn next_merge_action(state: &mut ArraySortContinuation) -> MergeAction {
    loop {
        if state.left < state.left_end && state.right < state.right_end {
            return MergeAction::Compare;
        }
        if state.left < state.left_end {
            return MergeAction::Take {
                index: state.left,
                from_left: true,
            };
        }
        if state.right < state.right_end {
            return MergeAction::Take {
                index: state.right,
                from_left: false,
            };
        }
        state.merge_start = state.right_end;
        if state.merge_start < state.items.len() {
            prepare_merge_run(state, state.merge_start);
            continue;
        }
        std::mem::swap(&mut state.items, &mut state.scratch);
        state.scratch.clear();
        state.width = state.width.saturating_mul(2);
        if state.width >= state.items.len() {
            return MergeAction::Complete;
        }
        prepare_merge_run(state, 0);
    }
}

fn prepare_merge_run(state: &mut ArraySortContinuation, start: usize) {
    state.merge_start = start;
    state.left = start;
    state.left_end = start.saturating_add(state.width).min(state.items.len());
    state.right = state.left_end;
    state.right_end = state
        .right
        .saturating_add(state.width)
        .min(state.items.len());
}

fn append_merged_item(
    state: &mut ArraySortContinuation,
    index: usize,
    from_left: bool,
) -> Result<(), NativeFailure> {
    state
        .scratch
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    state.scratch.push(state.items[index].duplicate());
    if from_left && index == state.left {
        state.left = state.left.saturating_add(1);
    } else if !from_left && index == state.right {
        state.right = state.right.saturating_add(1);
    } else {
        return Err(EngineFault::RuntimeInvariant {
            message: "array sort selected an item outside the active merge heads",
        }
        .into());
    }
    Ok(())
}

fn finish_sort_comparison(
    state: &mut ArraySortContinuation,
    take_left: bool,
) -> Result<(), NativeFailure> {
    let index = if take_left { state.left } else { state.right };
    append_merged_item(state, index, take_left)?;
    state.stage = ArraySortStage::NextMerge;
    Ok(())
}

fn allocate_sorted_destination(
    runtime: &mut Runtime,
    state: &mut ArraySortContinuation,
) -> Result<(), NativeFailure> {
    let length = u32::try_from(state.length).map_err(|_| {
        sort_type_error_with_kind(
            state.realm,
            &state.origin,
            ExceptionKind::RangeError,
            "invalid array length",
        )
    })?;
    let prototype = runtime.realm_array_prototype(state.realm)?;
    let SortOutput::Array { destination } = &mut state.output else {
        return Err(EngineFault::RuntimeInvariant {
            message: "an Array toSorted allocation used a typed-array output",
        }
        .into());
    };
    *destination = Some(
        runtime.allocate_sparse_array_with_prototype(HeapReference::Object(prototype), length)?,
    );
    Ok(())
}

fn define_sorted_element(
    runtime: &mut Runtime,
    destination: ObjectId,
    index: u64,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    let index = u32::try_from(index).map_err(|_| EngineFault::RuntimeInvariant {
        message: "toSorted output index exceeded the Array-index domain",
    })?;
    let key = ArrayIndex::new(index).ok_or(EngineFault::RuntimeInvariant {
        message: "toSorted output reached the non-index sentinel",
    })?;
    match runtime.define_array_data_property(
        destination,
        PropertyKey::from_index(key),
        PropertyLayout::data(true, true, true),
        value,
    )? {
        ArrayDefineOutcome::Complete => Ok(()),
        ArrayDefineOutcome::ReadOnlyLength | ArrayDefineOutcome::NonExtensible => {
            Err(EngineFault::RuntimeInvariant {
                message: "fresh toSorted destination refused an indexed definition",
            }
            .into())
        }
    }
}

fn convert_sort_value(
    runtime: &mut Runtime,
    state: ArraySortContinuation,
    value: StoredValue,
    hint: OperatorPrimitiveHint,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        value,
        hint,
        OperatorPrimitiveTarget::ArraySortValue(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn array_sort_continuation(state: ArraySortContinuation) -> NativeContinuation {
    NativeContinuation::ArraySort(Box::new(state))
}

fn begin_array_sort_element_get(
    runtime: &mut Runtime,
    mut state: ArraySortContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<GetContinuationDispatch<ArraySortContinuation>, NativeFailure> {
    let index = state.next_read.saturating_sub(1);
    let key = sort_element_key(runtime, index)?;
    charge_sort_lookup(runtime, &state.target, execution_budget)?;
    state.stage = ArraySortStage::AwaitRead;
    let dispatch = begin_value_get(
        runtime,
        &state.target,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_state_after(
        dispatch,
        state,
        array_sort_continuation,
        "Array sort element Get produced a structured result",
    )
}

fn suspend_sort(
    state: ArraySortContinuation,
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
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
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

fn sort_element_key(runtime: &mut Runtime, index: u64) -> Result<PropertyKey, NativeFailure> {
    if let Ok(index) = u32::try_from(index)
        && let Some(index) = ArrayIndex::new(index)
    {
        return Ok(PropertyKey::from_index(index));
    }
    let name = JsNumber::from_f64(length_as_f64(index)).to_javascript_string()?;
    Ok(runtime.property_key_from_string(&name)?)
}

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

const fn needs_sort_conversion(value: &StoredValue) -> bool {
    matches!(value, StoredValue::Function(_) | StoredValue::Object(_))
}

fn take_sort_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        EngineFault::RuntimeInvariant {
            message: "array sort resumed without its awaited completion",
        }
        .into()
    })
}

fn sort_type_error(realm: RealmId, origin: &JsStackFrame, message: &str) -> NativeFailure {
    sort_type_error_with_kind(realm, origin, ExceptionKind::TypeError, message)
}

fn sort_type_error_with_kind(
    realm: RealmId,
    origin: &JsStackFrame,
    kind: ExceptionKind,
    message: &str,
) -> NativeFailure {
    match JsString::from_utf8(message) {
        Ok(message) => NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError { kind, message },
            origin: origin.clone(),
        }),
        Err(error) => error.into(),
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "ToLength bounds every integer by 2^53 - 1, which binary64 represents exactly"
)]
fn length_as_f64(length: u64) -> f64 {
    length as f64
}
