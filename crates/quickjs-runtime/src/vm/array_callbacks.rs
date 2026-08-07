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
    /// Chooses the `ArraySpeciesCreate` result for `map` or `filter`.
    SelectSpecies,
    /// Awaiting the source Array's `constructor` property.
    AwaitConstructor,
    /// Awaiting the source constructor's `@@species` property.
    AwaitSpecies,
    /// Awaiting a custom species construction.
    AwaitSpeciesConstruct,
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
    /// The callback value. It is retained through `LengthOfArrayLike`, then
    /// checked for callability as the specification requires.
    callback: StoredValue,
    /// Whether this traversal tests `HasProperty` before reading an index.
    ///
    /// Ordinary array callbacks skip holes for most methods. Typed arrays have
    /// no holes, and—critically for resizable buffers—must still `Get` every
    /// index in the captured range after a later shrink.
    skip_holes: bool,
    /// Whether the direct Array receiver's prototype chain was just proven to
    /// contain no indexed properties. The proof is discarded before every
    /// user-observable element read, so a getter or callback can still install
    /// an inherited index for the next iteration.
    own_presence_only: bool,
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
    /// The `ArraySpeciesCreate` destination, present for `map` and `filter`.
    destination: Option<StoredValue>,
    /// The next index to write in the destination.
    written: u64,
    /// The value this method returns.
    result: StoredValue,
    realm: RealmId,
    stage: ArrayCallbackStage,
    origin: JsStackFrame,
}

impl ArrayCallbackContinuation {
    /// The receiver, callback, `thisArg`, current element, and result.
    pub(crate) const fn retained_values() -> u64 {
        5
    }

