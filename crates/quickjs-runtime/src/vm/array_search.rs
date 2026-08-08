/*
 * JavaScript Array.prototype search semantics derived from QuickJS.
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

//! `Array.prototype.indexOf`, `lastIndexOf`, and `includes`.
//!
//! All three are one resumable element loop, because every element read can run
//! a getter. They share that loop but differ in two observable ways, which is
//! why the differences are data rather than separate implementations:
//!
//! * **Comparison.** `indexOf` and `lastIndexOf` use strict equality, so
//!   `[NaN].indexOf(NaN)` is `-1`. `includes` uses `SameValueZero`, so
//!   `[NaN].includes(NaN)` is `true`. Both treat `+0` and `-0` as equal, so
//!   `[-0].indexOf(0)` is `0`.
//! * **Holes.** `indexOf` and `lastIndexOf` test `HasProperty` first and skip a
//!   missing index, so `[1,,3].indexOf(undefined)` is `-1`. `includes` reads
//!   every index, so a hole compares as `undefined` and
//!   `[1,,3].includes(undefined)` is `true`.
//!
//! The loop stops at the first match, which the pinned oracle confirms: with
//! two matching getters, `indexOf` runs only the first. The length is read once
//! with `ToLength` before any element.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// Which stage of the search loop a continuation resumes into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ArraySearchStage {
    /// Awaiting the `length` property read.
    AwaitLength,
    /// Awaiting `ToLength` of the length value.
    AwaitLengthConversion,
    /// Awaiting `ToIntegerOrInfinity` of the start position.
    AwaitPosition,
    /// Ready to visit the next index.
    NextElement,
    /// Awaiting `HasProperty` for an index-based search.
    AwaitPresence,
    /// Awaiting an element read, which may have entered a getter.
    AwaitElement,
}

/// One in-progress `Array.prototype` search.
pub(super) struct ArraySearchContinuation {
    search: ArraySearch,
    /// The coerced receiver whose elements are searched.
    target: StoredValue,
    /// The value being looked for.
    needle: StoredValue,
    /// The unconverted position argument, if one was supplied.
    position: Option<StoredValue>,
    /// The element count from the single `ToLength` length read.
    length: u64,
    /// The next index to visit.
    next: u64,
    /// The index most recently read, retained so a resumed getter knows it.
    current: u64,
    /// Whether the direct Array receiver's prototype chain was just proven to
    /// have no indexed properties. The proof is discarded before each element
    /// Get because an accessor can mutate the chain.
    own_presence_only: bool,
    realm: RealmId,
    stage: ArraySearchStage,
    origin: JsStackFrame,
}

impl ArraySearchContinuation {
    /// The receiver, the needle, and the pending position argument.
    pub(super) const fn retained_values() -> u64 {
        3
    }

    /// Reports the traced roots this continuation retains.
    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        trace_stored_value_root(&self.needle, mark);
        if let Some(position) = &self.position {
            trace_stored_value_root(position, mark);
        }
    }
}

/// Starts one `Array.prototype` search.
#[expect(
    clippy::too_many_arguments,
    reason = "one shared entry point carries the search identity and needle alongside the same receiver and resumption context every native dispatch takes"
)]
pub(super) fn begin_array_search(
    runtime: &mut Runtime,
    search: ArraySearch,
    realm: RealmId,
    receiver: StoredValue,
    needle: StoredValue,
    position: Option<StoredValue>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    // The search begins with `ToObject(this)`, so a nullish receiver throws
    // before the length is read and a primitive receiver's indexed properties
    // are observed through its realm-owned wrapper.
    let receiver = match to_object_value(runtime, realm, receiver, origin.clone())? {
        Ok(receiver) => receiver,
        Err(exception) => return Err(NativeFailure::Abrupt(exception)),
    };
    let state = ArraySearchContinuation {
        search,
        target: receiver,
        needle,
        position,
        length: 0,
        next: 0,
        current: 0,
        own_presence_only: false,
        realm,
        stage: ArraySearchStage::AwaitLength,
        origin,
    };
    advance_array_search(runtime, state, None, return_to, execution_budget)
}

/// Resumes the search loop.
#[allow(
    clippy::too_many_lines,
    clippy::needless_continue,
    reason = "the length, position, and element stages form one traced continuation shared by all three searches"
)]
pub(super) fn advance_array_search(
    runtime: &mut Runtime,
    mut state: ArraySearchContinuation,
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
            ArraySearchStage::AwaitLength => {
                let length_key = runtime.predefined_property_key(PredefinedAtom::Length);
                charge_search_lookup(runtime, &state.target, execution_budget)?;
                state.stage = ArraySearchStage::AwaitLengthConversion;
                let dispatch = begin_value_get(
                    runtime,
                    &state.target,
                    length_key,
                    None,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                await_get!(continue_get_state_after(
                    dispatch,
                    state,
                    array_search_continuation,
                    "Array search length Get produced a structured result",
                ));
            }
            ArraySearchStage::AwaitLengthConversion => {
                // The length is read once, before any element, and every later
                // index derives from it.
                let value = take_completion(&mut completion)?;
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::Number,
                        OperatorPrimitiveTarget::ArraySearchPosition(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                let number = operator_to_number(value, state.realm, &state.origin)?;
                state.length = number_to_length(number);
                state.stage = ArraySearchStage::AwaitPosition;
            }
            ArraySearchStage::AwaitPosition => {
                // An absent position needs no conversion at all.
                let Some(position) = state.position.take() else {
                    bound_search(&mut state);
                    continue;
                };
                if matches!(position, StoredValue::Function(_) | StoredValue::Object(_)) {
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    // Put the value back so the resumed stage converts it.
                    state.position = None;
                    return begin_operator_primitive_conversion(
                        runtime,
                        position,
                        OperatorPrimitiveHint::Number,
                        OperatorPrimitiveTarget::ArraySearchPosition(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                completion = Some(position);
                let value = take_completion(&mut completion)?;
                apply_position(&mut state, value)?;
            }
            ArraySearchStage::NextElement => {
                let Some(index) = next_index(&state) else {
                    return Ok(NativeDispatch::Immediate(missing_result(state.search)));
                };
                execution_budget.charge_instructions(1)?;
                state.current = index;
                advance_cursor(&mut state);

                let key = element_key(index)?;
                // Only the index-based searches skip a missing element, which is
                // what makes `[1,,3].indexOf(undefined)` and
                // `[1,,3].includes(undefined)` disagree.
                if state.search.skips_holes() {
                    if let Some(array) = array_search_own_presence_array(runtime, &mut state)? {
                        if runtime.array_own_property(array, &key)?.is_none() {
                            state.stage = ArraySearchStage::NextElement;
                            continue;
                        }
                        // The following Get can enter an accessor, so a later
                        // iteration must revalidate inherited indexes.
                        state.own_presence_only = false;
                        await_get!(begin_array_search_element_get(
                            runtime,
                            state,
                            return_to,
                            execution_budget,
                        ));
                    }
                    charge_search_lookup(runtime, &state.target, execution_budget)?;
                    state.stage = ArraySearchStage::AwaitPresence;
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
                        array_search_continuation,
                        "Array search HasProperty produced a structured result",
                    ));
                }
                await_get!(begin_array_search_element_get(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                ));
            }
            ArraySearchStage::AwaitPresence => {
                if !take_completion(&mut completion)?.is_truthy() {
                    state.stage = ArraySearchStage::NextElement;
                    continue;
                }
                await_get!(begin_array_search_element_get(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                ));
            }
            ArraySearchStage::AwaitElement => {
                let value = take_completion(&mut completion)?;
                if matches(state.search, &state.needle, &value) {
                    return Ok(NativeDispatch::Immediate(found_result(
                        state.search,
                        state.current,
                    )));
                }
                state.stage = ArraySearchStage::NextElement;
            }
        }
    }
}

/// Applies a converted position argument and enters the element loop.
fn apply_position(
    state: &mut ArraySearchContinuation,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    let number = operator_to_number(value, state.realm, &state.origin)?;
    let integer = number_to_integer_or_infinity(number);
    let length = state.length;
    // A negative position counts from the end; the two directions clamp
    // differently, which is why `[1,2,3].indexOf(3,-1)` is `2` while
    // `[1,2,1].lastIndexOf(1,1)` is `0`.
    if state.search.is_backward() {
        let last = length.saturating_sub(1);
        let start = if integer < 0.0 {
            let resolved = integer + saturating_as_f64(length);
            if resolved < 0.0 {
                // Nothing is in range, so the loop ends immediately.
                state.length = 0;
                state.next = 0;
                state.stage = ArraySearchStage::NextElement;
                return Ok(());
            }
            clamp_to_u64(resolved, last)
        } else {
            clamp_to_u64(integer, last)
        };
        state.next = start;
    } else {
        let start = if integer < 0.0 {
            let resolved = integer + saturating_as_f64(length);
            if resolved < 0.0 {
                0
            } else {
                clamp_to_u64(resolved, length)
            }
        } else {
            clamp_to_u64(integer, length)
        };
        state.next = start;
    }
    state.stage = ArraySearchStage::NextElement;
    Ok(())
}

/// Positions the cursor for a search that received no position argument.
fn bound_search(state: &mut ArraySearchContinuation) {
    state.next = if state.search.is_backward() {
        state.length.saturating_sub(1)
    } else {
        0
    };
    state.stage = ArraySearchStage::NextElement;
}

/// Returns the next index to visit, or `None` when the loop is finished.
fn next_index(state: &ArraySearchContinuation) -> Option<u64> {
    // The backward loop ends by zeroing `length` once index zero is visited,
    // so both directions share the same bound.
    (state.next < state.length).then_some(state.next)
}

/// Moves the cursor past the index just visited.
fn advance_cursor(state: &mut ArraySearchContinuation) {
    if state.search.is_backward() {
        match state.next.checked_sub(1) {
            Some(next) => state.next = next,
            // Index zero was the last one to visit, so end the loop.
            None => state.length = 0,
        }
    } else {
        state.next = state.next.saturating_add(1);
    }
}

/// Compares one element against the needle using the search's equality.
fn matches(search: ArraySearch, needle: &StoredValue, element: &StoredValue) -> bool {
    if search.answers_boolean() {
        // `SameValueZero` treats two `NaN`s as equal, which is the whole reason
        // `includes` exists alongside `indexOf`.
        needle.same_value_zero(element)
    } else {
        needle.strict_equals(element)
    }
}

/// Returns the result for a match at `index`.
fn found_result(search: ArraySearch, index: u64) -> StoredValue {
    if search.answers_boolean() {
        return StoredValue::Boolean(true);
    }
    StoredValue::Number(JsNumber::from_f64(saturating_as_f64(index)))
}

/// Returns the result when no element matched.
fn missing_result(search: ArraySearch) -> StoredValue {
    if search.answers_boolean() {
        return StoredValue::Boolean(false);
    }
    StoredValue::Number(JsNumber::from_i32(-1))
}

/// Clamps an already-truncated integer into `0..=maximum`.
fn clamp_to_u64(value: f64, maximum: u64) -> u64 {
    if value <= 0.0 {
        return 0;
    }
    let maximum_as_f64 = saturating_as_f64(maximum);
    if value >= maximum_as_f64 {
        return maximum;
    }
    // The bounds prove the value is a non-negative integer below `maximum`.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the preceding bounds keep the value inside the u64 domain exactly"
    )]
    let clamped = value as u64;
    clamped
}

/// Converts a length or index to binary64.
///
/// Every value here is at most `MAX_SAFE_INTEGER`, so the conversion is exact.
#[expect(
    clippy::cast_precision_loss,
    reason = "ToLength bounds every value by 2^53 - 1, which binary64 represents exactly"
)]
fn saturating_as_f64(value: u64) -> f64 {
    value as f64
}

/// Charges one property lookup against the budget.
///
/// A primitive receiver such as a String has no heap node of its own, so the
/// walk it would charge for does not exist; one instruction still accounts for
/// the lookup itself.
fn charge_search_lookup(
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

/// Returns the direct Array whose next `HasProperty` can be decided from its
/// own indexed storage. The cached proof is invalidated before any accessor
/// could run, so inherited indexes added by that accessor are still observed.
fn array_search_own_presence_array(
    runtime: &Runtime,
    state: &mut ArraySearchContinuation,
) -> Result<Option<ObjectId>, NativeFailure> {
    let StoredValue::Object(array) = state.target else {
        return Ok(None);
    };
    if !runtime.is_array_object(array)? {
        return Ok(None);
    }
    if state.own_presence_only {
        return Ok(Some(array));
    }
    let mut current = runtime
        .object_record(HeapReference::Object(array))?
        .prototype();
    while let Some(reference) = current {
        if !runtime.has_static_indexed_properties(reference)?
            || runtime.heap_has_indexed_own_property(reference)?
        {
            return Ok(None);
        }
        current = runtime.object_record(reference)?.prototype();
    }
    state.own_presence_only = true;
    Ok(Some(array))
}

fn array_search_continuation(state: ArraySearchContinuation) -> NativeContinuation {
    NativeContinuation::ArraySearch(Box::new(state))
}

fn begin_array_search_element_get(
    runtime: &mut Runtime,
    mut state: ArraySearchContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<GetContinuationDispatch<ArraySearchContinuation>, NativeFailure> {
    let key = element_key(state.current)?;
    charge_search_lookup(runtime, &state.target, execution_budget)?;
    state.stage = ArraySearchStage::AwaitElement;
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
        array_search_continuation,
        "Array search element Get produced a structured result",
    )
}

/// Returns the property key for one element index.
fn element_key(index: u64) -> Result<PropertyKey, NativeFailure> {
    let index = u32::try_from(index).map_err(|_| EngineFault::RuntimeInvariant {
        message: "array search index exceeded the array-index domain",
    })?;
    let index = ArrayIndex::new(index).ok_or(EngineFault::RuntimeInvariant {
        message: "array search index reached the non-index sentinel",
    })?;
    Ok(PropertyKey::from_index(index))
}

/// Extracts the awaited completion value.
fn take_completion(completion: &mut Option<StoredValue>) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        NativeFailure::Execution(
            EngineFault::RuntimeInvariant {
                message: "an array search resumed without its awaited completion",
            }
            .into(),
        )
    })
}
