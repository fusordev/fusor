/*
 * JavaScript String.prototype semantics derived from QuickJS.
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

//! The `String.prototype` method surface that needs no `RegExp`.
//!
//! Every one of these methods shares the same shape, which is why they share one
//! resumable state machine rather than repeating it per method:
//!
//! 1. `RequireObjectCoercible(this)` rejects `null` and `undefined`.
//! 2. `ToString(this)` produces the subject and can re-enter the interpreter.
//! 3. Each declared argument is coerced left to right, and each coercion can
//!    re-enter the interpreter.
//! 4. The method computes its result from already-converted values.
//!
//! The pinned oracle fixes that order: for
//! `String.prototype.indexOf.call(recv, arg, pos)` with side-effecting
//! conversions it logs `recv,arg,pos`, so the receiver is converted before any
//! argument and the arguments follow their declaration order.
//!
//! Indices are UTF-16 code-unit indices throughout, so a lone surrogate stays
//! observable and `"a\u{1F600}b".length` is `4`.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

use icu_casemap::CaseMapperBorrowed;
use icu_locale_core::LanguageIdentifier;
use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
use writeable::Writeable as _;

/// One already-coerced argument.
#[derive(Clone, Debug)]
enum ConvertedArgument {
    Text(JsString),
    /// An absent or `undefined` optional argument.
    Absent,
    Integer(f64),
    Number(JsNumber),
}

impl ConvertedArgument {
    /// Returns the text of a `String`-shaped argument.
    fn text(&self) -> Result<&JsString, NativeFailure> {
        match self {
            Self::Text(value) => Ok(value),
            Self::Absent | Self::Integer(_) | Self::Number(_) => {
                Err(EngineFault::RuntimeInvariant {
                    message: "a String method read a non-string argument as text",
                }
                .into())
            }
        }
    }

    /// Returns the integer of an `Integer`-shaped argument.
    fn integer(&self) -> Result<f64, NativeFailure> {
        match self {
            Self::Integer(value) => Ok(*value),
            Self::Text(_) | Self::Absent | Self::Number(_) => Err(EngineFault::RuntimeInvariant {
                message: "a String method read a non-integer argument as an integer",
            }
            .into()),
        }
    }
}

/// Which stage of a String method a continuation resumes into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StringMethodStage {
    /// Awaiting `ToString` of the receiver.
    AwaitSubject,
    /// Awaiting the conversion of the argument at `next_argument`.
    AwaitArgument,
}

/// One in-progress `String.prototype` method call.
pub(super) struct StringMethodContinuation {
    method: StringMethod,
    /// The receiver, until `ToString` replaces it with `subject`.
    receiver: StoredValue,
    /// The converted subject. `None` until stage `AwaitSubject` completes.
    subject: Option<JsString>,
    /// The arguments still awaiting conversion, in declaration order.
    pending: Vec<StoredValue>,
    /// The arguments converted so far.
    converted: Vec<ConvertedArgument>,
    /// The index of the argument being converted.
    next_argument: usize,
    realm: RealmId,
    stage: StringMethodStage,
    origin: JsStackFrame,
}

impl StringMethodContinuation {
    /// The receiver plus the subject and each retained argument.
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(usize_to_u64(self.pending.len()))
            .saturating_add(usize_to_u64(self.converted.len()))
    }

    /// Reports the receiver and any unconverted argument so cycle collection can
    /// trace them.
    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.receiver, mark);
        for value in &self.pending {
            trace_stored_value_root(value, mark);
        }
    }
}

/// Starts one `String.prototype` method.
#[expect(
    clippy::too_many_arguments,
    reason = "one shared entry point carries the method identity alongside the same receiver, arguments, and resumption context every native dispatch takes"
)]
pub(super) fn begin_string_method(
    runtime: &mut Runtime,
    method: StringMethod,
    realm: RealmId,
    receiver: StoredValue,
    arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    // `RequireObjectCoercible` runs before `ToString`, so a nullish receiver
    // throws before any argument is touched. The two `String` statics skip this
    // entirely because they ignore their receiver.
    if method.converts_receiver() && matches!(receiver, StoredValue::Undefined | StoredValue::Null)
    {
        return Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("null or undefined are forbidden")?,
            },
            origin,
        }));
    }

    let mut pending = Vec::new();
    let declared = method.argument_shape().len();
    let mut values = arguments.into_remaining_iter();
    if method.is_variadic() {
        for value in values {
            pending
                .try_reserve(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 1,
                })?;
            pending.push(value);
        }
    } else {
        pending
            .try_reserve_exact(declared)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::Frames,
                additional: declared,
            })?;
        for _ in 0..declared {
            pending.push(values.next().unwrap_or(StoredValue::Undefined));
        }
    }

    let state = StringMethodContinuation {
        method,
        receiver: receiver.duplicate(),
        subject: None,
        pending,
        converted: Vec::new(),
        next_argument: 0,
        realm,
        stage: StringMethodStage::AwaitSubject,
        origin: origin.clone(),
    };

    // A primitive String receiver needs no conversion, which keeps the common
    // case free of a continuation; a static has no subject at all.
    if !method.converts_receiver() {
        let mut state = state;
        state.subject = Some(JsString::empty());
        return advance_string_method(runtime, state, None, return_to, execution_budget);
    }
    if let StoredValue::String(subject) = receiver {
        let mut state = state;
        state.subject = Some(subject);
        return advance_string_method(runtime, state, None, return_to, execution_budget);
    }
    begin_operator_primitive_conversion(
        runtime,
        receiver,
        OperatorPrimitiveHint::String,
        OperatorPrimitiveTarget::StringMethodSubject(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

/// Resumes a String method after an awaited conversion.
pub(super) fn advance_string_method(
    runtime: &mut Runtime,
    mut state: StringMethodContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            StringMethodStage::AwaitSubject => {
                if state.subject.is_none() {
                    let value = take_completion(&mut completion)?;
                    state.subject = Some(operator_primitive_to_string(
                        value,
                        state.realm,
                        &state.origin,
                    )?);
                }
                state.stage = StringMethodStage::AwaitArgument;
            }
            StringMethodStage::AwaitArgument => {
                // Store the argument the previous iteration awaited.
                if let Some(value) = completion.take() {
                    let shape = argument_shape_at(state.method, state.next_argument);
                    let converted = convert_argument(shape, value, state.realm, &state.origin)?;
                    state.converted.try_reserve(1).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 1,
                        }
                    })?;
                    state.converted.push(converted);
                    state.next_argument = state.next_argument.saturating_add(1);
                }

                let Some(value) = state.pending.get(state.next_argument) else {
                    return finish_string_method(&state, execution_budget);
                };
                let shape = argument_shape_at(state.method, state.next_argument);
                let value = value.duplicate();

                // An absent or `undefined` optional argument never runs a
                // conversion, which is what makes `"hello".slice(1, undefined)`
                // the same as `"hello".slice(1)`.
                if matches!(
                    shape,
                    StringArgument::OptionalInteger | StringArgument::OptionalString
                ) && matches!(value, StoredValue::Undefined)
                {
                    state.converted.try_reserve(1).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 1,
                        }
                    })?;
                    state.converted.push(ConvertedArgument::Absent);
                    state.next_argument = state.next_argument.saturating_add(1);
                    continue;
                }

                let hint = match shape {
                    StringArgument::String | StringArgument::OptionalString => {
                        OperatorPrimitiveHint::String
                    }
                    StringArgument::Integer
                    | StringArgument::OptionalInteger
                    | StringArgument::Number => OperatorPrimitiveHint::Number,
                };
                // An already-primitive argument converts without suspending.
                if !matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    completion = Some(value);
                    continue;
                }
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    hint,
                    OperatorPrimitiveTarget::StringMethodArgument(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
        }
    }
}

/// Returns the coercion shape of one argument position.
fn argument_shape_at(method: StringMethod, index: usize) -> StringArgument {
    method
        .argument_shape()
        .get(index)
        .copied()
        .unwrap_or_else(|| method.variadic_argument())
}

/// Applies one argument's coercion to an already-primitive value.
fn convert_argument(
    shape: StringArgument,
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<ConvertedArgument, NativeFailure> {
    match shape {
        StringArgument::String | StringArgument::OptionalString => Ok(ConvertedArgument::Text(
            operator_primitive_to_string(value, realm, origin)?,
        )),
        StringArgument::Integer | StringArgument::OptionalInteger => {
            let number = operator_to_number(value, realm, origin)?;
            Ok(ConvertedArgument::Integer(number_to_integer_or_infinity(
                number,
            )))
        }
        StringArgument::Number => Ok(ConvertedArgument::Number(operator_to_number(
            value, realm, origin,
        )?)),
    }
}

/// Extracts the awaited completion value.
fn take_completion(completion: &mut Option<StoredValue>) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        NativeFailure::Execution(
            EngineFault::RuntimeInvariant {
                message: "a String method resumed without its awaited completion",
            }
            .into(),
        )
    })
}

/// Computes the method's result from the converted subject and arguments.
#[allow(
    clippy::too_many_lines,
    reason = "the method bodies are one flat dispatch over an already-converted argument list, which keeps every String.prototype result at a single audited site"
)]
fn finish_string_method(
    state: &StringMethodContinuation,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let subject = state
        .subject
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "a String method reached its body without a converted subject",
        })?;
    let length = subject.len();
    let arguments = state.converted.as_slice();
    let argument = |index: usize| -> Result<&ConvertedArgument, NativeFailure> {
        arguments.get(index).ok_or_else(|| {
            EngineFault::RuntimeInvariant {
                message: "a String method reached its body without every argument converted",
            }
            .into()
        })
    };

    let value = match state.method {
        StringMethod::CharAt => {
            let index = argument(0)?.integer()?;
            // An out-of-range index yields the empty string rather than
            // `undefined`, which is what separates `charAt` from `at`.
            match clamp_index(index, length) {
                Some(index) => StoredValue::String(subject.slice(index..index + 1)?),
                None => StoredValue::String(JsString::empty()),
            }
        }
        StringMethod::CharCodeAt => {
            let index = argument(0)?.integer()?;
            match clamp_index(index, length).and_then(|index| subject.code_unit_at(index)) {
                Some(unit) => StoredValue::Number(JsNumber::from_u32(u32::from(unit))),
                // An out-of-range index is `NaN`, not `undefined`.
                None => StoredValue::Number(JsNumber::from_f64(f64::NAN)),
            }
        }
        StringMethod::CodePointAt => {
            let index = argument(0)?.integer()?;
            match clamp_index(index, length) {
                Some(index) => {
                    StoredValue::Number(JsNumber::from_u32(code_point_at(subject, index)?))
                }
                None => StoredValue::Undefined,
            }
        }
        StringMethod::At => {
            // `at` accepts a negative index counting from the end and answers
            // `undefined` outside the range.
            let index = argument(0)?.integer()?;
            match relative_index(index, length) {
                Some(index) => StoredValue::String(subject.slice(index..index + 1)?),
                None => StoredValue::Undefined,
            }
        }
        StringMethod::Concat => {
            let mut result = subject.clone();
            for argument in arguments {
                result = result.concat(argument.text()?)?;
            }
            StoredValue::String(result)
        }
        StringMethod::IndexOf => {
            let needle = argument(0)?.text()?;
            let start = match argument(1)? {
                ConvertedArgument::Absent => 0,
                converted => clamp_to_length(converted.integer()?, length),
            };
            StoredValue::Number(JsNumber::from_f64(
                find_forward(subject, needle, start).map_or(-1.0, f64::from),
            ))
        }
        StringMethod::LastIndexOf => {
            let needle = argument(0)?.text()?;
            // A `NaN` position means "search from the end", which is why this
            // argument keeps its Number shape.
            let ConvertedArgument::Number(position) = argument(1)? else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "String.prototype.lastIndexOf lost its Number position",
                }
                .into());
            };
            let position = position.as_f64();
            let start = if position.is_nan() {
                length
            } else {
                clamp_to_length(
                    number_to_integer_or_infinity(JsNumber::from_f64(position)),
                    length,
                )
            };
            StoredValue::Number(JsNumber::from_f64(
                find_backward(subject, needle, start).map_or(-1.0, f64::from),
            ))
        }
        StringMethod::Includes => {
            let needle = argument(0)?.text()?;
            let start = match argument(1)? {
                ConvertedArgument::Absent => 0,
                converted => clamp_to_length(converted.integer()?, length),
            };
            StoredValue::Boolean(find_forward(subject, needle, start).is_some())
        }
        StringMethod::StartsWith => {
            let needle = argument(0)?.text()?;
            let start = match argument(1)? {
                ConvertedArgument::Absent => 0,
                converted => clamp_to_length(converted.integer()?, length),
            };
            StoredValue::Boolean(matches_at(subject, needle, start))
        }
        StringMethod::EndsWith => {
            let needle = argument(0)?.text()?;
            let end = match argument(1)? {
                ConvertedArgument::Absent => length,
                converted => clamp_to_length(converted.integer()?, length),
            };
            StoredValue::Boolean(
                end.checked_sub(needle.len())
                    .is_some_and(|start| matches_at(subject, needle, start)),
            )
        }
        StringMethod::Slice => {
            // `slice` accepts negative endpoints and yields the empty string
            // when they cross, unlike `substring`.
            let start = relative_bound(argument(0)?.integer()?, length);
            let end = match argument(1)? {
                ConvertedArgument::Absent => length,
                converted => relative_bound(converted.integer()?, length),
            };
            StoredValue::String(subject.slice(start..end.max(start))?)
        }
        StringMethod::Substring => {
            // `substring` clamps each endpoint into range and then swaps them
            // when they are out of order.
            let first = clamp_to_length(argument(0)?.integer()?, length);
            let second = match argument(1)? {
                ConvertedArgument::Absent => length,
                converted => clamp_to_length(converted.integer()?, length),
            };
            let (start, end) = if first <= second {
                (first, second)
            } else {
                (second, first)
            };
            StoredValue::String(subject.slice(start..end)?)
        }
        StringMethod::Substr => {
            // The Annex B `substr` takes a length rather than an end index, and
            // a negative start counts from the end.
            let start = relative_bound(argument(0)?.integer()?, length);
            let count = match argument(1)? {
                ConvertedArgument::Absent => f64::from(length - start),
                converted => converted.integer()?,
            };
            let available = f64::from(length - start);
            let count = count.clamp(0.0, available);
            // The clamp proves the value is a non-negative integer below 2^32.
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the preceding clamp bounds the count by the remaining length"
            )]
            let count = count as u32;
            StoredValue::String(subject.slice(start..start + count)?)
        }
        StringMethod::Repeat => {
            let count = argument(0)?.integer()?;
            // A negative or infinite count is a `RangeError`, which the oracle
            // reports as `invalid repeat count`.
            if count < 0.0 || count.is_infinite() {
                return Err(NativeFailure::Abrupt(PendingException {
                    realm: state.realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::RangeError,
                        message: JsString::from_utf8("invalid repeat count")?,
                    },
                    origin: state.origin.clone(),
                }));
            }
            StoredValue::String(repeat_string(subject, count, state)?)
        }
        StringMethod::Replace => {
            return Err(EngineFault::RuntimeInvariant {
                message: "String.prototype.replace entered the simple String method machine",
            }
            .into());
        }
        StringMethod::PadStart | StringMethod::PadEnd => {
            let target = argument(0)?.integer()?;
            let filler = match argument(1)? {
                ConvertedArgument::Absent => JsString::from_utf8(" ")?,
                converted => converted.text()?.clone(),
            };
            StoredValue::String(pad_string(
                subject,
                target,
                &filler,
                matches!(state.method, StringMethod::PadStart),
                state,
            )?)
        }
        StringMethod::Trim | StringMethod::TrimStart | StringMethod::TrimEnd => {
            let trim_start = matches!(state.method, StringMethod::Trim | StringMethod::TrimStart);
            let trim_end = matches!(state.method, StringMethod::Trim | StringMethod::TrimEnd);
            let mut start = 0;
            let mut end = length;
            if trim_start {
                while start < end && subject.code_unit_at(start).is_some_and(is_trimmable) {
                    start += 1;
                }
            }
            if trim_end {
                while end > start && subject.code_unit_at(end - 1).is_some_and(is_trimmable) {
                    end -= 1;
                }
            }
            StoredValue::String(subject.slice(start..end)?)
        }
        // The two `String` statics ignore `subject` and build their result from
        // the converted arguments alone. They differ in coercion and range:
        // `fromCharCode` reduces each argument modulo 2^16, so
        // `String.fromCharCode(65601)` is `"A"`, while `fromCodePoint` rejects
        // anything that is not an exact code point.
        StringMethod::FromCharCode | StringMethod::FromCodePoint => {
            let code_points = matches!(state.method, StringMethod::FromCodePoint);
            let mut units = Vec::new();
            for argument in arguments {
                let ConvertedArgument::Number(number) = argument else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "a String factory read a non-Number argument",
                    }
                    .into());
                };
                if code_points {
                    let code_point = validated_code_point(*number, state)?;
                    units
                        .try_reserve(2)
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 2,
                        })?;
                    // A supplementary code point becomes a surrogate pair, so
                    // `String.fromCodePoint(0x1F600).length` is `2`.
                    if let Some(offset) = code_point.checked_sub(0x1_0000) {
                        let high = u16::try_from(0xd800 + (offset >> 10))
                            .expect("the code-point bound proves the high surrogate fits");
                        let low = u16::try_from(0xdc00 + (offset & 0x3ff))
                            .expect("the mask proves the low surrogate fits");
                        units.push(high);
                        units.push(low);
                    } else {
                        units.push(
                            u16::try_from(code_point).expect("a BMP code point fits one code unit"),
                        );
                    }
                } else {
                    units
                        .try_reserve(1)
                        .map_err(|_| ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 1,
                        })?;
                    units.push(number_to_uint16(*number));
                }
            }
            StoredValue::String(JsString::from_code_units(units)?)
        }
        StringMethod::IsWellFormed => StoredValue::Boolean(is_well_formed(subject)),
        StringMethod::ToWellFormed => StoredValue::String(to_well_formed(subject)?),
        StringMethod::ToLowerCase | StringMethod::ToLocaleLowerCase => {
            execution_budget.charge_instructions(u64::from(subject.len()).saturating_add(1))?;
            let result = transform_unicode_segments(subject, UnicodeTransform::Lowercase)?;
            execution_budget.charge_instructions(u64::from(result.len()))?;
            StoredValue::String(result)
        }
        StringMethod::ToUpperCase | StringMethod::ToLocaleUpperCase => {
            execution_budget.charge_instructions(u64::from(subject.len()).saturating_add(1))?;
            let result = transform_unicode_segments(subject, UnicodeTransform::Uppercase)?;
            execution_budget.charge_instructions(u64::from(result.len()))?;
            StoredValue::String(result)
        }
        StringMethod::Normalize => {
            let form = match argument(0)? {
                ConvertedArgument::Absent => NormalizationForm::Nfc,
                ConvertedArgument::Text(name) => {
                    let Some(form) = normalization_form(name) else {
                        return Err(NativeFailure::Abrupt(PendingException {
                            realm: state.realm,
                            payload: PendingExceptionPayload::EngineError {
                                kind: ExceptionKind::RangeError,
                                message: JsString::from_utf8("bad normalization form")?,
                            },
                            origin: state.origin.clone(),
                        }));
                    };
                    form
                }
                ConvertedArgument::Integer(_) | ConvertedArgument::Number(_) => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "String.prototype.normalize lost its form String",
                    }
                    .into());
                }
            };
            execution_budget.charge_instructions(u64::from(subject.len()).saturating_add(1))?;
            let result = transform_unicode_segments(subject, UnicodeTransform::Normalize(form))?;
            execution_budget.charge_instructions(u64::from(result.len()))?;
            StoredValue::String(result)
        }
        StringMethod::LocaleCompare => {
            let that = argument(0)?.text()?;
            execution_budget.charge_instructions(
                u64::from(subject.len())
                    .saturating_add(u64::from(that.len()))
                    .saturating_add(1),
            )?;
            let left = transform_unicode_segments(
                subject,
                UnicodeTransform::Normalize(NormalizationForm::Nfc),
            )?;
            let right = transform_unicode_segments(
                that,
                UnicodeTransform::Normalize(NormalizationForm::Nfc),
            )?;
            execution_budget.charge_instructions(
                u64::from(left.len()).saturating_add(u64::from(right.len())),
            )?;
            let ordering = match left.cmp(&right) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            StoredValue::Number(JsNumber::from_i32(ordering))
        }
    };
    Ok(NativeDispatch::Immediate(value))
}

/// One of the four normalization forms admitted by ECMA-262.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NormalizationForm {
    Nfc,
    Nfd,
    Nfkc,
    Nfkd,
}

/// One Unicode transform applied independently to scalar runs separated by a
/// lone surrogate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnicodeTransform {
    Lowercase,
    Uppercase,
    Normalize(NormalizationForm),
}

/// Resolves an exact normalization-form name without lossy UTF-8 conversion.
fn normalization_form(name: &JsString) -> Option<NormalizationForm> {
    if js_string_is_ascii(name, b"NFC") {
        Some(NormalizationForm::Nfc)
    } else if js_string_is_ascii(name, b"NFD") {
        Some(NormalizationForm::Nfd)
    } else if js_string_is_ascii(name, b"NFKC") {
        Some(NormalizationForm::Nfkc)
    } else if js_string_is_ascii(name, b"NFKD") {
        Some(NormalizationForm::Nfkd)
    } else {
        None
    }
}

fn js_string_is_ascii(value: &JsString, expected: &[u8]) -> bool {
    usize::try_from(value.len()).ok() == Some(expected.len())
        && value
            .code_units()
            .zip(expected)
            .all(|(actual, expected)| actual == u16::from(*expected))
}

/// Applies full Unicode case conversion or normalization while preserving lone
/// UTF-16 surrogates as ECMAScript code points.
///
/// ICU4X accepts Unicode scalar strings. Splitting at every unpaired surrogate
/// is semantically exact: surrogate code points have no mapping, are not case
/// ignorable, and break both normalization sequences and case context.
fn transform_unicode_segments(
    subject: &JsString,
    transform: UnicodeTransform,
) -> Result<JsString, JsStringError> {
    let mut output = Vec::new();
    let mut segment = String::new();
    for decoded in char::decode_utf16(subject.code_units()) {
        match decoded {
            Ok(character) => {
                let additional = character.len_utf8();
                segment
                    .try_reserve(additional)
                    .map_err(|_| JsStringError::AllocationFailed { additional })?;
                segment.push(character);
            }
            Err(error) => {
                flush_unicode_segment(&segment, transform, &mut output)?;
                segment.clear();
                push_utf16_unit(&mut output, error.unpaired_surrogate())?;
            }
        }
    }
    flush_unicode_segment(&segment, transform, &mut output)?;
    JsString::from_code_units(output)
}

/// Writes one valid scalar segment through the selected ICU4X operation.
fn flush_unicode_segment(
    segment: &str,
    transform: UnicodeTransform,
    output: &mut Vec<u16>,
) -> Result<(), JsStringError> {
    if segment.is_empty() {
        return Ok(());
    }
    let mut sink = FallibleUtf16Sink::new(output);
    let result = match transform {
        UnicodeTransform::Lowercase => CaseMapperBorrowed::new()
            .lowercase(segment, &LanguageIdentifier::UNKNOWN)
            .write_to(&mut sink),
        UnicodeTransform::Uppercase => CaseMapperBorrowed::new()
            .uppercase(segment, &LanguageIdentifier::UNKNOWN)
            .write_to(&mut sink),
        UnicodeTransform::Normalize(NormalizationForm::Nfc) => {
            ComposingNormalizerBorrowed::new_nfc().normalize_to(segment, &mut sink)
        }
        UnicodeTransform::Normalize(NormalizationForm::Nfd) => {
            DecomposingNormalizerBorrowed::new_nfd().normalize_to(segment, &mut sink)
        }
        UnicodeTransform::Normalize(NormalizationForm::Nfkc) => {
            ComposingNormalizerBorrowed::new_nfkc().normalize_to(segment, &mut sink)
        }
        UnicodeTransform::Normalize(NormalizationForm::Nfkd) => {
            DecomposingNormalizerBorrowed::new_nfkd().normalize_to(segment, &mut sink)
        }
    };
    if result.is_err() {
        return Err(sink
            .failure
            .take()
            .unwrap_or(JsStringError::AllocationFailed { additional: 1 }));
    }
    Ok(())
}

/// A `fmt::Write` sink that encodes ICU4X output back into fallibly-grown
/// ECMAScript UTF-16 storage.
struct FallibleUtf16Sink<'a> {
    output: &'a mut Vec<u16>,
    failure: Option<JsStringError>,
}

impl<'a> FallibleUtf16Sink<'a> {
    fn new(output: &'a mut Vec<u16>) -> Self {
        Self {
            output,
            failure: None,
        }
    }
}

impl fmt::Write for FallibleUtf16Sink<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let additional = text.encode_utf16().count();
        let requested = self.output.len().saturating_add(additional);
        if requested > MAX_STRING_CODE_UNITS as usize {
            self.failure = Some(JsStringError::TooLong {
                requested: u64::try_from(requested).unwrap_or(u64::MAX),
                maximum: MAX_STRING_CODE_UNITS,
            });
            return Err(fmt::Error);
        }
        if self.output.try_reserve(additional).is_err() {
            self.failure = Some(JsStringError::AllocationFailed { additional });
            return Err(fmt::Error);
        }
        self.output.extend(text.encode_utf16());
        Ok(())
    }
}

fn push_utf16_unit(output: &mut Vec<u16>, unit: u16) -> Result<(), JsStringError> {
    let requested = output.len().saturating_add(1);
    if requested > MAX_STRING_CODE_UNITS as usize {
        return Err(JsStringError::TooLong {
            requested: u64::try_from(requested).unwrap_or(u64::MAX),
            maximum: MAX_STRING_CODE_UNITS,
        });
    }
    output
        .try_reserve(1)
        .map_err(|_| JsStringError::AllocationFailed { additional: 1 })?;
    output.push(unit);
    Ok(())
}

/// Converts an exact index into range, rejecting anything outside `0..length`.
///
/// The integer has already been truncated, so this only bounds it.
fn clamp_index(index: f64, length: u32) -> Option<u32> {
    if index < 0.0 || index >= f64::from(length) {
        return None;
    }
    // The bounds prove the value is a non-negative integer below `length`.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the preceding range check bounds the index by the subject length"
    )]
    let index = index as u32;
    Some(index)
}

/// Resolves a possibly negative relative index, as `at` defines it.
fn relative_index(index: f64, length: u32) -> Option<u32> {
    let resolved = if index < 0.0 {
        f64::from(length) + index
    } else {
        index
    };
    clamp_index(resolved, length)
}

/// Clamps an already-truncated integer into `0..=length`.
fn clamp_to_length(value: f64, length: u32) -> u32 {
    let clamped = value.clamp(0.0, f64::from(length));
    // The clamp proves the value is a non-negative integer at most `length`.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the preceding clamp bounds the value by the subject length"
    )]
    let clamped = clamped as u32;
    clamped
}

/// Resolves a relative endpoint, as `slice` and `substr` define it.
fn relative_bound(value: f64, length: u32) -> u32 {
    let resolved = if value < 0.0 {
        f64::from(length) + value
    } else {
        value
    };
    clamp_to_length(resolved, length)
}

/// Returns the code point starting at `index`, combining a surrogate pair.
///
/// A lone surrogate is returned as itself, which is what keeps
/// `"\uD800a".codePointAt(0)` equal to `0xD800`.
fn code_point_at(subject: &JsString, index: u32) -> Result<u32, NativeFailure> {
    let first = subject
        .code_unit_at(index)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "a String method read past the subject it had already bounded",
        })?;
    if !(0xd800..0xdc00).contains(&first) {
        return Ok(u32::from(first));
    }
    let Some(second) = subject.code_unit_at(index + 1) else {
        return Ok(u32::from(first));
    };
    if !(0xdc00..0xe000).contains(&second) {
        return Ok(u32::from(first));
    }
    let high = u32::from(first - 0xd800) << 10;
    let low = u32::from(second - 0xdc00);
    Ok(high + low + 0x1_0000)
}

/// Returns whether `needle` occurs at exactly `start`.
fn matches_at(subject: &JsString, needle: &JsString, start: u32) -> bool {
    let Some(end) = start.checked_add(needle.len()) else {
        return false;
    };
    if end > subject.len() {
        return false;
    }
    (0..needle.len())
        .all(|offset| subject.code_unit_at(start + offset) == needle.code_unit_at(offset))
}

/// Returns the first index at or after `start` where `needle` occurs.
///
/// An empty needle matches at `start`, which is why `"hello".indexOf("", 99)` is
/// the subject length rather than `-1`.
pub(super) fn find_forward(subject: &JsString, needle: &JsString, start: u32) -> Option<u32> {
    let last = subject.len().checked_sub(needle.len())?;
    (start..=last).find(|index| matches_at(subject, needle, *index))
}

/// Returns the last index at or before `start` where `needle` occurs.
fn find_backward(subject: &JsString, needle: &JsString, start: u32) -> Option<u32> {
    let last = subject.len().checked_sub(needle.len())?;
    let start = start.min(last);
    (0..=start)
        .rev()
        .find(|index| matches_at(subject, needle, *index))
}

/// Returns whether a UTF-16 code unit is trimmed by `trim`.
///
/// The set is `WhiteSpace` plus `LineTerminator`, which is the same set
/// `StringToNumber` skips, so `"\u00a0\ufeff ab".trim()` is `"ab"`.
fn is_trimmable(unit: u16) -> bool {
    matches!(
        unit,
        0x0009..=0x000d
            | 0x0020
            | 0x00a0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028..=0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}

/// Repeats `subject` `count` times, failing closed when the result is too long.
fn repeat_string(
    subject: &JsString,
    count: f64,
    state: &StringMethodContinuation,
) -> Result<JsString, NativeFailure> {
    // The caller rejected a negative or infinite count, so the product is the
    // only remaining overflow risk and it is checked rather than truncated.
    let total = count * f64::from(subject.len());
    if total > f64::from(u32::MAX) {
        return Err(string_too_long(state)?);
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the preceding bound proves the repeat count fits the u32 domain"
    )]
    let count = count as u32;
    let mut result = JsString::empty();
    for _ in 0..count {
        result = result.concat(subject)?;
    }
    Ok(result)
}

/// Pads `subject` up to `target` code units with `filler`.
fn pad_string(
    subject: &JsString,
    target: f64,
    filler: &JsString,
    at_start: bool,
    state: &StringMethodContinuation,
) -> Result<JsString, NativeFailure> {
    if target > f64::from(u32::MAX) {
        return Err(string_too_long(state)?);
    }
    let target = clamp_to_length(target, u32::MAX);
    // A target inside the subject, or an empty filler, leaves it unchanged.
    let Some(needed) = target.checked_sub(subject.len()) else {
        return Ok(subject.clone());
    };
    if needed == 0 || filler.is_empty() {
        return Ok(subject.clone());
    }
    let mut padding = JsString::empty();
    while padding.len() < needed {
        padding = padding.concat(filler)?;
    }
    let padding = padding.slice(0..needed)?;
    if at_start {
        Ok(padding.concat(subject)?)
    } else {
        Ok(subject.concat(&padding)?)
    }
}

/// Returns whether every surrogate in `subject` is paired.
fn is_well_formed(subject: &JsString) -> bool {
    let mut index = 0;
    while index < subject.len() {
        let Some(unit) = subject.code_unit_at(index) else {
            return false;
        };
        if (0xdc00..0xe000).contains(&unit) {
            // A trailing surrogate with no leading one is unpaired.
            return false;
        }
        if (0xd800..0xdc00).contains(&unit) {
            let paired = subject
                .code_unit_at(index + 1)
                .is_some_and(|next| (0xdc00..0xe000).contains(&next));
            if !paired {
                return false;
            }
            index += 2;
            continue;
        }
        index += 1;
    }
    true
}

/// Replaces every unpaired surrogate with `U+FFFD`.
fn to_well_formed(subject: &JsString) -> Result<JsString, JsStringError> {
    const REPLACEMENT: u16 = 0xfffd;
    let mut units = Vec::new();
    let hint = usize::try_from(subject.len()).unwrap_or(usize::MAX);
    units
        .try_reserve_exact(hint)
        .map_err(|_| JsStringError::AllocationFailed { additional: hint })?;
    let mut index = 0;
    while index < subject.len() {
        let Some(unit) = subject.code_unit_at(index) else {
            break;
        };
        if (0xd800..0xdc00).contains(&unit) {
            let next = subject.code_unit_at(index + 1);
            if next.is_some_and(|next| (0xdc00..0xe000).contains(&next)) {
                units.push(unit);
                units.push(next.expect("the pair was just tested"));
                index += 2;
                continue;
            }
            units.push(REPLACEMENT);
            index += 1;
            continue;
        }
        if (0xdc00..0xe000).contains(&unit) {
            units.push(REPLACEMENT);
            index += 1;
            continue;
        }
        units.push(unit);
        index += 1;
    }
    JsString::from_code_units(units)
}

/// Builds the `InternalError` a too-long result reports.
fn string_too_long(state: &StringMethodContinuation) -> Result<NativeFailure, NativeFailure> {
    Ok(NativeFailure::Abrupt(PendingException {
        realm: state.realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::InternalError,
            message: JsString::from_utf8("string too long")?,
        },
        origin: state.origin.clone(),
    }))
}

/// Validates one `String.fromCodePoint` argument.
///
/// A code point must be an exact integer in `0..=0x10FFFF`; anything else
/// reports `RangeError: invalid code point`, which the pinned oracle confirms
/// for `1.5`, `-1`, `NaN`, `Infinity`, and `0x110000`.
fn validated_code_point(
    value: JsNumber,
    state: &StringMethodContinuation,
) -> Result<u32, NativeFailure> {
    let value = value.as_f64();
    // Integrality is an exact property, so the comparison is deliberately exact.
    #[expect(
        clippy::float_cmp,
        reason = "a code point is an exact integer, so an epsilon comparison would admit the wrong values"
    )]
    let integral = value.is_finite() && value.trunc() == value;
    if integral && (0.0..=1_114_111.0).contains(&value) {
        // The range check proves the truncation is exact.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the preceding range check bounds the code point by 0x10FFFF"
        )]
        let code_point = value as u32;
        return Ok(code_point);
    }
    Err(NativeFailure::Abrupt(PendingException {
        realm: state.realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::RangeError,
            message: JsString::from_utf8("invalid code point")?,
        },
        origin: state.origin.clone(),
    }))
}
