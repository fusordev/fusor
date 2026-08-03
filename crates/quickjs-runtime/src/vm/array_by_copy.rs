/*
 * JavaScript Array.prototype change-by-copy semantics derived from QuickJS.
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

//! `Array.prototype.with`, `toReversed`, and `toSpliced`.
//!
//! These are the change-by-copy methods that answer a fresh dense Array. They
//! share one resumable snapshot read because they share one shape: read
//! `length` once with `ToLength`, convert the declared arguments, then read
//! each source index with `JS_TryGetPropertyInt64`, which reports an absent
//! index as `undefined` (`quickjs.c:9115-9142`). That is what makes the result
//! dense: `[1,,3].toReversed()` is `[3, undefined, 1]` with every index
//! present, not a reversed sparse array. Every read can enter a getter, so
//! each is a suspension point.
//!
//! The pinned oracle fixes the observable details:
//!
//! - `with` converts its index with `JS_ToInt64Sat` and reports a rejected one
//!   as `RangeError: invalid array index: <idx>` after the negative adjustment
//!   (`quickjs.c:41859-41868`), and it never reads the replaced index itself
//!   (`quickjs.c:41878-41892`).
//! - `toReversed` reads the source in *descending* order; the comment in the
//!   pinned source notes the order is observable (`quickjs.c:42775`).
//! - `toSpliced` resolves its window the way `splice` does, but reports an
//!   over-long result as `TypeError: invalid array length`
//!   (`quickjs.c:42932-42936`) rather than `splice`'s misspelled message.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// The largest result length these methods admit.
const MAX_ARRAY_LENGTH: u64 = (1_u64 << 53) - 1;

/// Which stage of the builder a continuation resumes into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayByCopyStage {
    /// Awaiting the `length` property read.
    AwaitLength,
    /// Awaiting `ToLength` of the length value.
    AwaitLengthConversion,
    /// Awaiting `ToNumber` of `with`'s index argument.
    AwaitIndex,
    /// Awaiting `ToIntegerOrInfinity` of `toSpliced`'s start argument.
    AwaitStart,
    /// Awaiting `ToIntegerOrInfinity` of `toSpliced`'s delete-count argument.
    AwaitDeleteCount,
    /// Ready to read the next source element.
    NextRead,
    /// Awaiting an element read that may have entered a getter.
    AwaitRead,
    /// Finished; the result array is allocated from the snapshot.
    Done,
}

/// One in-progress change-by-copy method.
pub(crate) struct ArrayByCopyContinuation {
    method: ArrayByCopy,
    /// The coerced receiver being read.
    target: StoredValue,
    /// The unconverted arguments plus `toSpliced`'s insertions.
    arguments: Vec<StoredValue>,
    /// The element count from the single `ToLength` length read.
    length: u64,
    /// `with`'s resolved replacement index.
    replace_index: u64,
    /// `toSpliced`'s resolved window start and removal count.
    start: u64,
    removed: u64,
    /// The output snapshot collected so far, in result order.
    values: Vec<StoredValue>,
    /// The next source index to read; `toReversed` walks it down.
    read_next: u64,
    /// The exclusive end of the current read segment.
    read_end: u64,
    /// Whether the current segment descends, which only `toReversed` uses.
    descending: bool,
    /// Whether `toSpliced` still owes its insertions and tail segment.
    insertions_pending: bool,
    realm: RealmId,
    stage: ArrayByCopyStage,
    origin: JsStackFrame,
}

impl ArrayByCopyContinuation {
    /// The receiver, the snapshot, and each argument.
    pub(crate) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(usize_to_u64(self.arguments.len()))
            .saturating_add(usize_to_u64(self.values.len()))
    }

    /// Reports the traced roots this continuation retains.
    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        for argument in &self.arguments {
            trace_stored_value_root(argument, mark);
        }
        for value in &self.values {
            trace_stored_value_root(value, mark);
        }
    }
}

/// Starts one change-by-copy method.
#[expect(
    clippy::too_many_arguments,
    reason = "one shared entry point carries the method identity alongside the same receiver, arguments, and resumption context every native dispatch takes"
)]
pub(super) fn begin_array_by_copy(
    runtime: &mut Runtime,
    method: ArrayByCopy,
    realm: RealmId,
    receiver: StoredValue,
    arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    // Every method begins with `ToObject(this)`, so a nullish receiver throws
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
    let state = ArrayByCopyContinuation {
        method,
        target: receiver,
        arguments: collected,
        length: 0,
        replace_index: 0,
        start: 0,
        removed: 0,
        values: Vec::new(),
        read_next: 0,
        read_end: 0,
        descending: false,
        insertions_pending: false,
        realm,
        stage: ArrayByCopyStage::AwaitLength,
        origin,
    };
    advance_array_by_copy(runtime, state, None, return_to, execution_budget)
}

/// Resumes a change-by-copy method after an awaited read or conversion.
#[allow(
    clippy::too_many_lines,
    reason = "the length, argument, and snapshot-read stages form one traced continuation shared by with, toReversed, and toSpliced"
)]
pub(super) fn advance_array_by_copy(
    runtime: &mut Runtime,
    mut state: ArrayByCopyContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            ArrayByCopyStage::AwaitLength => {
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                charge_by_copy_lookup(runtime, &state.target, execution_budget)?;
                match read_static_property(runtime, state.realm, &state.target, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        completion = Some(value);
                        state.stage = ArrayByCopyStage::AwaitLengthConversion;
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.stage = ArrayByCopyStage::AwaitLengthConversion;
                        return suspend(state, function, receiver, return_to);
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(by_copy_failure(&state, failure));
                    }
                }
            }
            ArrayByCopyStage::AwaitLengthConversion => {
                let value = take_completion(&mut completion)?;
                let number = operator_to_number(value, state.realm, &state.origin)?;
                state.length = number_to_length(number);
                match state.method {
                    ArrayByCopy::With => state.stage = ArrayByCopyStage::AwaitIndex,
                    ArrayByCopy::ToReversed => {
                        // The source is read in descending order; the pinned
                        // source marks the order observable (`quickjs.c:42775`).
                        state.read_next = state.length;
                        state.descending = true;
                        state.stage = ArrayByCopyStage::NextRead;
                    }
                    ArrayByCopy::ToSpliced => state.stage = ArrayByCopyStage::AwaitStart,
                }
            }
            ArrayByCopyStage::AwaitIndex => {
                if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    // `with` converts with `JS_ToInt64Sat`, which truncates and
                    // saturates rather than clamping (`quickjs.c:41859`); the
                    // rejected index is reported after the negative adjustment.
                    let mut index = number_to_int64_sat(number.as_f64());
                    if index < 0 {
                        index = index.saturating_add(length_as_i64(state.length));
                    }
                    if index < 0 || index >= length_as_i64(state.length) {
                        return Err(invalid_array_index(&state, index)?);
                    }
                    #[expect(
                        clippy::cast_sign_loss,
                        reason = "the bounds above prove the index is non-negative"
                    )]
                    {
                        state.replace_index = index as u64;
                    }
                    state.read_next = 0;
                    state.read_end = state.length;
                    state.stage = ArrayByCopyStage::NextRead;
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
                            OperatorPrimitiveTarget::ArrayByCopyArgument(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    // An absent index converts as `undefined`, which is `0`.
                    Some(value) => completion = Some(value.duplicate()),
                    None => completion = Some(StoredValue::Undefined),
                }
            }
            ArrayByCopyStage::AwaitStart => {
                if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    state.start =
                        relative_bound(number_to_integer_or_infinity(number), state.length);
                    if state.arguments.len() >= 2 {
                        state.stage = ArrayByCopyStage::AwaitDeleteCount;
                        continue;
                    }
                    // With only a start, everything from it is removed, which is
                    // why `[1,2,3].toSpliced(1)` is `[1]`.
                    state.removed = state.length.saturating_sub(state.start);
                    plan_to_spliced(&mut state)?;
                    state.stage = ArrayByCopyStage::NextRead;
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
                            OperatorPrimitiveTarget::ArrayByCopyArgument(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    Some(value) => completion = Some(value.duplicate()),
                    None => {
                        // `toSpliced()` with no arguments copies the receiver.
                        state.start = 0;
                        state.removed = 0;
                        plan_to_spliced(&mut state)?;
                        state.stage = ArrayByCopyStage::NextRead;
                    }
                }
            }
            ArrayByCopyStage::AwaitDeleteCount => {
                if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    let available = state.length.saturating_sub(state.start);
                    state.removed = clamp_count(number_to_integer_or_infinity(number), available);
                    plan_to_spliced(&mut state)?;
                    state.stage = ArrayByCopyStage::NextRead;
                    continue;
                }
                match state.arguments.get(1) {
                    Some(value) if needs_conversion(value) => {
                        let value = value.duplicate();
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::Number,
                            OperatorPrimitiveTarget::ArrayByCopyArgument(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    Some(value) => completion = Some(value.duplicate()),
                    None => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "a toSpliced delete-count stage ran with no argument",
                        }
                        .into());
                    }
                }
            }
            ArrayByCopyStage::NextRead => {
                let reads_remaining = if state.descending {
                    state.read_next > 0
                } else {
                    state.read_next < state.read_end
                };
                if !reads_remaining {
                    if state.insertions_pending {
                        // The insertions land between the head and tail reads.
                        state.insertions_pending = false;
                        let insertions = state.arguments.len().saturating_sub(2);
                        for offset in 0..insertions {
                            let value = state
                                .arguments
                                .get(offset.saturating_add(2))
                                .map_or(StoredValue::Undefined, StoredValue::duplicate);
                            push_snapshot_value(&mut state, value)?;
                        }
                        state.read_next = state.start.saturating_add(state.removed);
                        state.read_end = state.length;
                        continue;
                    }
                    state.stage = ArrayByCopyStage::Done;
                    continue;
                }
                execution_budget.charge_instructions(1)?;
                let index = if state.descending {
                    state.read_next = state.read_next.saturating_sub(1);
                    state.read_next
                } else {
                    let index = state.read_next;
                    state.read_next = state.read_next.saturating_add(1);
                    index
                };
                // `with` never reads the replaced index: the replacement value
                // lands directly, so a getter there is not called
                // (`quickjs.c:41878-41892`).
                if matches!(state.method, ArrayByCopy::With) && index == state.replace_index {
                    let value = state
                        .arguments
                        .get(1)
                        .map_or(StoredValue::Undefined, StoredValue::duplicate);
                    push_snapshot_value(&mut state, value)?;
                    continue;
                }
                let key = element_key(index)?;
                charge_by_copy_lookup(runtime, &state.target, execution_budget)?;
                // An absent index contributes `undefined`, which is what makes
                // the result dense rather than sparse.
                if !has_property(runtime, state.realm, &state.target, &key)? {
                    push_snapshot_value(&mut state, StoredValue::Undefined)?;
                    continue;
                }
                match read_static_property(runtime, state.realm, &state.target, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        completion = Some(value);
                        state.stage = ArrayByCopyStage::AwaitRead;
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.stage = ArrayByCopyStage::AwaitRead;
                        return suspend(state, function, receiver, return_to);
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(by_copy_failure(&state, failure));
                    }
                }
            }
            ArrayByCopyStage::AwaitRead => {
                let value = take_completion(&mut completion)?;
                push_snapshot_value(&mut state, value)?;
                state.stage = ArrayByCopyStage::NextRead;
            }
            ArrayByCopyStage::Done => {
                let values = std::mem::take(&mut state.values);
                let result = runtime.allocate_array(state.realm, values)?;
                return Ok(NativeDispatch::Immediate(StoredValue::Object(result)));
            }
        }
    }
}

/// Plans `toSpliced`'s two read segments and validates the result length.
fn plan_to_spliced(state: &mut ArrayByCopyContinuation) -> Result<(), NativeFailure> {
    let insertions = usize_to_u64(state.arguments.len().saturating_sub(2));
    let new_length = state
        .length
        .saturating_add(insertions)
        .saturating_sub(state.removed);
    if new_length > MAX_ARRAY_LENGTH {
        return Err(NativeFailure::Abrupt(PendingException {
            realm: state.realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("invalid array length")?,
            },
            origin: state.origin.clone(),
        }));
    }
    state.read_next = 0;
    state.read_end = state.start;
    state.insertions_pending = true;
    Ok(())
}

/// Appends one value to the output snapshot.
fn push_snapshot_value(
    state: &mut ArrayByCopyContinuation,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    state
        .values
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    state.values.push(value);
    Ok(())
}

/// Suspends into a getter call that resumes this continuation.
fn suspend(
    state: ArrayByCopyContinuation,
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
    continuations.push(NativeContinuation::ArrayByCopy(Box::new(state)));
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

/// Reports `RangeError: invalid array index: <index>`.
///
/// The index is rendered after the negative adjustment, matching the pinned
/// `PRId64` format (`quickjs.c:41866`).
fn invalid_array_index(
    state: &ArrayByCopyContinuation,
    index: i64,
) -> Result<NativeFailure, NativeFailure> {
    let mut message = JsString::from_utf8("invalid array index: ")?;
    let mut digits = [0_u8; 20];
    let length = render_i64(index, &mut digits);
    let rendered =
        std::str::from_utf8(&digits[..length]).map_err(|_| EngineFault::RuntimeInvariant {
            message: "an i64 rendering was not valid UTF-8",
        })?;
    message = message.concat(&JsString::from_utf8(rendered)?)?;
    Ok(NativeFailure::Abrupt(PendingException {
        realm: state.realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::RangeError,
            message,
        },
        origin: state.origin.clone(),
    }))
}

/// Renders an `i64` as decimal into `buffer`, returning the written length.
fn render_i64(value: i64, buffer: &mut [u8; 20]) -> usize {
    let mut magnitude = value.unsigned_abs();
    let mut reversed = [0_u8; 20];
    let mut digits = 0;
    loop {
        // A value taken modulo 10 is always below 10, so the conversion cannot
        // truncate, which Clippy proves as well.
        let digit = (magnitude % 10) as u8;
        reversed[digits] = b'0' + digit;
        magnitude /= 10;
        digits = digits.saturating_add(1);
        if magnitude == 0 {
            break;
        }
    }
    let mut written = 0;
    if value < 0 {
        buffer[0] = b'-';
        written = 1;
    }
    for index in (0..digits).rev() {
        buffer[written] = reversed[index];
        written = written.saturating_add(1);
    }
    written
}

/// Builds the exception a failed property operation reports.
fn by_copy_failure(state: &ArrayByCopyContinuation, failure: PropertyFailure) -> NativeFailure {
    match property_exception_at(state.realm, state.origin.clone(), None, failure) {
        Ok(exception) => NativeFailure::Abrupt(exception),
        Err(error) => error.into(),
    }
}

/// Charges one property lookup, tolerating a primitive receiver.
fn charge_by_copy_lookup(
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

/// Converts a Number to a saturating, truncating `i64`.
///
/// This is `JS_ToInt64SatFree` (`quickjs.c:13191-13226`): `NaN` becomes `0`,
/// the value truncates toward zero, and an out-of-range magnitude saturates.
/// Rust's float-to-int `as` conversion is defined with exactly those
/// semantics.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the truncation and saturation are the pinned JS_ToInt64Sat semantics"
)]
fn number_to_int64_sat(value: f64) -> i64 {
    value as i64
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

/// Clamps a delete count into `0..=available`.
fn clamp_count(requested: f64, available: u64) -> u64 {
    if requested.is_nan() || requested <= 0.0 {
        return 0;
    }
    let available_as_f64 = length_as_f64(available);
    if requested >= available_as_f64 {
        return available;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the preceding bounds keep the count inside the u64 domain exactly"
    )]
    let count = requested as u64;
    count
}

/// Converts a length to binary64.
#[expect(
    clippy::cast_precision_loss,
    reason = "ToLength bounds every length by 2^53 - 1, which binary64 represents exactly"
)]
fn length_as_f64(length: u64) -> f64 {
    length as f64
}

/// Converts a length to `i64`.
///
/// `ToLength` bounds every value by `2^53 - 1`, so the conversion is exact.
#[expect(
    clippy::cast_possible_wrap,
    reason = "ToLength bounds every length by 2^53 - 1, which i64 represents exactly"
)]
fn length_as_i64(length: u64) -> i64 {
    length as i64
}

/// Returns the property key for one element index.
fn element_key(index: u64) -> Result<PropertyKey, NativeFailure> {
    let index = u32::try_from(index).map_err(|_| EngineFault::RuntimeInvariant {
        message: "array by-copy index exceeded the array-index domain",
    })?;
    let index = ArrayIndex::new(index).ok_or(EngineFault::RuntimeInvariant {
        message: "array by-copy index reached the non-index sentinel",
    })?;
    Ok(PropertyKey::from_index(index))
}

/// Extracts the awaited completion value.
fn take_completion(completion: &mut Option<StoredValue>) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        NativeFailure::Execution(
            EngineFault::RuntimeInvariant {
                message: "an array by-copy method resumed without its awaited completion",
            }
            .into(),
        )
    })
}
