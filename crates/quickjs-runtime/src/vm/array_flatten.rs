/*
 * JavaScript Array.prototype flattening semantics derived from QuickJS.
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

//! `Array.prototype.flat` and `flatMap`.
//!
//! Both methods run `JS_FlattenIntoArray` (`quickjs.c:43014-43074`): each
//! present source element is either appended to the fresh destination or, when
//! the remaining depth is positive and the element is a real Array, entered
//! and read element by element. Upstream recurses; this port carries an
//! explicit worklist of source frames instead, so the driver stays iterative
//! while the observable order is identical: sources are read in ascending
//! index order, innermost first, and every read can enter a getter, so each is
//! a suspension point.
//!
//! The pinned oracle fixes the observable details:
//!
//! - Holes are skipped, so `[1,,[3]].flat()` has length `2`.
//! - `flatMap` validates its mapper with `check_function` *after* the length
//!   read (`quickjs.c:43086-43098`), so a throwing `length` getter beats a
//!   `not a function` mapper.
//! - `flat`'s depth converts with `JS_ToInt32Sat`, so `flat(1.9)` flattens one
//!   level while `flat(NaN)` flattens none (`quickjs.c:43100-43103`).
//! - The mapper is called with `(element, index, source)` and the `thisArg`
//!   receiver, and only the outermost source is mapped (`quickjs.c:43035-43042`).
//! - Appending past `2^53 - 1` reports `TypeError: Array too long`
//!   (`quickjs.c:43060-43062`).

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// The largest destination index these methods admit.
const MAX_ARRAY_LENGTH: u64 = (1_u64 << 53) - 1;

/// One in-progress source on the flatten worklist.
struct FlattenFrame {
    /// The source being read.
    source: StoredValue,
    /// The source's length from its single read.
    length: u64,
    /// The next index to read.
    next: u64,
    /// The remaining flatten depth below this frame.
    depth: u32,
    /// Whether this frame's elements pass through the mapper, which only
    /// `flatMap`'s outermost source does.
    mapped: bool,
}

/// Which stage of the driver a continuation resumes into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayFlattenStage {
    /// Awaiting the receiver's `length` property read.
    AwaitLength,
    /// Awaiting `ToLength` of the length value.
    AwaitLengthConversion,
    /// Awaiting `ToNumber` of `flat`'s depth argument.
    AwaitDepth,
    /// Ready to read the next source element.
    NextElement,
    /// Awaiting an element read that may have entered a getter.
    AwaitElement,
    /// Awaiting the mapper's result for the current element.
    AwaitMapped,
    /// Finished.
    Done,
}

/// One in-progress `flat` or `flatMap`.
pub(crate) struct ArrayFlattenContinuation {
    method: ArrayFlatten,
    /// The coerced receiver, which becomes the worklist's root frame.
    receiver: StoredValue,
    /// `flat`'s depth argument, or `flatMap`'s mapper and `thisArg`.
    arguments: Vec<StoredValue>,
    /// The fresh destination being appended to.
    destination: ObjectId,
    /// The source worklist, innermost frame last.
    frames: Vec<FlattenFrame>,
    /// The next destination index.
    written: u64,
    /// The receiver's length, held until the root frame is planned.
    length: u64,
    realm: RealmId,
    stage: ArrayFlattenStage,
    origin: JsStackFrame,
}

impl ArrayFlattenContinuation {
    /// The receiver, the destination, the arguments, and each frame's source.
    pub(crate) fn retained_values(&self) -> u64 {
        3_u64
            .saturating_add(usize_to_u64(self.arguments.len()))
            .saturating_add(usize_to_u64(self.frames.len()))
    }

    /// Reports the traced roots this continuation retains.
    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.receiver, mark);
        mark(CollectionRoot::Heap(HeapReference::Object(self.destination)));
        for argument in &self.arguments {
            trace_stored_value_root(argument, mark);
        }
        for frame in &self.frames {
            trace_stored_value_root(&frame.source, mark);
        }
    }
}

/// Starts one `flat` or `flatMap`.
#[expect(
    clippy::too_many_arguments,
    reason = "one shared entry point carries the method identity alongside the same receiver, arguments, and resumption context every native dispatch takes"
)]
pub(super) fn begin_array_flatten(
    runtime: &mut Runtime,
    method: ArrayFlatten,
    realm: RealmId,
    receiver: StoredValue,
    arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    // Both methods begin with `ToObject(this)`, so a nullish receiver throws
    // before `length` is read.
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
    // The destination is a fresh base Array, the same narrowing `concat` and
    // `splice` already document: upstream's `JS_ArraySpeciesCreate` reads the
    // receiver's `constructor` and `Symbol.species`, which this profile does
    // not yet admit, so it fails closed by ignoring them rather than by
    // answering through the wrong constructor.
    let destination = runtime.allocate_array(realm, Vec::new())?;
    let state = ArrayFlattenContinuation {
        method,
        receiver,
        arguments: collected,
        destination,
        frames: Vec::new(),
        written: 0,
        length: 0,
        realm,
        stage: ArrayFlattenStage::AwaitLength,
        origin,
    };
    advance_array_flatten(runtime, state, None, return_to, execution_budget)
}

/// Resumes a flattening method after an awaited read, conversion, or call.
#[allow(
    clippy::too_many_lines,
    reason = "the length, depth, element, mapping, and nesting stages form one traced continuation shared by flat and flatMap"
)]
pub(super) fn advance_array_flatten(
    runtime: &mut Runtime,
    mut state: ArrayFlattenContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            ArrayFlattenStage::AwaitLength => {
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                charge_flatten_lookup(runtime, &state.receiver, execution_budget)?;
                match read_static_property(runtime, state.realm, &state.receiver, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        completion = Some(value);
                        state.stage = ArrayFlattenStage::AwaitLengthConversion;
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.stage = ArrayFlattenStage::AwaitLengthConversion;
                        return suspend_with_receiver(
                            state,
                            receiver,
                            function,
                            CallArguments::empty(),
                            return_to,
                        );
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(flatten_failure(&state, failure));
                    }
                }
            }
            ArrayFlattenStage::AwaitLengthConversion => {
                let value = take_completion(&mut completion)?;
                let number = operator_to_number(value, state.realm, &state.origin)?;
                state.length = number_to_length(number);
                match state.method {
                    ArrayFlatten::Flat => state.stage = ArrayFlattenStage::AwaitDepth,
                    // `flatMap` validates its mapper after the length read
                    // (`quickjs.c:43086-43098`), so a throwing `length` getter
                    // beats a non-callable mapper.
                    ArrayFlatten::FlatMap => {
                        if !matches!(state.arguments.first(), Some(StoredValue::Function(_))) {
                            return Err(NativeFailure::Abrupt(PendingException {
                                realm: state.realm,
                                payload: PendingExceptionPayload::EngineError {
                                    kind: ExceptionKind::TypeError,
                                    message: JsString::from_utf8("not a function")?,
                                },
                                origin: state.origin.clone(),
                            }));
                        }
                        plan_root_frame(&mut state, 1, true)?;
                        state.stage = ArrayFlattenStage::NextElement;
                    }
                }
            }
            ArrayFlattenStage::AwaitDepth => {
                let depth = if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    number_to_flatten_depth(number.as_f64())
                } else {
                    match state.arguments.first() {
                        // An absent or `undefined` depth flattens one level.
                        None | Some(StoredValue::Undefined) => 1,
                        Some(value) if needs_conversion(value) => {
                            let value = value.duplicate();
                            let realm = state.realm;
                            let origin = state.origin.clone();
                            return begin_operator_primitive_conversion(
                                runtime,
                                value,
                                OperatorPrimitiveHint::Number,
                                OperatorPrimitiveTarget::ArrayFlattenArgument(Box::new(state)),
                                realm,
                                return_to,
                                origin,
                                execution_budget,
                            );
                        }
                        Some(value) => {
                            completion = Some(value.duplicate());
                            continue;
                        }
                    }
                };
                plan_root_frame(&mut state, depth, false)?;
                state.stage = ArrayFlattenStage::NextElement;
            }
            ArrayFlattenStage::NextElement => {
                let Some(frame) = state.frames.last_mut() else {
                    state.stage = ArrayFlattenStage::Done;
                    continue;
                };
                if frame.next >= frame.length {
                    state.frames.pop();
                    continue;
                }
                execution_budget.charge_instructions(1)?;
                let index = frame.next;
                frame.next = frame.next.saturating_add(1);
                let key = element_key(index)?;
                charge_flatten_lookup(runtime, &frame.source, execution_budget)?;
                // A missing index is skipped, so the destination never gains a
                // hole's `undefined`.
                if !has_property(runtime, state.realm, &frame.source, &key)? {
                    continue;
                }
                match read_static_property(runtime, state.realm, &frame.source, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        completion = Some(value);
                        state.stage = ArrayFlattenStage::AwaitElement;
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.stage = ArrayFlattenStage::AwaitElement;
                        return suspend_with_receiver(
                            state,
                            receiver,
                            function,
                            CallArguments::empty(),
                            return_to,
                        );
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(flatten_failure(&state, failure));
                    }
                }
            }
            ArrayFlattenStage::AwaitElement => {
                let element = take_completion(&mut completion)?;
                let mapped = state
                    .frames
                    .last()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "a flatten element arrived with no source frame",
                    })?
                    .mapped;
                if mapped {
                    // Only `flatMap`'s outermost source maps; the call passes
                    // `(element, index, source)` with the `thisArg` receiver
                    // (`quickjs.c:43036-43037`).
                    let (index, source) = {
                        let frame = state.frames.last().ok_or(EngineFault::RuntimeInvariant {
                            message: "a flatten element arrived with no source frame",
                        })?;
                        (frame.next.saturating_sub(1), frame.source.duplicate())
                    };
                    let function = match state.arguments.first() {
                        Some(StoredValue::Function(function)) => *function,
                        _ => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "a flatMap mapper disappeared before its call",
                            }
                            .into());
                        }
                    };
                    let receiver = state
                        .arguments
                        .get(1)
                        .map_or(StoredValue::Undefined, StoredValue::duplicate);
                    let mut values = Vec::new();
                    values
                        .try_reserve_exact(3)
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 3,
                        })?;
                    values.push(element);
                    values.push(StoredValue::Number(JsNumber::from_f64(length_as_f64(
                        index,
                    ))));
                    values.push(source);
                    state.stage = ArrayFlattenStage::AwaitMapped;
                    return suspend_with_receiver(
                        state,
                        receiver,
                        function,
                        CallArguments::from_values(values),
                        return_to,
                    );
                }
                state = append_or_enter(runtime, state, element, execution_budget)?;
                state.stage = ArrayFlattenStage::NextElement;
            }
            ArrayFlattenStage::AwaitMapped => {
                let element = take_completion(&mut completion)?;
                state = append_or_enter(runtime, state, element, execution_budget)?;
                state.stage = ArrayFlattenStage::NextElement;
            }
            ArrayFlattenStage::Done => {
                return Ok(NativeDispatch::Immediate(StoredValue::Object(
                    state.destination,
                )));
            }
        }
    }
}

/// Pushes the receiver as the worklist's root frame.
fn plan_root_frame(
    state: &mut ArrayFlattenContinuation,
    depth: u32,
    mapped: bool,
) -> Result<(), NativeFailure> {
    if !state.frames.is_empty() {
        return Err(EngineFault::RuntimeInvariant {
            message: "a flatten root frame was planned twice",
        }
        .into());
    }
    state
        .frames
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    let receiver = std::mem::replace(&mut state.receiver, StoredValue::Undefined);
    state.frames.push(FlattenFrame {
        source: receiver,
        length: state.length,
        next: 0,
        depth,
        mapped,
    });
    Ok(())
}

/// Appends one element to the destination, or enters it when it is a real
/// Array and the remaining depth is positive.
fn append_or_enter(
    runtime: &mut Runtime,
    mut state: ArrayFlattenContinuation,
    element: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<ArrayFlattenContinuation, NativeFailure> {
    let depth = state
        .frames
        .last()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "a flatten element arrived with no source frame",
        })?
        .depth;
    let nested = match (&element, depth > 0) {
        (StoredValue::Object(object), true) => runtime.is_array_object(*object)?,
        _ => false,
    };
    if nested {
        let StoredValue::Object(object) = &element else {
            return Err(EngineFault::RuntimeInvariant {
                message: "a flatten nested source was not an object",
            }
            .into());
        };
        // A real Array's length is its exotic data property, so reading it
        // cannot enter a getter; `js_get_length64` observes the same value.
        let length = runtime
            .array_length(*object)?
            .ok_or(EngineFault::RuntimeInvariant {
                message: "a flatten nested source lost its array length",
            })?;
        state
            .frames
            .try_reserve(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::Frames,
                additional: 1,
            })?;
        let parent_depth = state
            .frames
            .last()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "a flatten nested source arrived with no parent frame",
            })?
            .depth;
        state.frames.push(FlattenFrame {
            source: element,
            length: u64::from(length),
            next: 0,
            depth: parent_depth.saturating_sub(1),
            // Only the outermost source maps; nested frames never do
            // (`quickjs.c:43050-43052`).
            mapped: false,
        });
        return Ok(state);
    }
    if state.written >= MAX_ARRAY_LENGTH {
        return Err(NativeFailure::Abrupt(PendingException {
            realm: state.realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("Array too long")?,
            },
            origin: state.origin.clone(),
        }));
    }
    execution_budget.charge_instructions(1)?;
    let key = element_key(state.written)?;
    match runtime.define_array_data_property(
        state.destination,
        key,
        PropertyLayout::data(true, true, true),
        element,
    )? {
        ArrayDefineOutcome::Complete => {}
        ArrayDefineOutcome::ReadOnlyLength | ArrayDefineOutcome::NonExtensible => {
            return Err(EngineFault::RuntimeInvariant {
                message: "a freshly allocated flatten destination refused an element",
            }
            .into());
        }
    }
    state.written = state.written.saturating_add(1);
    Ok(state)
}

/// Suspends into a call that resumes this continuation.
fn suspend_with_receiver(
    state: ArrayFlattenContinuation,
    receiver: StoredValue,
    function: FunctionId,
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
    continuations.push(NativeContinuation::ArrayFlatten(Box::new(state)));
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
fn flatten_failure(state: &ArrayFlattenContinuation, failure: PropertyFailure) -> NativeFailure {
    match property_exception_at(state.realm, state.origin.clone(), None, failure) {
        Ok(exception) => NativeFailure::Abrupt(exception),
        Err(error) => error.into(),
    }
}

/// Charges one property lookup, tolerating a primitive source.
fn charge_flatten_lookup(
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

/// Converts a Number to `flat`'s depth.
///
/// This is `JS_ToInt32Sat` (`quickjs.c:13169-13172`): `NaN` becomes `0`, the
/// value truncates toward zero, and an out-of-range magnitude saturates. Only
/// the sign and the magnitude matter here: a non-positive depth flattens
/// nothing, and any depth above the nesting behaves identically, so the
/// result is clamped into `u32`.
fn number_to_flatten_depth(value: f64) -> u32 {
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    if value >= 2_147_483_647.0 {
        return 2_147_483_647;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the preceding bounds keep the depth inside the u32 domain exactly"
    )]
    let depth = value as u32;
    depth
}

/// Converts an index to binary64.
#[expect(
    clippy::cast_precision_loss,
    reason = "every index is bounded by 2^53 - 1, which binary64 represents exactly"
)]
fn length_as_f64(length: u64) -> f64 {
    length as f64
}

/// Returns the property key for one element index.
fn element_key(index: u64) -> Result<PropertyKey, NativeFailure> {
    let index = u32::try_from(index).map_err(|_| EngineFault::RuntimeInvariant {
        message: "array flatten index exceeded the array-index domain",
    })?;
    let index = ArrayIndex::new(index).ok_or(EngineFault::RuntimeInvariant {
        message: "array flatten index reached the non-index sentinel",
    })?;
    Ok(PropertyKey::from_index(index))
}

/// Extracts the awaited completion value.
fn take_completion(completion: &mut Option<StoredValue>) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        NativeFailure::Execution(
            EngineFault::RuntimeInvariant {
                message: "an array flatten method resumed without its awaited completion",
            }
            .into(),
        )
    })
}