    /// Reports the traced roots this continuation retains.
    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        trace_stored_value_root(&self.callback, mark);
        trace_stored_value_root(&self.this_argument, mark);
        trace_stored_value_root(&self.result, mark);
        if let Some(element) = &self.element {
            trace_stored_value_root(element, mark);
        }
        if let Some(destination) = &self.destination {
            trace_stored_value_root(destination, mark);
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
    // receiver throws even when the callback is also invalid. Retaining the
    // resulting wrapper also matters: the callback's third argument is `O`,
    // not the original primitive receiver.
    let receiver = match to_object_value(runtime, realm, receiver, origin.clone())? {
        Ok(receiver) => receiver,
        Err(exception) => return Err(NativeFailure::Abrupt(exception)),
    };
    let callback = arguments.take_first_or_undefined();
    let this_argument = arguments.take_first_or_undefined();
    let state = ArrayCallbackContinuation {
        method,
        target: receiver,
        callback,
        skip_holes: method.skips_holes(),
        own_presence_only: false,
        this_argument,
        length: 0,
        next: 0,
        current: 0,
        element: None,
        destination: None,
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
        callback: StoredValue::Function(callback),
        skip_holes: false,
        own_presence_only: false,
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
                if needs_conversion(&value) {
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::Number,
                        OperatorPrimitiveTarget::ArrayCallbackLength(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                let number = operator_to_number(value, state.realm, &state.origin)?;
                state.length = number_to_length(number);
                let StoredValue::Function(_) = state.callback else {
                    return Err(NativeFailure::Abrupt(PendingException {
                        realm: state.realm,
                        payload: PendingExceptionPayload::EngineError {
                            kind: ExceptionKind::TypeError,
                            message: JsString::from_utf8("not a function")?,
                        },
                        origin: state.origin.clone(),
                    }));
                };
                state.next = if state.method.is_backward() {
                    state.length.saturating_sub(1)
                } else {
                    0
                };
                state.stage = if state.method.builds_array() {
                    ArrayCallbackStage::SelectSpecies
                } else {
                    ArrayCallbackStage::NextElement
                };
            }
            ArrayCallbackStage::SelectSpecies => {
                if !proxy_aware_is_array(
                    runtime,
                    state.target.duplicate(),
                    state.realm,
                    state.origin.clone(),
                )? {
                    allocate_callback_destination(runtime, &mut state)?;
                    state.stage = ArrayCallbackStage::NextElement;
                    continue;
                }
                let key = runtime.predefined_property_key(PredefinedAtom::Constructor);
                charge_callback_lookup(runtime, &state.target, execution_budget)?;
                state.stage = ArrayCallbackStage::AwaitConstructor;
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
                    "Array callback constructor Get produced a structured result",
                ));
            }
            ArrayCallbackStage::AwaitConstructor => {
                let constructor = take_completion(&mut completion)?;
                if let StoredValue::Function(function) = constructor
                    && function_is_constructor(runtime, function)?
                {
                    let constructor_realm = runtime.function_realm(function)?;
                    if constructor_realm != state.realm
                        && function == runtime.realm_array_constructor(constructor_realm)?
                    {
                        allocate_callback_destination(runtime, &mut state)?;
                        state.stage = ArrayCallbackStage::NextElement;
                        continue;
                    }
                }
                if matches!(
                    constructor,
                    StoredValue::Function(_) | StoredValue::Object(_)
                ) {
                    let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolSpecies);
                    charge_callback_lookup(runtime, &constructor, execution_budget)?;
                    state.stage = ArrayCallbackStage::AwaitSpecies;
                    let dispatch = begin_value_get(
                        runtime,
                        &constructor,
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
                        "Array callback species Get produced a structured result",
                    ));
                } else if matches!(constructor, StoredValue::Undefined) {
                    allocate_callback_destination(runtime, &mut state)?;
                    state.stage = ArrayCallbackStage::NextElement;
                } else {
                    return callback_type_error(&state, "not a constructor");
                }
            }
            ArrayCallbackStage::AwaitSpecies => {
                let species = take_completion(&mut completion)?;
                if matches!(species, StoredValue::Undefined | StoredValue::Null) {
                    allocate_callback_destination(runtime, &mut state)?;
                    state.stage = ArrayCallbackStage::NextElement;
                    continue;
                }
                let StoredValue::Function(constructor) = species else {
                    return callback_type_error(&state, "not a constructor");
                };
                if !function_is_constructor(runtime, constructor)? {
                    return callback_type_error(&state, "not a constructor");
                }
                state.stage = ArrayCallbackStage::AwaitSpeciesConstruct;
                let length = if matches!(state.method, ArrayCallback::Map) {
                    state.length
                } else {
                    0
                };
                let argument = StoredValue::Number(JsNumber::from_f64(index_as_f64(length)));
                return suspend_construct_callback(state, constructor, argument, return_to);
            }
            ArrayCallbackStage::AwaitSpeciesConstruct => {
                let destination = take_completion(&mut completion)?;
                if destination.heap_reference().is_none() {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "ArraySpeciesCreate constructor returned a primitive",
                    }
                    .into());
                }
                state.destination = Some(destination);
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
                    if let Some(array) = array_callback_own_presence_array(runtime, &mut state)? {
                        if runtime.array_own_property(array, &key)?.is_none() {
                            // No user code ran while proving the absence, so the
                            // same prototype-chain proof remains valid for the
                            // following hole.
                            if matches!(state.method, ArrayCallback::Map) {
                                state.written = state.written.saturating_add(1);
                            }
                            state.stage = ArrayCallbackStage::NextElement;
                            continue;
                        }
                        // The subsequent Get can enter an accessor, and its
                        // callback can mutate a prototype, so revalidate before
                        // using the shortcut again.
                        state.own_presence_only = false;
                        await_get!(begin_array_callback_element_get(
                            runtime,
                            state,
                            return_to,
                            execution_budget,
                        ));
                    }
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
                let StoredValue::Function(callback) = state.callback else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "Array callback resumed without a callable callback",
                    }
                    .into());
                };
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
                        append_element(runtime, &mut state, answer, execution_budget)?;
                    }
                    ArrayCallback::Filter => {
                        if truthy {
                            let element = element.ok_or(EngineFault::RuntimeInvariant {
                                message: "array filter lost the element it was testing",
                            })?;
                            append_element(runtime, &mut state, element, execution_budget)?;
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
                    Some(destination) => destination,
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

/// Creates one result property with `CreateDataPropertyOrThrow`.
fn append_element(
    runtime: &mut Runtime,
    state: &mut ArrayCallbackContinuation,
    value: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    let Some(destination) = state.destination.as_ref() else {
        return Err(EngineFault::RuntimeInvariant {
            message: "an array callback appended an element with no destination",
        }
        .into());
    };
    let key = element_key(state.written)?;
    match define_static_property(runtime, destination, key, value, execution_budget)? {
        PropertyWriteOutcome::Complete => {
            state.written = state.written.saturating_add(1);
            Ok(())
        }
        PropertyWriteOutcome::Failed(failure) => Err(callback_property_failure(state, failure)),
        PropertyWriteOutcome::Setter { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Array callback CreateDataPropertyOrThrow attempted to call a setter",
        }
        .into()),
    }
}

/// Allocates the default `ArraySpeciesCreate` result when the source has no
/// usable custom species.
fn allocate_callback_destination(
    runtime: &mut Runtime,
    state: &mut ArrayCallbackContinuation,
) -> Result<(), NativeFailure> {
    let destination = runtime.allocate_array(state.realm, Vec::new())?;
    if matches!(state.method, ArrayCallback::Map) {
        let length = u32::try_from(state.length)
            .map_err(|_| callback_range_error(state, "invalid array length"))?;
        match runtime.set_array_length(destination, length)? {
            ArrayLengthWriteOutcome::Complete
            | ArrayLengthWriteOutcome::BlockedByNonConfigurable { .. } => {}
            ArrayLengthWriteOutcome::ReadOnly => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "a fresh ArraySpeciesCreate result had a read-only length",
                }
                .into());
            }
        }
    }
    state.destination = Some(StoredValue::Object(destination));
    Ok(())
}

