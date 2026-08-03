/*
 * JavaScript Array.prototype copying semantics derived from QuickJS.
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

//! `Array.prototype.slice`, `concat`, `at`, `toReversed`, and `with`.
//!
//! These read without mutating. All except `at` build a fresh Array, but every
//! method shares the same resumable element read because each read can enter a
//! getter. The two change-by-copy methods deliberately read through holes and
//! create an own `undefined` property, unlike `slice` and `concat`, which
//! preserve holes.
//!
//! `concat` spreads only a real Array. A plain array-like becomes a single
//! element, which the pinned oracle confirms: `[1].concat({length:2,0:"a"})` has
//! length `2` and its second element is the object itself. Nesting is not
//! flattened either, so `[1].concat([[2]])` keeps an Array at index `1`.
//!
//! Holes survive into the result. `[1,,3].slice(0)` keeps index `1` absent
//! because an absent source is skipped rather than written as `undefined`, which
//! is the same rule the mutators follow.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// Which stage of the copier a continuation resumes into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayCopierStage {
    /// Awaiting the current source's `length` read.
    AwaitLength,
    /// Awaiting `ToLength` of the length value.
    AwaitLengthConversion,
    /// Awaiting `ToIntegerOrInfinity` of `slice`'s, `at`'s, or `with`'s index.
    AwaitStart,
    /// Awaiting `ToIntegerOrInfinity` of `slice`'s end argument.
    AwaitEnd,
    /// Ready to read the next source element.
    NextElement,
    /// Awaiting an element read that may have entered a getter.
    AwaitElement,
    /// Ready to advance `concat` to its next source.
    NextSource,
    /// Finished.
    Done,
}

/// One in-progress `Array.prototype` copying method.
pub(crate) struct ArrayCopierContinuation {
    copier: ArrayCopier,
    /// The receiver, which is also `concat`'s first source.
    target: StoredValue,
    /// `concat`'s remaining sources, or `slice`/`at`'s arguments.
    arguments: Vec<StoredValue>,
    /// The source currently being read.
    source: StoredValue,
    /// The index of the next `concat` source to take from `arguments`.
    next_source: usize,
    /// Whether the current source is spread element-by-element.
    spreading: bool,
    /// The current source's length.
    length: u64,
    /// The next index to read from the current source.
    next: u64,
    /// The exclusive end of the current source's range.
    end: u64,
    /// The destination array, absent for `at` and until change-by-copy methods
    /// know their validated result length/index.
    destination: Option<ObjectId>,
    /// The next index to write in the destination.
    written: u64,
    /// `at`'s answer.
    result: StoredValue,
    /// The validated replacement index for `with`.
    selected: Option<u64>,
    realm: RealmId,
    stage: ArrayCopierStage,
    origin: JsStackFrame,
}

impl ArrayCopierContinuation {
    /// The receiver, the current source, the result, and each argument.
    pub(crate) fn retained_values(&self) -> u64 {
        3_u64.saturating_add(usize_to_u64(self.arguments.len()))
    }

    /// Reports the traced roots this continuation retains.
    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        trace_stored_value_root(&self.source, mark);
        trace_stored_value_root(&self.result, mark);
        if let Some(destination) = self.destination {
            mark(CollectionRoot::Heap(HeapReference::Object(destination)));
        }
        for argument in &self.arguments {
            trace_stored_value_root(argument, mark);
        }
    }
}

/// Starts one `Array.prototype` copying method.
#[expect(
    clippy::too_many_arguments,
    reason = "one shared entry point carries the method identity alongside the same receiver, arguments, and resumption context every native dispatch takes"
)]
pub(super) fn begin_array_copier(
    runtime: &mut Runtime,
    copier: ArrayCopier,
    realm: RealmId,
    receiver: StoredValue,
    arguments: CallArguments,
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
    let mut collected = Vec::new();
    for value in arguments.into_remaining_iter() {
        collected
            .try_reserve(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::Frames,
                additional: 1,
            })?;
        collected.push(value);
    }
    // `slice` and `concat` allocate their result before this shared driver.
    // `toReversed` must read `length` first, and `with` must additionally
    // convert and validate its index before ArrayCreate, so they allocate in
    // their later specification stages.
    let destination = match copier {
        ArrayCopier::Slice | ArrayCopier::Concat => {
            Some(runtime.allocate_array(realm, Vec::new())?)
        }
        ArrayCopier::At | ArrayCopier::ToReversed | ArrayCopier::With => None,
    };
    // `concat` applies the same spread test to its receiver as to every later
    // source: only a real Array is spread. An array-like receiver therefore
    // becomes a single element, which the oracle confirms with
    // `Array.prototype.concat.call({length:2,0:"a"},9)` reporting length 2.
    // Every other copier always reads its receiver's elements.
    let spreading = match (copier, &receiver) {
        (ArrayCopier::Slice | ArrayCopier::At | ArrayCopier::ToReversed | ArrayCopier::With, _) => {
            true
        }
        (ArrayCopier::Concat, StoredValue::Object(object)) => runtime.is_array_object(*object)?,
        (ArrayCopier::Concat, _) => false,
    };
    let state = ArrayCopierContinuation {
        copier,
        target: receiver.duplicate(),
        arguments: collected,
        source: receiver,
        next_source: 0,
        spreading,
        length: 0,
        next: 0,
        end: 0,
        destination,
        written: 0,
        result: StoredValue::Undefined,
        selected: None,
        realm,
        stage: ArrayCopierStage::AwaitLength,
        origin,
    };
    advance_array_copier(runtime, state, None, return_to, execution_budget)
}

/// Resumes a copying method after an awaited read or conversion.
#[allow(
    clippy::too_many_lines,
    reason = "the length, range, element, and source-advance stages form one traced continuation shared by all five copying methods"
)]
pub(super) fn advance_array_copier(
    runtime: &mut Runtime,
    mut state: ArrayCopierContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            ArrayCopierStage::AwaitLength => {
                // A `concat` source that is not a real Array contributes itself
                // as one element, so it never has its length read.
                if !state.spreading {
                    let element = state.source.duplicate();
                    append_element(runtime, &mut state, element)?;
                    state.stage = ArrayCopierStage::NextSource;
                    continue;
                }
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                charge_copier_lookup(runtime, &state.source, execution_budget)?;
                match read_static_property(runtime, state.realm, &state.source, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        completion = Some(value);
                        state.stage = ArrayCopierStage::AwaitLengthConversion;
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.stage = ArrayCopierStage::AwaitLengthConversion;
                        return suspend(state, function, receiver, return_to);
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(copier_failure(&state, failure));
                    }
                }
            }
            ArrayCopierStage::AwaitLengthConversion => {
                let value = take_completion(&mut completion)?;
                if needs_conversion(&value) {
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::Number,
                        OperatorPrimitiveTarget::ArrayCopierArgument(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                let number = operator_to_number(value, state.realm, &state.origin)?;
                state.length = number_to_length(number);
                match state.copier {
                    // `concat` always takes a whole source.
                    ArrayCopier::Concat => {
                        state.next = 0;
                        state.end = state.length;
                        state.stage = ArrayCopierStage::NextElement;
                    }
                    ArrayCopier::Slice | ArrayCopier::At | ArrayCopier::With => {
                        state.stage = ArrayCopierStage::AwaitStart;
                    }
                    ArrayCopier::ToReversed => {
                        allocate_change_by_copy_destination(runtime, &mut state)?;
                        state.next = 0;
                        state.end = state.length;
                        state.stage = ArrayCopierStage::NextElement;
                    }
                }
            }
            ArrayCopierStage::AwaitStart => {
                if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    let integer = number_to_integer_or_infinity(number);
                    apply_start(runtime, &mut state, integer)?;
                    continue;
                }
                match state.arguments.first() {
                    Some(value) if needs_conversion(value) => {
                        let value = value.duplicate();
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::Number,
                            OperatorPrimitiveTarget::ArrayCopierArgument(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    Some(value) => completion = Some(value.duplicate()),
                    // An absent start begins at zero.
                    None => apply_start(runtime, &mut state, 0.0)?,
                }
            }
            ArrayCopierStage::AwaitEnd => {
                if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    state.end = relative_bound(number_to_integer_or_infinity(number), state.length);
                    state.stage = ArrayCopierStage::NextElement;
                    continue;
                }
                match state.arguments.get(1) {
                    // An explicit `undefined` end is the same as an absent one,
                    // so it runs to the length rather than converting to `0`.
                    Some(StoredValue::Undefined) | None => {
                        state.end = state.length;
                        state.stage = ArrayCopierStage::NextElement;
                    }
                    Some(value) if needs_conversion(value) => {
                        let value = value.duplicate();
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::Number,
                            OperatorPrimitiveTarget::ArrayCopierArgument(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    Some(value) => completion = Some(value.duplicate()),
                }
            }
            ArrayCopierStage::NextElement => {
                if state.next >= state.end {
                    state.stage = ArrayCopierStage::NextSource;
                    continue;
                }
                execution_budget.charge_instructions(1)?;
                let index = state.next;
                state.next = state.next.saturating_add(1);
                if matches!(state.copier, ArrayCopier::With) && state.selected == Some(index) {
                    let replacement = state
                        .arguments
                        .get(1)
                        .map_or(StoredValue::Undefined, StoredValue::duplicate);
                    append_element(runtime, &mut state, replacement)?;
                    continue;
                }
                let source_index = if matches!(state.copier, ArrayCopier::ToReversed) {
                    state.length.saturating_sub(index).saturating_sub(1)
                } else {
                    index
                };
                let key = element_key(source_index)?;
                charge_copier_lookup(runtime, &state.source, execution_budget)?;
                // The older copying methods preserve holes. The change-by-copy
                // methods use Get directly and therefore materialize a missing
                // source index as an own `undefined` property.
                if !matches!(state.copier, ArrayCopier::ToReversed | ArrayCopier::With)
                    && !has_property(runtime, state.realm, &state.source, &key)?
                {
                    if matches!(state.copier, ArrayCopier::At) {
                        state.stage = ArrayCopierStage::Done;
                        continue;
                    }
                    state.written = state.written.saturating_add(1);
                    continue;
                }
                match read_static_property(runtime, state.realm, &state.source, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        completion = Some(value);
                        state.stage = ArrayCopierStage::AwaitElement;
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.stage = ArrayCopierStage::AwaitElement;
                        return suspend(state, function, receiver, return_to);
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(copier_failure(&state, failure));
                    }
                }
            }
            ArrayCopierStage::AwaitElement => {
                let value = take_completion(&mut completion)?;
                if matches!(state.copier, ArrayCopier::At) {
                    state.result = value;
                    state.stage = ArrayCopierStage::Done;
                    continue;
                }
                append_element(runtime, &mut state, value)?;
                state.stage = ArrayCopierStage::NextElement;
            }
            ArrayCopierStage::NextSource => {
                // Only `concat` has more than one source.
                if !matches!(state.copier, ArrayCopier::Concat) {
                    state.stage = ArrayCopierStage::Done;
                    continue;
                }
                let Some(next) = state.arguments.get(state.next_source) else {
                    state.stage = ArrayCopierStage::Done;
                    continue;
                };
                let next = next.duplicate();
                state.next_source = state.next_source.saturating_add(1);
                // Only a real Array spreads; everything else, including an
                // array-like, is appended as a single element.
                state.spreading = match next {
                    StoredValue::Object(object) => runtime.is_array_object(object)?,
                    _ => false,
                };
                state.source = next;
                state.stage = ArrayCopierStage::AwaitLength;
            }
            ArrayCopierStage::Done => {
                return Ok(NativeDispatch::Immediate(match state.destination {
                    Some(destination) => {
                        // The destination's length is set once at the end, so a
                        // trailing hole is still counted.
                        finish_destination(runtime, &state, destination)?;
                        StoredValue::Object(destination)
                    }
                    None => state.result,
                }));
            }
        }
    }
}

/// Applies `slice`'s start, `at`'s index, or `with`'s replacement index.
fn apply_start(
    runtime: &mut Runtime,
    state: &mut ArrayCopierContinuation,
    integer: f64,
) -> Result<(), NativeFailure> {
    match state.copier {
        ArrayCopier::At => {
            // `at` accepts a negative index counting from the end and answers
            // `undefined` outside the range.
            let length_as_f64 = length_as_f64(state.length);
            let resolved = if integer < 0.0 {
                length_as_f64 + integer
            } else {
                integer
            };
            if resolved < 0.0 || resolved >= length_as_f64 {
                state.stage = ArrayCopierStage::Done;
                return Ok(());
            }
            let index = relative_bound(resolved, state.length);
            state.next = index;
            state.end = index.saturating_add(1);
            state.stage = ArrayCopierStage::NextElement;
        }
        ArrayCopier::Slice | ArrayCopier::Concat => {
            state.next = relative_bound(integer, state.length);
            state.stage = ArrayCopierStage::AwaitEnd;
        }
        ArrayCopier::ToReversed => {
            return Err(EngineFault::RuntimeInvariant {
                message: "toReversed entered an argument-conversion stage",
            }
            .into());
        }
        ArrayCopier::With => {
            let length = length_as_f64(state.length);
            let actual = if integer >= 0.0 {
                integer
            } else {
                length + integer
            };
            if actual < 0.0 || actual >= length {
                return Err(NativeFailure::Abrupt(copier_range_error(
                    state,
                    "invalid array index",
                )?));
            }
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the specification range check bounds the integral index by ToLength"
            )]
            let selected = actual as u64;
            state.selected = Some(selected);
            allocate_change_by_copy_destination(runtime, state)?;
            state.next = 0;
            state.end = state.length;
            state.stage = ArrayCopierStage::NextElement;
        }
    }
    Ok(())
}

/// Performs the `ArrayCreate(length)` used by change-by-copy methods.
fn allocate_change_by_copy_destination(
    runtime: &mut Runtime,
    state: &mut ArrayCopierContinuation,
) -> Result<(), NativeFailure> {
    let Ok(length) = u32::try_from(state.length) else {
        return Err(NativeFailure::Abrupt(copier_range_error(
            state,
            "invalid array length",
        )?));
    };
    let prototype = runtime.realm_array_prototype(state.realm)?;
    let destination =
        runtime.allocate_sparse_array_with_prototype(HeapReference::Object(prototype), length)?;
    state.destination = Some(destination);
    Ok(())
}

/// Appends one element to the destination array.
fn append_element(
    runtime: &mut Runtime,
    state: &mut ArrayCopierContinuation,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    let Some(destination) = state.destination else {
        return Err(EngineFault::RuntimeInvariant {
            message: "an array copier appended an element with no destination",
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
/// The length is written once at the end rather than per element, so a trailing
/// hole is still counted: `[1,,].slice(0)` has length `2`.
fn finish_destination(
    runtime: &mut Runtime,
    state: &ArrayCopierContinuation,
    destination: ObjectId,
) -> Result<(), NativeFailure> {
    let length = u32::try_from(state.written).map_err(|_| EngineFault::RuntimeInvariant {
        message: "an array copier produced a length outside the array-index domain",
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

/// Suspends into a getter call that resumes this continuation.
fn suspend(
    state: ArrayCopierContinuation,
    function: FunctionId,
    receiver: StoredValue,
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
    continuations.push(NativeContinuation::ArrayCopier(Box::new(state)));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::empty(),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

/// Builds the exception a failed property read reports.
fn copier_failure(state: &ArrayCopierContinuation, failure: PropertyFailure) -> NativeFailure {
    match property_exception_at(state.realm, state.origin.clone(), None, failure) {
        Ok(exception) => NativeFailure::Abrupt(exception),
        Err(error) => error.into(),
    }
}

/// Builds one realm-owned range exception for a change-by-copy precondition.
fn copier_range_error(
    state: &ArrayCopierContinuation,
    message: &str,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm: state.realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::RangeError,
            message: JsString::from_utf8(message)?,
        },
        origin: state.origin.clone(),
    })
}

/// Charges one property lookup, tolerating a primitive source.
fn charge_copier_lookup(
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

/// Resolves a relative endpoint against a length.
fn relative_bound(value: f64, length: u64) -> u64 {
    let length_as_f64 = length_as_f64(length);
    let resolved = if value < 0.0 {
        length_as_f64 + value
    } else {
        value
    };
    if resolved <= 0.0 {
        return 0;
    }
    if resolved >= length_as_f64 {
        return length;
    }
    // The bounds prove the value is a non-negative integer below `length`.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the preceding bounds keep the value inside the u64 domain exactly"
    )]
    let bound = resolved as u64;
    bound
}

/// Converts a length to binary64.
#[expect(
    clippy::cast_precision_loss,
    reason = "ToLength bounds every length by 2^53 - 1, which binary64 represents exactly"
)]
fn length_as_f64(length: u64) -> f64 {
    length as f64
}

/// Returns the array index for one element position.
fn element_index(index: u64) -> Result<ArrayIndex, NativeFailure> {
    let index = u32::try_from(index).map_err(|_| EngineFault::RuntimeInvariant {
        message: "array copier index exceeded the array-index domain",
    })?;
    ArrayIndex::new(index).ok_or_else(|| {
        EngineFault::RuntimeInvariant {
            message: "array copier index reached the non-index sentinel",
        }
        .into()
    })
}

/// Returns the property key for one element index.
fn element_key(index: u64) -> Result<PropertyKey, NativeFailure> {
    Ok(PropertyKey::from_index(element_index(index)?))
}

/// Extracts the awaited completion value.
fn take_completion(completion: &mut Option<StoredValue>) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        NativeFailure::Execution(
            EngineFault::RuntimeInvariant {
                message: "an array copier resumed without its awaited completion",
            }
            .into(),
        )
    })
}
