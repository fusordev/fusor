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

/// Resumes the loop after an awaited read or callback.
#[allow(
    clippy::too_many_lines,
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
    loop {
        match state.stage {
            ArrayCallbackStage::AwaitLength => {
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                charge_callback_lookup(runtime, &state.target, execution_budget)?;
                match read_static_property(runtime, state.realm, &state.target, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        completion = Some(value);
                        state.stage = ArrayCallbackStage::AwaitLengthConversion;
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.stage = ArrayCallbackStage::AwaitLengthConversion;
                        return suspend(state, function, receiver, Vec::new(), return_to);
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(callback_failure(&state, failure));
                    }
                }
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
                charge_callback_lookup(runtime, &state.target, execution_budget)?;
                // Most methods skip a missing index; the `find` family visits it
                // and sees `undefined`.
                if state.method.skips_holes()
                    && !has_property(runtime, state.realm, &state.target, &key)?
                {
                    // `map` still counts the hole so the result keeps its shape.
                    if matches!(state.method, ArrayCallback::Map) {
                        state.written = state.written.saturating_add(1);
                    }
                    continue;
                }
                match read_static_property(runtime, state.realm, &state.target, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        completion = Some(value);
                        state.stage = ArrayCallbackStage::AwaitElement;
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.stage = ArrayCallbackStage::AwaitElement;
                        return suspend(state, function, receiver, Vec::new(), return_to);
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(callback_failure(&state, failure));
                    }
                }
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

/// Builds the exception a failed property read reports.
fn callback_failure(state: &ArrayCallbackContinuation, failure: PropertyFailure) -> NativeFailure {
    match property_exception_at(state.realm, state.origin.clone(), None, failure) {
        Ok(exception) => NativeFailure::Abrupt(exception),
        Err(error) => error.into(),
    }
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
