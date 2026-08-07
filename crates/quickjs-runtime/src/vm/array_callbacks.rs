/*
 * JavaScript Array.prototype callback semantics derived from QuickJS.
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

//! The `Array.prototype` methods that take a callback.
//!
//! `forEach`, `map`, `filter`, `every`, `some`, `find`, `findIndex`, `findLast`,
//! and `findLastIndex` share one resumable loop. Unlike the other Array
//! families, the suspension here is intrinsic rather than incidental: the
//! callback *is* a user call on every iteration, so the loop cannot be written
//! any other way.
//!
//! Three behaviors distinguish the members, and all three are carried as data:
//!
//! * **Holes.** `forEach`, `map`, `filter`, `every`, and `some` test
//!   `HasProperty` and skip a missing index, so `[1,,3].forEach` runs twice.
//!   The `find` family visits every index in range, so `[1,,3].find` runs three
//!   times and sees `undefined` in the middle.
//! * **Early exit.** `every` stops on a falsy result, `some` and the `find`
//!   family stop on a truthy one, and `forEach`, `map`, and `filter` never stop
//!   early.
//! * **Result.** `forEach` answers `undefined`, `map` and `filter` build a fresh
//!   Array, `every` and `some` answer a Boolean, `find`/`findLast` answer the
//!   element, and `findIndex`/`findLastIndex` answer the index.
//!
//! The length is read once with `ToLength` before the first callback, so a
//! callback that grows the array is not revisited: the oracle reports
//! `[1,2].forEach(v => { out += v; a.push(9); })` as visiting only `1` and `2`.
//! A callback that shrinks it is still bounded by `HasProperty`, so
//! `[1,2,3].forEach(v => { out += v; a.length = 1; })` visits only `1`.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// Which stage of the loop a continuation resumes into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayCallbackStage {
    /// Awaiting the `length` property read.
    AwaitLength,
    /// Awaiting `ToLength` of the length value.
    AwaitLengthConversion,
    /// Ready to visit the next index.
    NextElement,
    /// Awaiting `HasProperty` for a hole-skipping method.
    AwaitPresence,
    /// Awaiting an element read that may have entered a getter.
    AwaitElement,
    /// Awaiting the callback's result.
    AwaitCallback,
    /// Finished.
    Done,
}

/// One in-progress `Array.prototype` callback loop.
pub(crate) struct ArrayCallbackContinuation {
    method: ArrayCallback,
    /// The coerced receiver whose elements are visited.
    target: StoredValue,
    /// The callback, already verified to be callable.
    callback: FunctionId,
    /// Whether this traversal tests `HasProperty` before reading an index.
    ///
    /// Ordinary array callbacks skip holes for most methods. Typed arrays have
    /// no holes, and—critically for resizable buffers—must still `Get` every
    /// index in the captured range after a later shrink.
    skip_holes: bool,
    /// The `thisArg` the callback receives.
    this_argument: StoredValue,
    /// The element count from the single `ToLength` length read.
    length: u64,
    /// The next index to visit.
    next: u64,
    /// The index currently being visited.
    current: u64,
    /// The element currently being visited, retained for `filter` and `find`.
    element: Option<StoredValue>,
    /// The destination array, present for `map` and `filter`.
    destination: Option<ObjectId>,
    /// The next index to write in the destination.
    written: u64,
    /// The value this method returns.
    result: StoredValue,
    realm: RealmId,
    stage: ArrayCallbackStage,
    origin: JsStackFrame,
}

impl ArrayCallbackContinuation {
    /// The receiver, the `thisArg`, the current element, and the result.
    pub(crate) const fn retained_values() -> u64 {
        4
    }

    /// Reports the traced roots this continuation retains.
    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        trace_stored_value_root(&self.this_argument, mark);
        trace_stored_value_root(&self.result, mark);
        mark(CollectionRoot::Heap(HeapReference::Function(self.callback)));
        if let Some(element) = &self.element {
            trace_stored_value_root(element, mark);
        }
        if let Some(destination) = self.destination {
            mark(CollectionRoot::Heap(HeapReference::Object(destination)));
        }
    }
}

/// Starts one `Array.prototype` callback method.
#[expect(
    clippy::too_many_arguments,
    reason = "one shared entry point carries the method identity alongside the same receiver, arguments, and resumption context every native dispatch takes"
)]
pub(super) fn begin_array_callback(
    runtime: &mut Runtime,
    method: ArrayCallback,
    realm: RealmId,
    receiver: StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    // `ToObject(this)` runs before the callback is checked, so a nullish
    // receiver throws even when the callback is also invalid.
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
    // The callback must be callable before the length is read, which the oracle
    // confirms: `[1].forEach()` throws `not a function`.
    let StoredValue::Function(callback) = arguments.take_first_or_undefined() else {
        return Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("not a function")?,
            },
            origin,
        }));
    };
    let this_argument = arguments.take_first_or_undefined();
    let destination = if method.builds_array() {
        Some(runtime.allocate_array(realm, Vec::new())?)
    } else {
        None
    };
    let state = ArrayCallbackContinuation {
        method,
        target: receiver,
        callback,
        skip_holes: method.skips_holes(),
        this_argument,
        length: 0,
        next: 0,
        current: 0,
        element: None,
        destination,
        written: 0,
        // The default result is each method's "nothing matched" answer.
        result: default_result(method),
        realm,
        stage: ArrayCallbackStage::AwaitLength,
        origin,
    };
    advance_array_callback(runtime, state, None, return_to, execution_budget)
}

/// Starts a non-allocating `%TypedArray%.prototype` callback method.
///
/// The caller has already performed `ValidateTypedArray` and captured the
/// initial `TypedArrayLength`. That ordering deliberately differs from the
/// generic Array entry point: `TypedArray` callbacks validate and capture their
/// length before testing whether the predicate is callable.
#[expect(
    clippy::too_many_arguments,
    reason = "the typed-array entry carries the same native call context as the shared array callback loop"
)]
pub(super) fn begin_typed_array_callback(
    runtime: &mut Runtime,
    method: ArrayCallback,
    realm: RealmId,
    receiver: StoredValue,
    length: u64,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    debug_assert!(
        !method.builds_array(),
        "typed map and filter need their own species-aware destination path"
    );
    let StoredValue::Function(callback) = arguments.take_first_or_undefined() else {
        return Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("not a function")?,
            },
            origin,
        }));
    };
    let state = ArrayCallbackContinuation {
        method,
        target: receiver,
        callback,
        skip_holes: false,
        this_argument: arguments.take_first_or_undefined(),
        length,
        next: if method.is_backward() {
            length.saturating_sub(1)
        } else {
            0
        },
        current: 0,
        element: None,
        destination: None,
        written: 0,
        result: default_result(method),
        realm,
        stage: ArrayCallbackStage::NextElement,
        origin,
    };
    advance_array_callback(runtime, state, None, return_to, execution_budget)
}

/// Resumes the loop after an awaited read or callback.
#[allow(
    clippy::too_many_lines,
    clippy::needless_continue,
    reason = "the length, element, and callback stages form one traced continuation shared by all nine methods"
)]
pub(super) fn advance_array_callback(
    runtime: &mut Runtime,
    mut state: ArrayCallbackContinuation,
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
            ArrayCallbackStage::AwaitLength => {
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                charge_callback_lookup(runtime, &state.target, execution_budget)?;
                state.stage = ArrayCallbackStage::AwaitLengthConversion;
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
                    array_callback_continuation,
                    "Array callback length Get produced a structured result",
                ));
            }
            ArrayCallbackStage::AwaitLengthConversion => {
                // The length is snapshotted once, so a callback that grows the
                // array does not extend the loop.
                let value = take_completion(&mut completion)?;
                let number = operator_to_number(value, state.realm, &state.origin)?;
                state.length = number_to_length(number);
                state.next = if state.method.is_backward() {
                    state.length.saturating_sub(1)
                } else {
                    0
                };
                state.stage = ArrayCallbackStage::NextElement;
            }
            ArrayCallbackStage::NextElement => {
                if state.length == 0 || state.next >= state.length {
                    state.stage = ArrayCallbackStage::Done;
                    continue;
                }
                execution_budget.charge_instructions(1)?;
                let index = state.current_index();
                state.current = index;
                state.advance();

                let key = element_key(index)?;
                // Most methods skip a missing index; the `find` family visits it
                // and sees `undefined`.
                if state.skip_holes {
                    charge_callback_lookup(runtime, &state.target, execution_budget)?;
                    state.stage = ArrayCallbackStage::AwaitPresence;
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
                        array_callback_continuation,
                        "Array callback HasProperty produced a structured result",
                    ));
                }
                await_get!(begin_array_callback_element_get(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                ));
            }
            ArrayCallbackStage::AwaitPresence => {
                if !take_completion(&mut completion)?.is_truthy() {
                    // `map` still counts the hole so the result keeps its shape.
                    if matches!(state.method, ArrayCallback::Map) {
                        state.written = state.written.saturating_add(1);
                    }
                    state.stage = ArrayCallbackStage::NextElement;
                    continue;
                }
                await_get!(begin_array_callback_element_get(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                ));
            }
            ArrayCallbackStage::AwaitElement => {
                let element = take_completion(&mut completion)?;
                // The callback receives `(element, index, array)`.
                let mut callback_arguments = Vec::new();
                callback_arguments.try_reserve_exact(3).map_err(|_| {
                    ExecutionError::AllocationFailed {
                        resource: RuntimeResource::Frames,
                        additional: 3,
                    }
                })?;
                callback_arguments.push(element.duplicate());
                callback_arguments.push(StoredValue::Number(JsNumber::from_f64(index_as_f64(
                    state.current,
                ))));
                callback_arguments.push(state.target.duplicate());
                state.element = Some(element);
                state.stage = ArrayCallbackStage::AwaitCallback;
                let callback = state.callback;
                let receiver = state.this_argument.duplicate();
                return suspend(state, callback, receiver, callback_arguments, return_to);
            }
            ArrayCallbackStage::AwaitCallback => {
                let answer = take_completion(&mut completion)?;
                let element = state.element.take();
                let truthy = answer.is_truthy();
                match state.method {
                    ArrayCallback::ForEach => {}
                    ArrayCallback::Map => {
                        append_element(runtime, &mut state, answer)?;
                    }
                    ArrayCallback::Filter => {
                        if truthy {
                            let element = element.ok_or(EngineFault::RuntimeInvariant {
                                message: "array filter lost the element it was testing",
                            })?;
                            append_element(runtime, &mut state, element)?;
                        }
                    }
                    ArrayCallback::Every => {
                        if !truthy {
                            state.result = StoredValue::Boolean(false);
                            state.stage = ArrayCallbackStage::Done;
                            continue;
                        }
                    }
                    ArrayCallback::Some => {
                        if truthy {
                            state.result = StoredValue::Boolean(true);
                            state.stage = ArrayCallbackStage::Done;
                            continue;
                        }
                    }
                    ArrayCallback::Find | ArrayCallback::FindLast => {
                        if truthy {
                            state.result = element.unwrap_or(StoredValue::Undefined);
                            state.stage = ArrayCallbackStage::Done;
                            continue;
                        }
                    }
                    ArrayCallback::FindIndex | ArrayCallback::FindLastIndex => {
                        if truthy {
                            state.result = StoredValue::Number(JsNumber::from_f64(index_as_f64(
                                state.current,
                            )));
                            state.stage = ArrayCallbackStage::Done;
                            continue;
                        }
                    }
                }
                state.stage = ArrayCallbackStage::NextElement;
            }
            ArrayCallbackStage::Done => {
                return Ok(NativeDispatch::Immediate(match state.destination {
                    Some(destination) => {
                        finish_destination(runtime, &state, destination)?;
                        StoredValue::Object(destination)
                    }
                    None => state.result,
                }));
            }
        }
    }
}

impl ArrayCallbackContinuation {
    /// Returns the index this iteration visits.
    const fn current_index(&self) -> u64 {
        self.next
    }

    /// Moves the cursor past the index just visited.
    fn advance(&mut self) {
        if self.method.is_backward() {
            match self.next.checked_sub(1) {
                Some(next) => self.next = next,
                // Index zero was the last one, so end the loop.
                None => self.length = 0,
            }
        } else {
            self.next = self.next.saturating_add(1);
        }
    }
}

/// Returns each method's answer when nothing matched.
fn default_result(method: ArrayCallback) -> StoredValue {
    match method {
        // `every` on an empty array is `true` and `some` is `false`, which are
        // the identity elements of the two quantifiers.
        ArrayCallback::Every => StoredValue::Boolean(true),
        ArrayCallback::Some => StoredValue::Boolean(false),
        ArrayCallback::FindIndex | ArrayCallback::FindLastIndex => {
            StoredValue::Number(JsNumber::from_i32(-1))
        }
        ArrayCallback::ForEach
        | ArrayCallback::Map
        | ArrayCallback::Filter
        | ArrayCallback::Find
        | ArrayCallback::FindLast => StoredValue::Undefined,
    }
}

/// Appends one element to the destination array.
fn append_element(
    runtime: &mut Runtime,
    state: &mut ArrayCallbackContinuation,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    let Some(destination) = state.destination else {
        return Err(EngineFault::RuntimeInvariant {
            message: "an array callback appended an element with no destination",
        }
        .into());
    };
    let key = element_key(state.written)?;
    state.written = state.written.saturating_add(1);
    match runtime.define_array_data_property(
        destination,
        key,
        PropertyLayout::data(true, true, true),
        value,
    )? {
        ArrayDefineOutcome::Complete => Ok(()),
        ArrayDefineOutcome::ReadOnlyLength | ArrayDefineOutcome::NonExtensible => {
            Err(EngineFault::RuntimeInvariant {
                message: "a freshly allocated destination array refused an element",
            }
            .into())
        }
    }
}

/// Sets the destination's final length.
///
/// `map` writes one slot per source index, including the holes it skipped, so
/// its result keeps the source's shape. `filter` writes only what it kept.
fn finish_destination(
    runtime: &mut Runtime,
    state: &ArrayCallbackContinuation,
    destination: ObjectId,
) -> Result<(), NativeFailure> {
    let length = u32::try_from(state.written).map_err(|_| EngineFault::RuntimeInvariant {
        message: "an array callback produced a length outside the array-index domain",
    })?;
    match runtime.set_array_length(destination, length)? {
        ArrayLengthWriteOutcome::Complete
        | ArrayLengthWriteOutcome::BlockedByNonConfigurable { .. } => Ok(()),
        ArrayLengthWriteOutcome::ReadOnly => Err(EngineFault::RuntimeInvariant {
            message: "a freshly allocated destination array refused its length",
        }
        .into()),
    }
}

/// Suspends into a call that resumes this continuation.
fn suspend(
    state: ArrayCallbackContinuation,
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
    continuations.push(NativeContinuation::ArrayCallback(Box::new(state)));
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

fn array_callback_continuation(state: ArrayCallbackContinuation) -> NativeContinuation {
    NativeContinuation::ArrayCallback(Box::new(state))
}

fn begin_array_callback_element_get(
    runtime: &mut Runtime,
    mut state: ArrayCallbackContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<GetContinuationDispatch<ArrayCallbackContinuation>, NativeFailure> {
    let key = element_key(state.current)?;
    charge_callback_lookup(runtime, &state.target, execution_budget)?;
    state.stage = ArrayCallbackStage::AwaitElement;
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
        array_callback_continuation,
        "Array callback element Get produced a structured result",
    )
}

/// Charges one property lookup, tolerating a primitive receiver.
fn charge_callback_lookup(
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

/// Converts an index to binary64.
///
/// `ToLength` bounds every index by `2^53 - 1`, so the conversion is exact.
#[expect(
    clippy::cast_precision_loss,
    reason = "ToLength bounds every index by 2^53 - 1, which binary64 represents exactly"
)]
fn index_as_f64(index: u64) -> f64 {
    index as f64
}

/// Returns the property key for one element index.
fn element_key(index: u64) -> Result<PropertyKey, NativeFailure> {
    let index = u32::try_from(index).map_err(|_| EngineFault::RuntimeInvariant {
        message: "array callback index exceeded the array-index domain",
    })?;
    let index = ArrayIndex::new(index).ok_or(EngineFault::RuntimeInvariant {
        message: "array callback index reached the non-index sentinel",
    })?;
    Ok(PropertyKey::from_index(index))
}

/// Extracts the awaited completion value.
fn take_completion(completion: &mut Option<StoredValue>) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        NativeFailure::Execution(
            EngineFault::RuntimeInvariant {
                message: "an array callback resumed without its awaited completion",
            }
            .into(),
        )
    })
}

/// Which stage of a reduction a continuation resumes into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayReductionStage {
    /// Awaiting the `length` property read.
    AwaitLength,
    /// Awaiting `ToLength` of the length value.
    AwaitLengthConversion,
    /// Looking for the first present element to seed the accumulator.
    SeedAccumulator,
    /// Awaiting `HasProperty` while locating the seed element.
    AwaitSeedPresence,
    /// Awaiting the seed element's read.
    AwaitSeedRead,
    /// Ready to visit the next index.
    NextElement,
    /// Awaiting `HasProperty` for one reduction element.
    AwaitElementPresence,
    /// Awaiting an element read that may have entered a getter.
    AwaitElement,
    /// Awaiting the callback's result, which becomes the accumulator.
    AwaitCallback,
    /// Finished.
    Done,
}

/// One in-progress `Array.prototype` reduction.
pub(crate) struct ArrayReductionContinuation {
    reduction: ArrayReduction,
    /// The coerced receiver whose elements are folded.
    target: StoredValue,
    /// The callback, already verified to be callable.
    callback: FunctionId,
    /// Whether this reduction performs `HasProperty` before each element read.
    ///
    /// Ordinary Array reductions skip holes. Typed arrays have no holes and
    /// must instead perform a fresh `Get` for every index in their previously
    /// captured length, even after a resizable backing buffer has shrunk.
    skip_holes: bool,
    /// The accumulator, absent until it is seeded.
    accumulator: Option<StoredValue>,
    /// The element count from the single `ToLength` length read.
    length: u64,
    /// The next index to visit.
    next: u64,
    /// The index currently being visited.
    current: u64,
    realm: RealmId,
    stage: ArrayReductionStage,
    origin: JsStackFrame,
}

impl ArrayReductionContinuation {
    /// The receiver and the accumulator.
    pub(crate) const fn retained_values() -> u64 {
        2
    }

    /// Reports the traced roots this continuation retains.
    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        mark(CollectionRoot::Heap(HeapReference::Function(self.callback)));
        if let Some(accumulator) = &self.accumulator {
            trace_stored_value_root(accumulator, mark);
        }
    }

    /// Moves the cursor past the index just visited.
    fn advance(&mut self) {
        if self.reduction.is_backward() {
            match self.next.checked_sub(1) {
                Some(next) => self.next = next,
                // Index zero was the last one, so end the loop.
                None => self.length = 0,
            }
        } else {
            self.next = self.next.saturating_add(1);
        }
    }
}

/// Starts one `Array.prototype` reduction.
#[expect(
    clippy::too_many_arguments,
    reason = "one shared entry point carries the reduction identity alongside the same receiver, arguments, and resumption context every native dispatch takes"
)]
pub(super) fn begin_array_reduction(
    runtime: &mut Runtime,
    reduction: ArrayReduction,
    realm: RealmId,
    receiver: StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
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
    let StoredValue::Function(callback) = arguments.take_first_or_undefined() else {
        return Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("not a function")?,
            },
            origin,
        }));
    };
    // An absent initial value is distinct from an explicit `undefined` one: the
    // former seeds from the first present element while the latter is the
    // accumulator.
    let accumulator = arguments.take_first();
    let state = ArrayReductionContinuation {
        reduction,
        target: receiver,
        callback,
        skip_holes: true,
        accumulator,
        length: 0,
        next: 0,
        current: 0,
        realm,
        stage: ArrayReductionStage::AwaitLength,
        origin,
    };
    advance_array_reduction(runtime, state, None, return_to, execution_budget)
}

/// Starts a `%TypedArray%.prototype.reduce` or `reduceRight` operation.
///
/// The caller has already completed `ValidateTypedArray` and captured the
/// length. That is deliberately before `IsCallable(callback)`, and direct
/// `Get` operations replace ordinary array hole checks for the whole captured
/// range.
#[expect(
    clippy::too_many_arguments,
    reason = "the typed-array entry carries the shared reduction state and the native call context explicitly"
)]
pub(super) fn begin_typed_array_reduction(
    runtime: &mut Runtime,
    reduction: ArrayReduction,
    realm: RealmId,
    receiver: StoredValue,
    length: usize,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(callback) = arguments.take_first_or_undefined() else {
        return Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("not a function")?,
            },
            origin,
        }));
    };
    // An omitted initial value differs from an explicit `undefined`: it makes
    // the first (or last) element the accumulator without invoking callback.
    let accumulator = arguments.take_first();
    let has_initial = accumulator.is_some();
    let length = usize_to_u64(length);
    let state = ArrayReductionContinuation {
        reduction,
        target: receiver,
        callback,
        skip_holes: false,
        accumulator,
        length,
        next: if reduction.is_backward() {
            length.saturating_sub(1)
        } else {
            0
        },
        current: 0,
        realm,
        stage: if has_initial {
            ArrayReductionStage::NextElement
        } else {
            ArrayReductionStage::SeedAccumulator
        },
        origin,
    };
    advance_array_reduction(runtime, state, None, return_to, execution_budget)
}

/// Resumes a reduction after an awaited read or callback.
#[allow(
    clippy::too_many_lines,
    clippy::needless_continue,
    reason = "the length, seeding, element, and callback stages form one traced continuation shared by both reductions"
)]
pub(super) fn advance_array_reduction(
    runtime: &mut Runtime,
    mut state: ArrayReductionContinuation,
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
            ArrayReductionStage::AwaitLength => {
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                charge_callback_lookup(runtime, &state.target, execution_budget)?;
                state.stage = ArrayReductionStage::AwaitLengthConversion;
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
                    array_reduction_continuation,
                    "Array reduction length Get produced a structured result",
                ));
            }
            ArrayReductionStage::AwaitLengthConversion => {
                let value = take_completion(&mut completion)?;
                let number = operator_to_number(value, state.realm, &state.origin)?;
                state.length = number_to_length(number);
                state.next = if state.reduction.is_backward() {
                    state.length.saturating_sub(1)
                } else {
                    0
                };
                state.stage = if state.accumulator.is_some() {
                    ArrayReductionStage::NextElement
                } else {
                    ArrayReductionStage::SeedAccumulator
                };
            }
            ArrayReductionStage::SeedAccumulator => {
                // Without an initial value the accumulator is the first *present*
                // element, so holes before it are skipped rather than folded.
                if state.length == 0 || state.next >= state.length {
                    // An empty or all-holes array has nothing to seed from.
                    return Err(NativeFailure::Abrupt(PendingException {
                        realm: state.realm,
                        payload: PendingExceptionPayload::EngineError {
                            kind: ExceptionKind::TypeError,
                            message: JsString::from_utf8("empty array")?,
                        },
                        origin: state.origin.clone(),
                    }));
                }
                execution_budget.charge_instructions(1)?;
                let index = state.next;
                state.current = index;
                state.advance();
                if state.skip_holes {
                    let key = element_key(index)?;
                    charge_callback_lookup(runtime, &state.target, execution_budget)?;
                    state.stage = ArrayReductionStage::AwaitSeedPresence;
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
                        array_reduction_continuation,
                        "Array reduction seed HasProperty produced a structured result",
                    ));
                }
                await_get!(begin_array_reduction_element_get(
                    runtime,
                    state,
                    ArrayReductionStage::AwaitSeedRead,
                    return_to,
                    execution_budget,
                ));
            }
            ArrayReductionStage::AwaitSeedPresence => {
                if !take_completion(&mut completion)?.is_truthy() {
                    state.stage = ArrayReductionStage::SeedAccumulator;
                    continue;
                }
                await_get!(begin_array_reduction_element_get(
                    runtime,
                    state,
                    ArrayReductionStage::AwaitSeedRead,
                    return_to,
                    execution_budget,
                ));
            }
            ArrayReductionStage::AwaitSeedRead => {
                state.accumulator = Some(take_completion(&mut completion)?);
                state.stage = ArrayReductionStage::NextElement;
            }
            ArrayReductionStage::NextElement => {
                if state.length == 0 || state.next >= state.length {
                    state.stage = ArrayReductionStage::Done;
                    continue;
                }
                execution_budget.charge_instructions(1)?;
                let index = state.next;
                state.current = index;
                state.advance();
                if state.skip_holes {
                    let key = element_key(index)?;
                    charge_callback_lookup(runtime, &state.target, execution_budget)?;
                    // A hole is skipped: the callback never sees it.
                    state.stage = ArrayReductionStage::AwaitElementPresence;
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
                        array_reduction_continuation,
                        "Array reduction HasProperty produced a structured result",
                    ));
                }
                await_get!(begin_array_reduction_element_get(
                    runtime,
                    state,
                    ArrayReductionStage::AwaitElement,
                    return_to,
                    execution_budget,
                ));
            }
            ArrayReductionStage::AwaitElementPresence => {
                if !take_completion(&mut completion)?.is_truthy() {
                    state.stage = ArrayReductionStage::NextElement;
                    continue;
                }
                await_get!(begin_array_reduction_element_get(
                    runtime,
                    state,
                    ArrayReductionStage::AwaitElement,
                    return_to,
                    execution_budget,
                ));
            }
            ArrayReductionStage::AwaitElement => {
                let element = take_completion(&mut completion)?;
                let accumulator =
                    state
                        .accumulator
                        .take()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "an array reduction folded an element without an accumulator",
                        })?;
                // The callback receives `(accumulator, element, index, array)`.
                let mut callback_arguments = Vec::new();
                callback_arguments.try_reserve_exact(4).map_err(|_| {
                    ExecutionError::AllocationFailed {
                        resource: RuntimeResource::Frames,
                        additional: 4,
                    }
                })?;
                callback_arguments.push(accumulator);
                callback_arguments.push(element);
                callback_arguments.push(StoredValue::Number(JsNumber::from_f64(index_as_f64(
                    state.current,
                ))));
                callback_arguments.push(state.target.duplicate());
                state.stage = ArrayReductionStage::AwaitCallback;
                let callback = state.callback;
                return suspend_reduction(
                    state,
                    callback,
                    StoredValue::Undefined,
                    callback_arguments,
                    return_to,
                );
            }
            ArrayReductionStage::AwaitCallback => {
                // The callback's result replaces the accumulator.
                state.accumulator = Some(take_completion(&mut completion)?);
                state.stage = ArrayReductionStage::NextElement;
            }
            ArrayReductionStage::Done => {
                return Ok(NativeDispatch::Immediate(
                    state.accumulator.take().unwrap_or(StoredValue::Undefined),
                ));
            }
        }
    }
}

fn array_reduction_continuation(state: ArrayReductionContinuation) -> NativeContinuation {
    NativeContinuation::ArrayReduction(Box::new(state))
}

fn begin_array_reduction_element_get(
    runtime: &mut Runtime,
    mut state: ArrayReductionContinuation,
    next_stage: ArrayReductionStage,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<GetContinuationDispatch<ArrayReductionContinuation>, NativeFailure> {
    let key = element_key(state.current)?;
    charge_callback_lookup(runtime, &state.target, execution_budget)?;
    state.stage = next_stage;
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
        array_reduction_continuation,
        "Array reduction element Get produced a structured result",
    )
}

/// Suspends into a call that resumes this reduction.
fn suspend_reduction(
    state: ArrayReductionContinuation,
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
    continuations.push(NativeContinuation::ArrayReduction(Box::new(state)));
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
