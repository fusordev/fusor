/*
 * JavaScript Date semantics derived from ECMA-262 and QuickJS.
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

//! UTC and time-value foundation for the ES2025 `%Date%` intrinsic.

use temporal_rs::{
    Calendar, Instant, PlainDateTime, Temporal, TimeZone, ZonedDateTime, options::Disambiguation,
};

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

const MS_PER_SECOND: f64 = 1_000.0;
const MS_PER_MINUTE: f64 = 60.0 * MS_PER_SECOND;
const MS_PER_HOUR: f64 = 60.0 * MS_PER_MINUTE;
const MS_PER_DAY: f64 = 24.0 * MS_PER_HOUR;
const TIME_CLIP_BOUND: f64 = 8_640_000_000_000_000.0;
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

pub(super) struct DateUtcContinuation {
    arguments: Vec<StoredValue>,
    converted: Vec<JsNumber>,
}

pub(super) struct DateConstructorContinuation {
    arguments: Vec<StoredValue>,
    converted: Vec<JsNumber>,
    new_target: FunctionId,
}

pub(super) struct DateSetterContinuation {
    arguments: Vec<StoredValue>,
    converted: Vec<JsNumber>,
    object: ObjectId,
    original: JsNumber,
    method: DatePrototypeMethod,
}

pub(super) struct DateToJsonContinuation {
    receiver: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

impl DateConstructorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        usize_to_u64(self.arguments.len()).saturating_add(1)
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        for argument in &self.arguments {
            trace_stored_value_root(argument, mark);
        }
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
    }
}

impl DateUtcContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        usize_to_u64(self.arguments.len())
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        for argument in &self.arguments {
            trace_stored_value_root(argument, mark);
        }
    }
}

impl DateSetterContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        usize_to_u64(self.arguments.len()).saturating_add(1)
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        for argument in &self.arguments {
            trace_stored_value_root(argument, mark);
        }
        mark(CollectionRoot::Heap(HeapReference::Object(self.object)));
    }
}

impl DateToJsonContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.receiver, mark);
    }
}

pub(super) fn begin_date_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = inputs.new_target else {
        let rendered = current_local_date_string()?;
        return Ok(NativeDispatch::Immediate(StoredValue::String(
            JsString::from_utf8(&rendered)?,
        )));
    };
    let mut arguments = inputs.arguments.into_remaining_values();
    match arguments.len() {
        0 => begin_date_constructor_wrapper(
            runtime,
            realm,
            new_target,
            current_time_value(),
            return_to,
            Some(origin),
            execution_budget,
        ),
        1 => {
            let argument = arguments.pop().expect("one Date argument");
            if let StoredValue::Object(object) = argument
                && let Some(value) = runtime.date_value(object)?
            {
                return begin_date_constructor_wrapper(
                    runtime,
                    realm,
                    new_target,
                    value,
                    return_to,
                    Some(origin),
                    execution_budget,
                );
            }
            begin_operator_primitive_conversion(
                runtime,
                argument,
                OperatorPrimitiveHint::Default,
                OperatorPrimitiveTarget::DateConstructor { new_target },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        _ => begin_date_constructor_components(
            runtime,
            arguments,
            new_target,
            realm,
            return_to,
            &origin,
            execution_budget,
        ),
    }
}

fn begin_date_constructor_components(
    runtime: &mut Runtime,
    mut arguments: Vec<StoredValue>,
    new_target: FunctionId,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    arguments.truncate(7);
    let mut converted = Vec::new();
    converted
        .try_reserve_exact(arguments.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: arguments.len(),
        })?;
    advance_date_constructor_components(
        runtime,
        DateConstructorContinuation {
            arguments,
            converted,
            new_target,
        },
        None,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn advance_date_constructor_components(
    runtime: &mut Runtime,
    mut state: DateConstructorContinuation,
    completion: Option<JsNumber>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        state.converted.push(value);
    }
    if state.converted.len() == state.arguments.len() {
        let value = date_local_from_components(&state.converted);
        return begin_date_constructor_wrapper(
            runtime,
            realm,
            state.new_target,
            value,
            return_to,
            Some(origin.clone()),
            execution_budget,
        );
    }
    let index = state.converted.len();
    let argument = std::mem::replace(&mut state.arguments[index], StoredValue::Undefined);
    begin_operator_primitive_conversion(
        runtime,
        argument,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::DateConstructorComponents(Box::new(state)),
        realm,
        return_to,
        origin.clone(),
        execution_budget,
    )
}

pub(super) fn begin_date_constructor_wrapper(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: FunctionId,
    value: JsNumber,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    begin_intrinsic_get(
        runtime,
        realm,
        HeapReference::Function(new_target),
        StoredValue::Function(new_target),
        &prototype_key,
        IntrinsicGetContinuation::DateConstructor { new_target, value },
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_date_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    date_value: JsNumber,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_date_prototype(realm)?)
        }
    };
    let object = runtime.allocate_date_object(prototype, date_value)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(super) fn finish_date_constructor_primitive(
    runtime: &mut Runtime,
    value: StoredValue,
    new_target: FunctionId,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let value = match value {
        StoredValue::String(text) => parse_date_string(&text, realm, origin)?,
        value => time_clip(operator_to_number(value, realm, origin)?.as_f64()),
    };
    begin_date_constructor_wrapper(
        runtime,
        realm,
        new_target,
        value,
        return_to,
        Some(origin.clone()),
        execution_budget,
    )
}

pub(super) fn begin_date_static(
    runtime: &mut Runtime,
    method: DateStaticMethod,
    realm: RealmId,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        DateStaticMethod::Now => Ok(NativeDispatch::Immediate(StoredValue::Number(
            current_time_value(),
        ))),
        DateStaticMethod::Parse => begin_operator_primitive_conversion(
            runtime,
            arguments.take_first_or_undefined(),
            OperatorPrimitiveHint::String,
            OperatorPrimitiveTarget::DateParse,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        DateStaticMethod::Utc => begin_date_utc(
            runtime,
            arguments,
            realm,
            return_to,
            &origin,
            execution_budget,
        ),
    }
}

fn begin_date_utc(
    runtime: &mut Runtime,
    arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut values = arguments.into_remaining_values();
    values.truncate(7);
    let conversion_count = values.len();
    let mut converted = Vec::new();
    converted.try_reserve_exact(conversion_count).map_err(|_| {
        ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: conversion_count,
        }
    })?;
    advance_date_utc(
        runtime,
        DateUtcContinuation {
            arguments: values,
            converted,
        },
        None,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn advance_date_utc(
    runtime: &mut Runtime,
    mut state: DateUtcContinuation,
    completion: Option<JsNumber>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        state.converted.push(value);
    }
    if state.converted.len() == state.arguments.len() {
        return Ok(NativeDispatch::Immediate(StoredValue::Number(
            date_utc_from_components(&state.converted),
        )));
    }
    let index = state.converted.len();
    let argument = std::mem::replace(&mut state.arguments[index], StoredValue::Undefined);
    begin_operator_primitive_conversion(
        runtime,
        argument,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::DateUtc(Box::new(state)),
        realm,
        return_to,
        origin.clone(),
        execution_budget,
    )
}

pub(super) fn finish_date_parse(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let text = operator_primitive_to_string(value, realm, origin)?;
    Ok(NativeDispatch::Immediate(StoredValue::Number(
        parse_date_string(&text, realm, origin)?,
    )))
}

pub(super) fn finish_date_set_time(
    runtime: &mut Runtime,
    object: ObjectId,
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let value = time_clip(operator_to_number(value, realm, origin)?.as_f64());
    runtime.set_date_value(object, value)?;
    Ok(NativeDispatch::Immediate(StoredValue::Number(value)))
}

fn begin_date_to_json(
    runtime: &mut Runtime,
    receiver: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let receiver = match to_object_value(runtime, realm, receiver, origin.clone())? {
        Ok(receiver) => receiver,
        Err(exception) => return Err(NativeFailure::Abrupt(exception)),
    };
    begin_operator_primitive_conversion(
        runtime,
        receiver.duplicate(),
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::DateToJson(Box::new(DateToJsonContinuation {
            receiver,
            realm,
            origin: origin.clone(),
        })),
        realm,
        return_to,
        origin.clone(),
        execution_budget,
    )
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "operator completion ownership matches the ToPrimitive decision boundary"
)]
pub(super) fn begin_date_to_json_invoke(
    runtime: &mut Runtime,
    state: DateToJsonContinuation,
    primitive: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Number(number) = primitive
        && !number.as_f64().is_finite()
    {
        return Ok(NativeDispatch::Immediate(StoredValue::Null));
    }
    let key = runtime.predefined_property_key(PredefinedAtom::ToIsoString);
    charge_heap_property_lookup(runtime, &state.receiver, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        &state.receiver,
        key,
        Some(&JsString::from_utf8("toISOString")?),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::DateToJson,
        |state, method| finish_date_to_json_call(state, method, return_to),
        "Date toJSON Get produced a structured result",
    )
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Invoke completion ownership matches the dynamic Get decision boundary"
)]
pub(super) fn finish_date_to_json_call(
    state: DateToJsonContinuation,
    method: StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(function) = method else {
        return date_type_error(state.realm, &state.origin, "toISOString is not callable");
    };
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver: state.receiver,
        arguments: CallArguments::empty(),
        return_to,
        origin: state.origin,
        continuations: Vec::new(),
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

fn begin_date_to_primitive(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if !matches!(receiver, StoredValue::Function(_) | StoredValue::Object(_)) {
        return date_type_error(
            realm,
            &origin,
            "Date @@toPrimitive receiver is not an object",
        );
    }
    let hint = match arguments.take_first_or_undefined() {
        StoredValue::String(value) if string_equals_ascii(&value, "number") => {
            OperatorPrimitiveHint::Number
        }
        StoredValue::String(value)
            if string_equals_ascii(&value, "string") || string_equals_ascii(&value, "default") =>
        {
            OperatorPrimitiveHint::String
        }
        _ => return date_type_error(realm, &origin, "invalid Date @@toPrimitive hint"),
    };
    begin_ordinary_primitive_conversion(
        runtime,
        receiver.duplicate(),
        hint,
        OperatorPrimitiveTarget::DateToPrimitive,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn string_equals_ascii(value: &JsString, expected: &str) -> bool {
    usize::try_from(value.len()).ok() == Some(expected.len())
        && expected
            .bytes()
            .zip(0_u32..)
            .all(|(byte, index)| value.code_unit_at(index) == Some(u16::from(byte)))
}

fn date_type_error(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    }))
}

#[allow(
    clippy::too_many_arguments,
    reason = "native setter dispatch carries the receiver state plus the standard VM continuation context"
)]
fn begin_date_setter(
    runtime: &mut Runtime,
    method: DatePrototypeMethod,
    object: ObjectId,
    original: JsNumber,
    arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let limit = date_setter_argument_limit(method);
    let mut values = arguments.into_remaining_values();
    values.truncate(limit);
    if values.is_empty() {
        values
            .try_reserve(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::FrameValues,
                additional: 1,
            })?;
        values.push(StoredValue::Undefined);
    }
    let mut converted = Vec::new();
    converted
        .try_reserve_exact(values.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: values.len(),
        })?;
    advance_date_setter(
        runtime,
        DateSetterContinuation {
            arguments: values,
            converted,
            object,
            original,
            method,
        },
        None,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn advance_date_setter(
    runtime: &mut Runtime,
    mut state: DateSetterContinuation,
    completion: Option<JsNumber>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        state.converted.push(value);
    }
    if state.converted.len() == state.arguments.len() {
        let (value, write) = apply_date_setter(state.method, state.original, &state.converted);
        if write {
            runtime.set_date_value(state.object, value)?;
        }
        return Ok(NativeDispatch::Immediate(StoredValue::Number(value)));
    }
    let index = state.converted.len();
    let argument = std::mem::replace(&mut state.arguments[index], StoredValue::Undefined);
    begin_operator_primitive_conversion(
        runtime,
        argument,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::DateSetter(Box::new(state)),
        realm,
        return_to,
        origin.clone(),
        execution_budget,
    )
}

fn date_setter_argument_limit(method: DatePrototypeMethod) -> usize {
    match method {
        DatePrototypeMethod::SetHours | DatePrototypeMethod::SetUtcHours => 4,
        DatePrototypeMethod::SetMinutes
        | DatePrototypeMethod::SetUtcMinutes
        | DatePrototypeMethod::SetFullYear
        | DatePrototypeMethod::SetUtcFullYear => 3,
        DatePrototypeMethod::SetSeconds
        | DatePrototypeMethod::SetUtcSeconds
        | DatePrototypeMethod::SetMonth
        | DatePrototypeMethod::SetUtcMonth => 2,
        DatePrototypeMethod::SetMilliseconds
        | DatePrototypeMethod::SetUtcMilliseconds
        | DatePrototypeMethod::SetDate
        | DatePrototypeMethod::SetUtcDate
        | DatePrototypeMethod::SetYear => 1,
        _ => unreachable!("non-component Date setter"),
    }
}

#[derive(Clone, Copy)]
struct DateSetterFields {
    year: f64,
    month: f64,
    date: f64,
    hour: f64,
    minute: f64,
    second: f64,
    millisecond: f64,
}

fn apply_date_setter(
    method: DatePrototypeMethod,
    original: JsNumber,
    converted: &[JsNumber],
) -> (JsNumber, bool) {
    let utc = matches!(
        method,
        DatePrototypeMethod::SetUtcMilliseconds
            | DatePrototypeMethod::SetUtcSeconds
            | DatePrototypeMethod::SetUtcMinutes
            | DatePrototypeMethod::SetUtcHours
            | DatePrototypeMethod::SetUtcDate
            | DatePrototypeMethod::SetUtcMonth
            | DatePrototypeMethod::SetUtcFullYear
    );
    let recovers_invalid = matches!(
        method,
        DatePrototypeMethod::SetYear
            | DatePrototypeMethod::SetFullYear
            | DatePrototypeMethod::SetUtcFullYear
    );
    let original = original.as_f64();
    let original_invalid = original.is_nan();
    if original_invalid && !recovers_invalid {
        return (JsNumber::from_f64(f64::NAN), false);
    }
    let base = if original_invalid && recovers_invalid {
        0.0
    } else {
        original
    };
    let Some(mut fields) = date_setter_fields(base, utc || original_invalid) else {
        return (JsNumber::from_f64(f64::NAN), true);
    };
    let value = |index: usize| converted[index].as_f64();
    match method {
        DatePrototypeMethod::SetMilliseconds | DatePrototypeMethod::SetUtcMilliseconds => {
            fields.millisecond = value(0);
        }
        DatePrototypeMethod::SetSeconds | DatePrototypeMethod::SetUtcSeconds => {
            fields.second = value(0);
            if converted.len() >= 2 {
                fields.millisecond = value(1);
            }
        }
        DatePrototypeMethod::SetMinutes | DatePrototypeMethod::SetUtcMinutes => {
            fields.minute = value(0);
            if converted.len() >= 2 {
                fields.second = value(1);
            }
            if converted.len() >= 3 {
                fields.millisecond = value(2);
            }
        }
        DatePrototypeMethod::SetHours | DatePrototypeMethod::SetUtcHours => {
            fields.hour = value(0);
            if converted.len() >= 2 {
                fields.minute = value(1);
            }
            if converted.len() >= 3 {
                fields.second = value(2);
            }
            if converted.len() >= 4 {
                fields.millisecond = value(3);
            }
        }
        DatePrototypeMethod::SetDate | DatePrototypeMethod::SetUtcDate => {
            fields.date = value(0);
        }
        DatePrototypeMethod::SetMonth | DatePrototypeMethod::SetUtcMonth => {
            fields.month = value(0);
            if converted.len() >= 2 {
                fields.date = value(1);
            }
        }
        DatePrototypeMethod::SetYear => {
            fields.year = annex_b_set_year_value(value(0));
        }
        DatePrototypeMethod::SetFullYear | DatePrototypeMethod::SetUtcFullYear => {
            fields.year = value(0);
            if converted.len() >= 2 {
                fields.month = value(1);
            }
            if converted.len() >= 3 {
                fields.date = value(2);
            }
        }
        _ => unreachable!("non-component Date setter"),
    }
    let date = make_day(fields.year, fields.month, fields.date) * MS_PER_DAY
        + make_time(
            fields.hour,
            fields.minute,
            fields.second,
            fields.millisecond,
        );
    let result = if utc {
        time_clip(date)
    } else {
        time_clip_local_date(date)
    };
    (result, true)
}

fn annex_b_set_year_value(year: f64) -> f64 {
    if year.is_nan() {
        return f64::NAN;
    }
    let year = year.trunc();
    if (0.0..=99.0).contains(&year) {
        year + 1900.0
    } else {
        year
    }
}

fn date_setter_fields(value: f64, utc: bool) -> Option<DateSetterFields> {
    if utc {
        let fields = temporal_utc_fields(value)?;
        Some(DateSetterFields {
            year: f64::from(fields.year),
            month: f64::from(fields.month - 1),
            date: f64::from(fields.date),
            hour: f64::from(fields.hour),
            minute: f64::from(fields.minute),
            second: f64::from(fields.second),
            millisecond: f64::from(fields.millisecond),
        })
    } else {
        let fields = temporal_local_date_time(value)?;
        Some(DateSetterFields {
            year: f64::from(fields.year()),
            month: f64::from(fields.month() - 1),
            date: f64::from(fields.day()),
            hour: f64::from(fields.hour()),
            minute: f64::from(fields.minute()),
            second: f64::from(fields.second()),
            millisecond: f64::from(fields.millisecond()),
        })
    }
}

fn time_clip_local_date(local_value: f64) -> JsNumber {
    let Some(milliseconds) = utc_time_from_local_value(local_value) else {
        return JsNumber::from_f64(f64::NAN);
    };
    if milliseconds.unsigned_abs() > 8_640_000_000_000_000_u64 {
        JsNumber::from_f64(f64::NAN)
    } else {
        JsNumber::from_i64(milliseconds)
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "native dispatch keeps the VM continuation inputs explicit"
)]
pub(super) fn dispatch_date_prototype(
    runtime: &mut Runtime,
    method: DatePrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(method, DatePrototypeMethod::SymbolToPrimitive) {
        return begin_date_to_primitive(
            runtime,
            receiver,
            arguments,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if matches!(method, DatePrototypeMethod::ToJson) {
        return begin_date_to_json(
            runtime,
            receiver.duplicate(),
            realm,
            return_to,
            &origin,
            execution_budget,
        );
    }
    let (object, value) = require_date_value(runtime, receiver, realm, &origin)?;
    if matches!(
        method,
        DatePrototypeMethod::ToLocaleString
            | DatePrototypeMethod::ToLocaleDateString
            | DatePrototypeMethod::ToLocaleTimeString
    ) {
        return begin_intl_date_to_locale_string(
            runtime,
            method,
            arguments,
            value,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    match method {
        DatePrototypeMethod::ValueOf | DatePrototypeMethod::GetTime => {
            Ok(NativeDispatch::Immediate(StoredValue::Number(value)))
        }
        DatePrototypeMethod::ToString
        | DatePrototypeMethod::ToDateString
        | DatePrototypeMethod::ToTimeString => {
            let rendered = local_date_string(method, value.as_f64());
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&rendered)?,
            )))
        }
        DatePrototypeMethod::ToIsoString => {
            let Some(rendered) = to_iso_string(value.as_f64()) else {
                return Err(NativeFailure::Abrupt(PendingException {
                    realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::RangeError,
                        message: JsString::from_utf8("invalid time value")?,
                    },
                    origin: origin.clone(),
                }));
            };
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&rendered)?,
            )))
        }
        DatePrototypeMethod::ToUtcString => {
            let rendered = to_utc_string(value.as_f64());
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&rendered)?,
            )))
        }
        DatePrototypeMethod::ToTemporalInstant => {
            let milliseconds = value.as_f64();
            if !milliseconds.is_finite() {
                return Err(NativeFailure::Abrupt(PendingException {
                    realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::RangeError,
                        message: JsString::from_utf8("invalid time value")?,
                    },
                    origin: origin.clone(),
                }));
            }
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a branded finite Date value is an integral TimeClip result within i64"
            )]
            let instant = Instant::from_epoch_milliseconds(milliseconds as i64).map_err(|_| {
                EngineFault::RuntimeInvariant {
                    message: "a valid Date escaped the shared Temporal.Instant range",
                }
            })?;
            let prototype = HeapReference::Object(runtime.realm_temporal_instant_prototype(realm)?);
            let object = runtime.allocate_temporal_instant(prototype, instant)?;
            Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
        }
        DatePrototypeMethod::GetUtcFullYear
        | DatePrototypeMethod::GetUtcMonth
        | DatePrototypeMethod::GetUtcDate
        | DatePrototypeMethod::GetUtcHours
        | DatePrototypeMethod::GetUtcMinutes
        | DatePrototypeMethod::GetUtcSeconds
        | DatePrototypeMethod::GetUtcMilliseconds
        | DatePrototypeMethod::GetUtcDay => Ok(NativeDispatch::Immediate(StoredValue::Number(
            utc_component(method, value.as_f64()),
        ))),
        DatePrototypeMethod::GetYear
        | DatePrototypeMethod::GetFullYear
        | DatePrototypeMethod::GetMonth
        | DatePrototypeMethod::GetDate
        | DatePrototypeMethod::GetHours
        | DatePrototypeMethod::GetMinutes
        | DatePrototypeMethod::GetSeconds
        | DatePrototypeMethod::GetMilliseconds
        | DatePrototypeMethod::GetDay
        | DatePrototypeMethod::GetTimezoneOffset => Ok(NativeDispatch::Immediate(
            StoredValue::Number(local_component(method, value.as_f64())),
        )),
        DatePrototypeMethod::SetTime => begin_operator_primitive_conversion(
            runtime,
            arguments.take_first_or_undefined(),
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::DateSetTime { object },
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        DatePrototypeMethod::SetMilliseconds
        | DatePrototypeMethod::SetUtcMilliseconds
        | DatePrototypeMethod::SetSeconds
        | DatePrototypeMethod::SetUtcSeconds
        | DatePrototypeMethod::SetMinutes
        | DatePrototypeMethod::SetUtcMinutes
        | DatePrototypeMethod::SetHours
        | DatePrototypeMethod::SetUtcHours
        | DatePrototypeMethod::SetDate
        | DatePrototypeMethod::SetUtcDate
        | DatePrototypeMethod::SetMonth
        | DatePrototypeMethod::SetUtcMonth
        | DatePrototypeMethod::SetYear
        | DatePrototypeMethod::SetFullYear
        | DatePrototypeMethod::SetUtcFullYear => begin_date_setter(
            runtime,
            method,
            object,
            value,
            arguments,
            realm,
            return_to,
            &origin,
            execution_budget,
        ),
        DatePrototypeMethod::ToJson | DatePrototypeMethod::SymbolToPrimitive => {
            unreachable!("generic Date method dispatched through branded path")
        }
        DatePrototypeMethod::ToLocaleString
        | DatePrototypeMethod::ToLocaleDateString
        | DatePrototypeMethod::ToLocaleTimeString => {
            unreachable!("locale Date methods dispatched through Intl.DateTimeFormat")
        }
    }
}

fn require_date_value(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<(ObjectId, JsNumber), NativeFailure> {
    if let StoredValue::Object(object) = receiver
        && let Some(value) = runtime.date_value(*object)?
    {
        return Ok((*object, value));
    }
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a Date object")?,
        },
        origin: origin.clone(),
    }))
}

fn current_time_value() -> JsNumber {
    Temporal::utc_now().instant().map_or_else(
        |_| JsNumber::from_f64(f64::NAN),
        |instant| JsNumber::from_i64(instant.epoch_milliseconds()),
    )
}

fn current_local_date_string() -> Result<String, EngineFault> {
    let date_time = Temporal::local_now()
        .zoned_date_time_iso(None)
        .map_err(|_| EngineFault::RuntimeInvariant {
            message: "temporal_rs could not resolve the host date and time",
        })?;
    Ok(format_local_date_string(&date_time))
}

fn format_local_date_string(date_time: &ZonedDateTime) -> String {
    format!(
        "{} {}",
        format_date_string(date_time),
        format_time_string(date_time)
    )
}

fn format_date_string(date_time: &ZonedDateTime) -> String {
    let year = date_time.year();
    let year_sign = if year < 0 { "-" } else { "" };
    let year = year.unsigned_abs();

    format!(
        "{weekday} {month_name} {date:02} {year_sign}{year:04}",
        weekday = WEEKDAYS[usize::from(date_time.day_of_week() % 7)],
        month_name = MONTHS[usize::from(date_time.month()) - 1],
        date = date_time.day(),
    )
}

fn format_time_string(date_time: &ZonedDateTime) -> String {
    // TimeZoneString first truncates the nanosecond offset to milliseconds,
    // then renders only its hour and minute components. The optional
    // implementation-defined parenthesized time-zone name is intentionally
    // empty so this representation is stable across hosts.
    let offset_milliseconds = date_time.offset_nanoseconds() / 1_000_000;
    let offset_sign = if offset_milliseconds < 0 { '-' } else { '+' };
    let absolute_offset = offset_milliseconds.unsigned_abs();
    let offset_hour = absolute_offset / 3_600_000;
    let offset_minute = (absolute_offset / 60_000) % 60;

    format!(
        "{hour:02}:{minute:02}:{second:02} GMT{offset_sign}{offset_hour:02}{offset_minute:02}",
        hour = date_time.hour(),
        minute = date_time.minute(),
        second = date_time.second(),
    )
}

fn local_date_string(method: DatePrototypeMethod, value: f64) -> String {
    let Some(date_time) = temporal_local_date_time(value) else {
        return "Invalid Date".to_owned();
    };
    match method {
        DatePrototypeMethod::ToString => format_local_date_string(&date_time),
        DatePrototypeMethod::ToDateString => format_date_string(&date_time),
        DatePrototypeMethod::ToTimeString => format_time_string(&date_time),
        _ => unreachable!("non-string Date method"),
    }
}

pub(super) fn time_clip(value: f64) -> JsNumber {
    if !value.is_finite() || value.abs() > TIME_CLIP_BOUND {
        return JsNumber::from_f64(f64::NAN);
    }
    let truncated = value.trunc();
    JsNumber::from_f64(if truncated == 0.0 { 0.0 } else { truncated })
}

fn date_utc_from_components(values: &[JsNumber]) -> JsNumber {
    let mut components = [0.0; 7];
    components[0] = f64::NAN;
    components[2] = 1.0;
    for (index, value) in values.iter().enumerate() {
        components[index] = value.as_f64();
    }
    if components.iter().any(|value| !value.is_finite()) {
        return JsNumber::from_f64(f64::NAN);
    }
    let mut year = components[0].trunc();
    if (0.0..=99.0).contains(&year) {
        year += 1_900.0;
    }
    let day = make_day(year, components[1], components[2]);
    let time = make_time(components[3], components[4], components[5], components[6]);
    time_clip(day * MS_PER_DAY + time)
}

fn date_local_from_components(values: &[JsNumber]) -> JsNumber {
    let mut components = [0.0; 7];
    components[2] = 1.0;
    for (index, value) in values.iter().enumerate() {
        components[index] = value.as_f64();
    }
    if components.iter().any(|value| !value.is_finite()) {
        return JsNumber::from_f64(f64::NAN);
    }
    let mut year = components[0].trunc();
    if (0.0..=99.0).contains(&year) {
        year += 1_900.0;
    }
    let local_value = make_day(year, components[1], components[2]) * MS_PER_DAY
        + make_time(components[3], components[4], components[5], components[6]);
    let Some(milliseconds) = utc_time_from_local_value(local_value) else {
        return JsNumber::from_f64(f64::NAN);
    };
    if milliseconds.unsigned_abs() > 8_640_000_000_000_000_u64 {
        JsNumber::from_f64(f64::NAN)
    } else {
        JsNumber::from_i64(milliseconds)
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "finite integral Date fields are bounded before conversion to Temporal components"
)]
fn plain_date_time_from_time_value(value: f64) -> Option<PlainDateTime> {
    if !value.is_finite() || value.fract() != 0.0 {
        return None;
    }
    let day = (value / MS_PER_DAY).floor();
    if day.abs() > 400_000_000.0 {
        return None;
    }
    let within_day = value - day * MS_PER_DAY;
    let hour = (within_day / MS_PER_HOUR).floor();
    let minute = ((within_day % MS_PER_HOUR) / MS_PER_MINUTE).floor();
    let second = ((within_day % MS_PER_MINUTE) / MS_PER_SECOND).floor();
    let millisecond = within_day % MS_PER_SECOND;
    let (year, month, date) = civil_from_days(day as i64)?;
    PlainDateTime::try_new_iso(
        year,
        month,
        date,
        hour as u8,
        minute as u8,
        second as u8,
        millisecond as u16,
        0,
        0,
    )
    .ok()
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "finite integral Date components are normalized and bounded before civil-day conversion"
)]
fn make_day(year: f64, month: f64, date: f64) -> f64 {
    if !year.is_finite() || !month.is_finite() || !date.is_finite() {
        return f64::NAN;
    }
    let year = year.trunc();
    let month = month.trunc();
    let normalized_year = year + (month / 12.0).floor();
    if !normalized_year.is_finite() || normalized_year.abs() > 1_000_000.0 {
        return f64::NAN;
    }
    let normalized_month = month.rem_euclid(12.0) as u32 + 1;
    let first = days_from_civil(normalized_year as i64, normalized_month, 1);
    first as f64 + date.trunc() - 1.0
}

fn make_time(hour: f64, minute: f64, second: f64, millisecond: f64) -> f64 {
    if !hour.is_finite() || !minute.is_finite() || !second.is_finite() || !millisecond.is_finite() {
        return f64::NAN;
    }
    hour.trunc() * MS_PER_HOUR
        + minute.trunc() * MS_PER_MINUTE
        + second.trunc() * MS_PER_SECOND
        + millisecond.trunc()
}

fn utc_component(method: DatePrototypeMethod, value: f64) -> JsNumber {
    let Some(fields) = temporal_utc_fields(value) else {
        return JsNumber::from_f64(f64::NAN);
    };
    let component = match method {
        DatePrototypeMethod::GetUtcFullYear => i64::from(fields.year),
        DatePrototypeMethod::GetUtcMonth => i64::from(fields.month) - 1,
        DatePrototypeMethod::GetUtcDate => i64::from(fields.date),
        DatePrototypeMethod::GetUtcHours => i64::from(fields.hour),
        DatePrototypeMethod::GetUtcMinutes => i64::from(fields.minute),
        DatePrototypeMethod::GetUtcSeconds => i64::from(fields.second),
        DatePrototypeMethod::GetUtcMilliseconds => i64::from(fields.millisecond),
        // Temporal numbers Monday as 1 through Sunday as 7; legacy Date
        // numbers Sunday as 0 through Saturday as 6.
        DatePrototypeMethod::GetUtcDay => i64::from(fields.day_of_week % 7),
        DatePrototypeMethod::ValueOf
        | DatePrototypeMethod::ToString
        | DatePrototypeMethod::ToUtcString
        | DatePrototypeMethod::ToIsoString
        | DatePrototypeMethod::ToDateString
        | DatePrototypeMethod::ToTimeString
        | DatePrototypeMethod::ToLocaleString
        | DatePrototypeMethod::ToLocaleDateString
        | DatePrototypeMethod::ToLocaleTimeString
        | DatePrototypeMethod::GetTimezoneOffset
        | DatePrototypeMethod::GetTime
        | DatePrototypeMethod::GetYear
        | DatePrototypeMethod::GetFullYear
        | DatePrototypeMethod::GetMonth
        | DatePrototypeMethod::GetDate
        | DatePrototypeMethod::GetHours
        | DatePrototypeMethod::GetMinutes
        | DatePrototypeMethod::GetSeconds
        | DatePrototypeMethod::GetMilliseconds
        | DatePrototypeMethod::GetDay
        | DatePrototypeMethod::SetTime
        | DatePrototypeMethod::SetMilliseconds
        | DatePrototypeMethod::SetUtcMilliseconds
        | DatePrototypeMethod::SetSeconds
        | DatePrototypeMethod::SetUtcSeconds
        | DatePrototypeMethod::SetMinutes
        | DatePrototypeMethod::SetUtcMinutes
        | DatePrototypeMethod::SetHours
        | DatePrototypeMethod::SetUtcHours
        | DatePrototypeMethod::SetDate
        | DatePrototypeMethod::SetUtcDate
        | DatePrototypeMethod::SetMonth
        | DatePrototypeMethod::SetUtcMonth
        | DatePrototypeMethod::SetYear
        | DatePrototypeMethod::SetFullYear
        | DatePrototypeMethod::SetUtcFullYear
        | DatePrototypeMethod::ToTemporalInstant
        | DatePrototypeMethod::ToJson
        | DatePrototypeMethod::SymbolToPrimitive => {
            unreachable!("non-component Date method")
        }
    };
    JsNumber::from_i64(component)
}

fn local_component(method: DatePrototypeMethod, value: f64) -> JsNumber {
    let Some(fields) = temporal_local_date_time(value) else {
        return JsNumber::from_f64(f64::NAN);
    };
    match method {
        DatePrototypeMethod::GetYear => {
            JsNumber::from_i64(i64::from(fields.year()).saturating_sub(1900))
        }
        DatePrototypeMethod::GetFullYear => JsNumber::from_i64(i64::from(fields.year())),
        DatePrototypeMethod::GetMonth => JsNumber::from_i64(i64::from(fields.month()) - 1),
        DatePrototypeMethod::GetDate => JsNumber::from_i64(i64::from(fields.day())),
        DatePrototypeMethod::GetHours => JsNumber::from_i64(i64::from(fields.hour())),
        DatePrototypeMethod::GetMinutes => JsNumber::from_i64(i64::from(fields.minute())),
        DatePrototypeMethod::GetSeconds => JsNumber::from_i64(i64::from(fields.second())),
        DatePrototypeMethod::GetMilliseconds => JsNumber::from_i64(i64::from(fields.millisecond())),
        DatePrototypeMethod::GetDay => JsNumber::from_i64(i64::from(fields.day_of_week() % 7)),
        DatePrototypeMethod::GetTimezoneOffset => {
            timezone_offset_minutes(fields.offset_nanoseconds())
        }
        _ => unreachable!("non-local-component Date method"),
    }
}

fn timezone_offset_minutes(offset_nanoseconds: i64) -> JsNumber {
    if offset_nanoseconds == 0 {
        return JsNumber::from_i32(0);
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "time-zone offsets are far below Number's exact-integer boundary"
    )]
    let offset_nanoseconds = offset_nanoseconds as f64;
    JsNumber::from_f64(-offset_nanoseconds / (MS_PER_MINUTE * 1_000_000.0))
}

fn to_iso_string(value: f64) -> Option<String> {
    let fields = temporal_utc_fields(value)?;
    let year = if (0..=9_999).contains(&fields.year) {
        format!("{:04}", fields.year)
    } else if fields.year < 0 {
        format!("-{abs:06}", abs = fields.year.unsigned_abs())
    } else {
        format!("+{:06}", fields.year)
    };
    Some(format!(
        "{year}-{month:02}-{date:02}T{hour:02}:{minute:02}:{second:02}.{millisecond:03}Z",
        month = fields.month,
        date = fields.date,
        hour = fields.hour,
        minute = fields.minute,
        second = fields.second,
        millisecond = fields.millisecond,
    ))
}

fn to_utc_string(value: f64) -> String {
    let Some(fields) = temporal_utc_fields(value) else {
        return "Invalid Date".to_owned();
    };
    let year = if fields.year < 0 {
        format!("-{abs:04}", abs = fields.year.unsigned_abs())
    } else {
        format!("{:04}", fields.year)
    };
    format!(
        "{weekday}, {date:02} {month_name} {year} {hour:02}:{minute:02}:{second:02} GMT",
        weekday = WEEKDAYS[usize::from(fields.day_of_week % 7)],
        date = fields.date,
        month_name = MONTHS[usize::from(fields.month) - 1],
        hour = fields.hour,
        minute = fields.minute,
        second = fields.second,
    )
}

struct UtcDateFields {
    year: i32,
    month: u8,
    date: u8,
    hour: u8,
    minute: u8,
    second: u8,
    millisecond: u16,
    day_of_week: u16,
}

fn temporal_utc_fields(value: f64) -> Option<UtcDateFields> {
    let date_time = temporal_date_time(value, TimeZone::utc())?;
    Some(UtcDateFields {
        year: date_time.year(),
        month: date_time.month(),
        date: date_time.day(),
        hour: date_time.hour(),
        minute: date_time.minute(),
        second: date_time.second(),
        millisecond: date_time.millisecond(),
        day_of_week: date_time.day_of_week(),
    })
}

fn temporal_local_date_time(value: f64) -> Option<ZonedDateTime> {
    temporal_date_time(value, host_time_zone())
}

fn utc_time_from_local_value(local_value: f64) -> Option<i64> {
    let local_date_time = plain_date_time_from_time_value(local_value)?;
    local_date_time
        .to_zoned_date_time(host_time_zone(), Disambiguation::Compatible)
        .ok()
        .map(|date_time| date_time.epoch_milliseconds())
}

fn host_time_zone() -> TimeZone {
    Temporal::local_now()
        .time_zone()
        .unwrap_or_else(|_| TimeZone::utc())
}

fn temporal_date_time(value: f64, time_zone: TimeZone) -> Option<ZonedDateTime> {
    if !value.is_finite() || value.fract() != 0.0 {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the value is finite, integral, and already constrained by the Date TimeClip domain"
    )]
    let epoch_nanoseconds = value as i128 * 1_000_000;
    ZonedDateTime::try_new(epoch_nanoseconds, time_zone, Calendar::ISO).ok()
}

fn parse_date_string(
    value: &JsString,
    _realm: RealmId,
    _origin: &JsStackFrame,
) -> Result<JsNumber, NativeFailure> {
    let text = value.to_utf8_lossy()?;
    let milliseconds = match parse_iso_date(&text) {
        Some(ParsedDate::Utc(milliseconds)) => Some(milliseconds),
        Some(ParsedDate::Local(local_value)) => utc_time_from_local_value(local_value).map(
            #[allow(
                clippy::cast_precision_loss,
                reason = "Date epoch milliseconds are bounded below Number's exact-integer limit"
            )]
            |value| value as f64,
        ),
        None => parse_rendered_date(&text),
    };
    let Some(milliseconds) = milliseconds else {
        return Ok(JsNumber::from_f64(f64::NAN));
    };
    Ok(time_clip(milliseconds))
}

enum ParsedDate {
    Utc(f64),
    Local(f64),
}

fn parse_iso_date(text: &str) -> Option<ParsedDate> {
    let (date_text, time_text) = text
        .split_once('T')
        .map_or((text, None), |(date, time)| (date, Some(time)));
    let (year, month, date) = parse_iso_date_fields(date_text)?;
    let day = make_day(f64::from(year), f64::from(month - 1), f64::from(date));
    let Some(time_text) = time_text else {
        return Some(ParsedDate::Utc(day * MS_PER_DAY));
    };
    let (clock, offset_minutes) = parse_iso_time(time_text)?;
    let date_time = day * MS_PER_DAY + clock;
    Some(
        offset_minutes.map_or(ParsedDate::Local(date_time), |offset| {
            ParsedDate::Utc(date_time - f64::from(offset) * MS_PER_MINUTE)
        }),
    )
}

fn parse_iso_date_fields(text: &str) -> Option<(i32, u32, u32)> {
    let (year, rest) = if let Some(sign) = text.as_bytes().first().copied()
        && matches!(sign, b'+' | b'-')
    {
        let digits = text.get(1..7)?;
        if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let magnitude = digits.parse::<i32>().ok()?;
        if sign == b'-' && magnitude == 0 {
            return None;
        }
        let year = if sign == b'-' { -magnitude } else { magnitude };
        (year, text.get(7..).unwrap_or_default())
    } else {
        let digits = text.get(..4)?;
        if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        (
            digits.parse::<i32>().ok()?,
            text.get(4..).unwrap_or_default(),
        )
    };
    match rest.len() {
        0 => Some((year, 1, 1)),
        3 if rest.starts_with('-') => {
            let month = parse_two_digits(&rest[1..3])?;
            (1..=12).contains(&month).then_some((year, month, 1))
        }
        6 if rest.as_bytes().first() == Some(&b'-') && rest.as_bytes().get(3) == Some(&b'-') => {
            let month = parse_two_digits(&rest[1..3])?;
            let date = parse_two_digits(&rest[4..6])?;
            ((1..=12).contains(&month) && (1..=31).contains(&date)).then_some((year, month, date))
        }
        _ => None,
    }
}

fn parse_iso_time(text: &str) -> Option<(f64, Option<i32>)> {
    let (clock, offset) = if let Some(clock) = text.strip_suffix('Z') {
        (clock, Some(0))
    } else {
        let index = text
            .char_indices()
            .rev()
            .find_map(|(index, character)| matches!(character, '+' | '-').then_some(index));
        let Some(index) = index else {
            return parse_clock(text).map(|clock| (clock, None));
        };
        let sign = if text.as_bytes().get(index) == Some(&b'-') {
            -1_i32
        } else {
            1_i32
        };
        let zone = text.get(index + 1..)?;
        if zone.len() != 5 || zone.as_bytes().get(2) != Some(&b':') {
            return None;
        }
        let hours = i32::try_from(parse_two_digits(&zone[..2])?).ok()?;
        let minutes = i32::try_from(parse_two_digits(&zone[3..])?).ok()?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        (text.get(..index)?, Some(sign * (hours * 60 + minutes)))
    };
    Some((parse_clock(clock)?, offset))
}

fn parse_clock(clock: &str) -> Option<f64> {
    let mut parts = clock.split(':');
    let hour = parse_two_digits(parts.next()?)?;
    let minute = parse_two_digits(parts.next()?)?;
    let seconds = parts.next();
    if parts.next().is_some() || hour > 24 || minute > 59 {
        return None;
    }
    let (second, millisecond) = seconds.map_or(Some((0, 0)), parse_seconds)?;
    if second > 59 || (hour == 24 && (minute != 0 || second != 0 || millisecond != 0)) {
        return None;
    }
    Some(make_time(
        f64::from(hour),
        f64::from(minute),
        f64::from(second),
        f64::from(millisecond),
    ))
}

fn parse_rendered_date(text: &str) -> Option<f64> {
    let fields = text.split_ascii_whitespace().collect::<Vec<_>>();
    let (month, date, year, clock, offset_minutes) = match fields.as_slice() {
        [weekday, month, date, year, clock, zone]
            if WEEKDAYS.contains(weekday) && zone.starts_with("GMT") =>
        {
            (
                parse_month_name(month)?,
                parse_two_digits(date)?,
                year.parse::<i32>().ok()?,
                parse_clock(clock)?,
                parse_rendered_offset(zone)?,
            )
        }
        [weekday, date, month, year, clock, "GMT"]
            if weekday
                .strip_suffix(',')
                .is_some_and(|weekday| WEEKDAYS.contains(&weekday)) =>
        {
            (
                parse_month_name(month)?,
                parse_two_digits(date)?,
                year.parse::<i32>().ok()?,
                parse_clock(clock)?,
                0,
            )
        }
        _ => return None,
    };
    if !(1..=31).contains(&date) {
        return None;
    }
    let day = make_day(f64::from(year), f64::from(month - 1), f64::from(date));
    Some(day * MS_PER_DAY + clock - f64::from(offset_minutes) * MS_PER_MINUTE)
}

fn parse_month_name(value: &str) -> Option<u32> {
    MONTHS
        .iter()
        .position(|month| *month == value)
        .and_then(|index| u32::try_from(index + 1).ok())
}

fn parse_rendered_offset(value: &str) -> Option<i32> {
    let offset = value.strip_prefix("GMT")?;
    let (sign, digits) = match offset.as_bytes().first().copied()? {
        b'+' => (1_i32, offset.get(1..)?),
        b'-' => (-1_i32, offset.get(1..)?),
        _ => return None,
    };
    if digits.len() != 4 {
        return None;
    }
    let hours = i32::try_from(parse_two_digits(digits.get(..2)?)?).ok()?;
    let minutes = i32::try_from(parse_two_digits(digits.get(2..)?)?).ok()?;
    (hours <= 23 && minutes <= 59).then_some(sign * (hours * 60 + minutes))
}

fn parse_seconds(text: &str) -> Option<(u32, u32)> {
    let (seconds, fraction) = text
        .split_once('.')
        .map_or((text, None), |(seconds, fraction)| {
            (seconds, Some(fraction))
        });
    let seconds = parse_two_digits(seconds)?;
    let milliseconds = match fraction {
        None => 0,
        Some(fraction)
            if !fraction.is_empty()
                && fraction.len() <= 3
                && fraction.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            let fraction_digits = u32::try_from(fraction.len()).ok()?;
            fraction.parse::<u32>().ok()? * 10_u32.pow(3 - fraction_digits)
        }
        Some(_) => return None,
    };
    Some((seconds, milliseconds))
}

fn parse_two_digits(text: &str) -> Option<u32> {
    (text.len() == 2 && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse::<u32>().ok())
        .flatten()
}

fn days_from_civil(mut year: i64, month: u32, date: u32) -> i64 {
    year -= i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(date) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> Option<(i32, u8, u8)> {
    let shifted = days.checked_add(719_468)?;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let date = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = shifted_month + if shifted_month < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    Some((
        i32::try_from(year).ok()?,
        u8::try_from(month).ok()?,
        u8::try_from(date).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporal_bridge_covers_epoch_and_time_clip_bounds() {
        let epoch = temporal_utc_fields(0.0).expect("epoch");
        assert_eq!((epoch.year, epoch.month, epoch.date), (1970, 1, 1));
        let maximum = temporal_utc_fields(TIME_CLIP_BOUND).expect("maximum");
        assert_eq!(
            (maximum.year, maximum.month, maximum.date),
            (275_760, 9, 13)
        );
        let minimum = temporal_utc_fields(-TIME_CLIP_BOUND).expect("minimum");
        assert_eq!(
            (minimum.year, minimum.month, minimum.date),
            (-271_821, 4, 20)
        );
    }

    #[test]
    fn iso_rendering_covers_signed_years_and_negative_milliseconds() {
        assert_eq!(
            to_iso_string(0.0).as_deref(),
            Some("1970-01-01T00:00:00.000Z")
        );
        assert_eq!(
            to_iso_string(-1.0).as_deref(),
            Some("1969-12-31T23:59:59.999Z")
        );
        assert_eq!(
            to_iso_string(TIME_CLIP_BOUND).as_deref(),
            Some("+275760-09-13T00:00:00.000Z")
        );
    }

    #[test]
    fn local_date_rendering_uses_ecmascript_offset_shape_without_a_zone_name() {
        let eastern = TimeZone::try_from_identifier_str("-05:00").expect("fixed offset");
        let date_time =
            ZonedDateTime::try_new(0, eastern, Calendar::ISO).expect("local Unix epoch");
        assert_eq!(format_date_string(&date_time), "Wed Dec 31 1969");
        assert_eq!(format_time_string(&date_time), "19:00:00 GMT-0500");
        assert_eq!(
            format_local_date_string(&date_time),
            "Wed Dec 31 1969 19:00:00 GMT-0500"
        );
    }

    #[test]
    fn zero_timezone_offset_is_positive_zero() {
        assert_eq!(
            timezone_offset_minutes(0).as_f64().to_bits(),
            0.0_f64.to_bits()
        );
        assert_eq!(
            timezone_offset_minutes(3_600_000_000_000)
                .as_f64()
                .to_bits(),
            (-60.0_f64).to_bits()
        );
    }

    #[test]
    fn civil_day_inverse_covers_negative_time_and_date_boundaries() {
        for (year, month, date) in [
            (1970_i32, 1_u8, 1_u8),
            (1969, 12, 31),
            (2000, 2, 29),
            (-271_821, 4, 20),
            (275_760, 9, 13),
        ] {
            let days = days_from_civil(i64::from(year), u32::from(month), u32::from(date));
            assert_eq!(civil_from_days(days), Some((year, month, date)));
        }
        let before_epoch = plain_date_time_from_time_value(-1.0).expect("pre-epoch instant");
        assert_eq!(
            (
                before_epoch.year(),
                before_epoch.month(),
                before_epoch.day(),
                before_epoch.hour(),
                before_epoch.minute(),
                before_epoch.second(),
                before_epoch.millisecond(),
            ),
            (1969, 12, 31, 23, 59, 59, 999)
        );
    }
}