fn callback_property_failure(
    state: &ArrayCallbackContinuation,
    failure: PropertyFailure,
) -> NativeFailure {
    match property_exception_at(state.realm, state.origin.clone(), None, failure) {
        Ok(exception) => NativeFailure::Abrupt(exception),
        Err(error) => error.into(),
    }
}

fn callback_type_error(
    state: &ArrayCallbackContinuation,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    let message = JsString::from_utf8(message)?;
    Err(NativeFailure::Abrupt(PendingException {
        realm: state.realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message,
        },
        origin: state.origin.clone(),
    }))
}

fn callback_range_error(state: &ArrayCallbackContinuation, message: &str) -> NativeFailure {
    match JsString::from_utf8(message) {
        Ok(message) => NativeFailure::Abrupt(PendingException {
            realm: state.realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message,
            },
            origin: state.origin.clone(),
        }),
        Err(error) => error.into(),
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

/// Suspends into a species constructor that resumes this callback method.
fn suspend_construct_callback(
    state: ArrayCallbackContinuation,
    constructor: FunctionId,
    argument: StoredValue,
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
        function: constructor,
        receiver: StoredValue::Undefined,
        arguments: CallArguments::from_values(vec![argument]),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: Some(constructor),
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

/// Returns the direct Array whose next `HasProperty` can be answered from its
/// own indexed storage alone.
///
/// The proof is deliberately short-lived: after an element `Get` or a callback
/// invokes user code, the next loop iteration traverses the prototype chain
/// again. That keeps inherited indexed accessors and callback-installed
/// prototype properties observable while avoiding repeated scans of ordinary
/// `Array.prototype` and `Object.prototype` for consecutive holes.
fn array_callback_own_presence_array(
    runtime: &Runtime,
    state: &mut ArrayCallbackContinuation,
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
    /// The callback value, validated after the `LengthOfArrayLike` step.
    callback: StoredValue,
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
    /// The receiver, callback, and accumulator.
    pub(crate) const fn retained_values() -> u64 {
        3
    }

    /// Reports the traced roots this continuation retains.
    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        trace_stored_value_root(&self.callback, mark);
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
    // `ToObject(this)` precedes `IsCallable(callback)`, and reductions retain
    // the wrapper as the source of their subsequent indexed operations.
    let receiver = match to_object_value(runtime, realm, receiver, origin.clone())? {
        Ok(receiver) => receiver,
        Err(exception) => return Err(NativeFailure::Abrupt(exception)),
    };
    let callback = arguments.take_first_or_undefined();
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
        callback: StoredValue::Function(callback),
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
                if needs_conversion(&value) {
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::Number,
                        OperatorPrimitiveTarget::ArrayReductionLength(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                let number = operator_to_number(value, state.realm, &state.origin)?;
                state.length = number_to_length(number);
                let StoredValue::Function(_) = state.callback else {
                    return Err(NativeFailure::Abrupt(PendingException {
                        realm: state.realm,
                        payload: PendingExceptionPayload::EngineError {
                            kind: ExceptionKind::TypeError,
                            message: JsString::from_utf8("not a function")?,
                        },
                        origin: state.origin.clone(),
                    }));
                };
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
                let StoredValue::Function(callback) = state.callback else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "Array reduction resumed without a callable callback",
                    }
                    .into());
                };
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

/// Returns whether `ToNumber` must first perform a resumable `ToPrimitive`.
const fn needs_conversion(value: &StoredValue) -> bool {
    matches!(value, StoredValue::Function(_) | StoredValue::Object(_))
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
