use super::super::conversions::operator_primitive_to_string;
#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use core::str::FromStr;
use temporal_rs::{
    Calendar, MonthCode, PlainDate, PlainDateTime, PlainTime, TimeZone,
    fields::{CalendarFields, DateTimeFields},
    options::{
        DifferenceSettings, Disambiguation, DisplayCalendar, Overflow, RoundingIncrement,
        RoundingMode, RoundingOptions, ToStringRoundingOptions, Unit,
    },
    parsers::Precision,
    partial::{PartialDateTime, PartialTime},
};

/// The `Temporal.PlainDateTime` constructor retains its ISO date/time
/// components while every observable primitive conversion is resumed.  The
/// final calendar conversion follows the nine numeric components.
pub(in crate::vm) struct TemporalPlainDateTimeConstructorContinuation {
    arguments: Vec<StoredValue>,
    converted: Vec<JsNumber>,
    new_target: FunctionId,
}

const TEMPORAL_PLAIN_DATE_TIME_BAG_FIELDS: [&str; 11] = [
    "calendar",
    "day",
    "hour",
    "microsecond",
    "millisecond",
    "minute",
    "month",
    "monthCode",
    "nanosecond",
    "second",
    "year",
];

const TEMPORAL_PLAIN_DATE_TIME_WITH_FIELDS: [&str; 12] = [
    "calendar",
    "timeZone",
    "day",
    "hour",
    "microsecond",
    "millisecond",
    "minute",
    "month",
    "monthCode",
    "nanosecond",
    "second",
    "year",
];

#[derive(Clone, Copy)]
pub(in crate::vm) enum TemporalPlainDateTimeBagStage {
    ReadField,
    AwaitField,
    AwaitConversion,
}

/// Resumable `ToTemporalDateTime` conversion for ordinary property bags.
///
/// The field order is the Temporal `PrepareTemporalFields` order. Both
/// property access and primitive conversion may execute JavaScript, so the
/// complete bag is retained by the continuation until a final calendar/time
/// record is ready for `temporal_rs`.
pub(in crate::vm) struct TemporalPlainDateTimeBagContinuation {
    pub(super) base: StoredValue,
    pub(super) calendar: Option<Calendar>,
    pub(super) day: Option<JsNumber>,
    pub(super) hour: Option<JsNumber>,
    pub(super) microsecond: Option<JsNumber>,
    pub(super) millisecond: Option<JsNumber>,
    pub(super) minute: Option<JsNumber>,
    pub(super) month: Option<JsNumber>,
    pub(super) month_code: Option<TemporalPlainDateTimeMonthCode>,
    pub(super) nanosecond: Option<JsNumber>,
    pub(super) second: Option<JsNumber>,
    pub(super) year: Option<JsNumber>,
    pub(super) next: usize,
    pub(super) stage: TemporalPlainDateTimeBagStage,
    pub(super) target: TemporalPlainDateTimeLikeTarget,
    pub(super) realm: RealmId,
    pub(super) origin: JsStackFrame,
}

pub(super) enum TemporalPlainDateTimeMonthCode {
    Parsed(MonthCode),
    Raw(JsString),
}

impl TemporalPlainDateTimeBagContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        2
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.base, mark);
        self.target.trace_roots(mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalPlainDateTimeToZonedDateTimeStage {
    AwaitDisambiguation,
    AwaitDisambiguationConversion,
}

/// Resumable disambiguation option handling for
/// `Temporal.PlainDateTime.prototype.toZonedDateTime`.
pub(in crate::vm) struct TemporalPlainDateTimeToZonedDateTimeContinuation {
    date_time: PlainDateTime,
    time_zone: TimeZone,
    options: StoredValue,
    stage: TemporalPlainDateTimeToZonedDateTimeStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainDateTimeToZonedDateTimeContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

pub(in crate::vm) enum TemporalPlainDateTimeLikeTarget {
    From {
        options: StoredValue,
    },
    CompareFirst {
        second: StoredValue,
    },
    CompareSecond {
        first: PlainDateTime,
    },
    Equals {
        receiver: PlainDateTime,
    },
    Difference {
        receiver: PlainDateTime,
        options: StoredValue,
        since: bool,
    },
    With {
        receiver: PlainDateTime,
        options: StoredValue,
    },
    FromPlainDate {
        receiver: PlainDate,
    },
}

impl TemporalPlainDateTimeLikeTarget {
    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::From { options }
            | Self::CompareFirst { second: options }
            | Self::Difference { options, .. }
            | Self::With { options, .. } => {
                trace_stored_value_root(options, mark);
            }
            Self::CompareSecond { .. } | Self::Equals { .. } | Self::FromPlainDate { .. } => {}
        }
    }
}

fn temporal_plain_date_time_property_bag_fields(
    target: &TemporalPlainDateTimeLikeTarget,
) -> &'static [&'static str] {
    match target {
        TemporalPlainDateTimeLikeTarget::With { .. } => &TEMPORAL_PLAIN_DATE_TIME_WITH_FIELDS,
        TemporalPlainDateTimeLikeTarget::FromPlainDate { .. } => &TEMPORAL_PLAIN_TIME_BAG_FIELDS,
        TemporalPlainDateTimeLikeTarget::From { .. }
        | TemporalPlainDateTimeLikeTarget::CompareFirst { .. }
        | TemporalPlainDateTimeLikeTarget::CompareSecond { .. }
        | TemporalPlainDateTimeLikeTarget::Equals { .. }
        | TemporalPlainDateTimeLikeTarget::Difference { .. } => {
            &TEMPORAL_PLAIN_DATE_TIME_BAG_FIELDS
        }
    }
}

pub(in crate::vm) struct TemporalPlainDateTimeWithFields {
    year: Option<i64>,
    month: Option<i64>,
    month_code: Option<JsString>,
    day: Option<i64>,
    hour: Option<i64>,
    minute: Option<i64>,
    second: Option<i64>,
    millisecond: Option<i64>,
    microsecond: Option<i64>,
    nanosecond: Option<i64>,
}

impl TemporalPlainDateTimeConstructorContinuation {
    pub(in crate::vm) fn retained_values(&self) -> u64 {
        usize_to_u64(self.arguments.len()).saturating_add(1)
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        for argument in &self.arguments {
            trace_stored_value_root(argument, mark);
        }
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
    }
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "Temporal component conversion is resumable across user-defined primitive conversion"
)]
pub(in crate::vm) fn begin_temporal_plain_date_time_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = inputs.new_target else {
        return temporal_type_error(realm, &origin, "Temporal.PlainDateTime is not callable");
    };
    let mut arguments = inputs.arguments.into_remaining_values();
    arguments.truncate(10);
    arguments
        .try_reserve(10_usize.saturating_sub(arguments.len()))
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 10_usize.saturating_sub(arguments.len()),
        })?;
    while arguments.len() < 10 {
        arguments.push(StoredValue::Undefined);
    }
    let mut converted = Vec::new();
    converted
        .try_reserve_exact(9)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 9,
        })?;
    advance_temporal_plain_date_time_constructor(
        runtime,
        TemporalPlainDateTimeConstructorContinuation {
            arguments,
            converted,
            new_target,
        },
        None,
        realm,
        return_to,
        &origin,
        execution_budget,
    )
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "Temporal component conversion is resumable across user-defined primitive conversion"
)]
pub(in crate::vm) fn advance_temporal_plain_date_time_constructor(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeConstructorContinuation,
    completion: Option<JsNumber>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        state.converted.push(value);
    }
    while state.converted.len() < 9 {
        let index = state.converted.len();
        let argument = std::mem::replace(&mut state.arguments[index], StoredValue::Undefined);
        if index >= 3 && matches!(argument, StoredValue::Undefined) {
            state.converted.push(JsNumber::from_i64(0));
            continue;
        }
        return begin_operator_primitive_conversion(
            runtime,
            argument,
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::TemporalPlainDateTimeConstructor(Box::new(state)),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        );
    }

    let calendar = std::mem::replace(&mut state.arguments[9], StoredValue::Undefined);
    if matches!(calendar, StoredValue::Undefined) {
        return complete_temporal_plain_date_time_constructor(
            runtime,
            &state,
            Calendar::default(),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    let StoredValue::String(value) = calendar else {
        return temporal_type_error(
            realm,
            origin,
            "Temporal.PlainDateTime calendar must be a string",
        );
    };
    let calendar = match Calendar::from_str(&value.to_utf8_lossy()?) {
        Ok(calendar) => calendar,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    complete_temporal_plain_date_time_constructor(
        runtime,
        &state,
        calendar,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the completed constructor must still observe newTarget.prototype"
)]
fn complete_temporal_plain_date_time_constructor(
    runtime: &mut Runtime,
    state: &TemporalPlainDateTimeConstructorContinuation,
    calendar: Calendar,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let [
        year,
        month,
        day,
        hour,
        minute,
        second,
        millisecond,
        microsecond,
        nanosecond,
    ] = state.converted.as_slice()
    else {
        return Err(EngineFault::RuntimeInvariant {
            message: "Temporal.PlainDateTime constructor completed before all components converted",
        }
        .into());
    };
    let year = temporal_plain_date_time_integer(*year, realm, origin)?;
    let month = temporal_plain_date_time_integer(*month, realm, origin)?;
    let day = temporal_plain_date_time_integer(*day, realm, origin)?;
    let hour = temporal_plain_date_time_integer(*hour, realm, origin)?;
    let minute = temporal_plain_date_time_integer(*minute, realm, origin)?;
    let second = temporal_plain_date_time_integer(*second, realm, origin)?;
    let millisecond = temporal_plain_date_time_integer(*millisecond, realm, origin)?;
    let microsecond = temporal_plain_date_time_integer(*microsecond, realm, origin)?;
    let nanosecond = temporal_plain_date_time_integer(*nanosecond, realm, origin)?;
    let (Ok(year), Ok(month), Ok(day), Ok(hour), Ok(minute), Ok(second)) = (
        i32::try_from(year),
        u8::try_from(month),
        u8::try_from(day),
        u8::try_from(hour),
        u8::try_from(minute),
        u8::try_from(second),
    ) else {
        return temporal_range_error(
            realm,
            origin,
            "Temporal.PlainDateTime fields are outside the supported range",
        );
    };
    let (Ok(millisecond), Ok(microsecond), Ok(nanosecond)) = (
        u16::try_from(millisecond),
        u16::try_from(microsecond),
        u16::try_from(nanosecond),
    ) else {
        return temporal_range_error(
            realm,
            origin,
            "Temporal.PlainDateTime fields are outside the supported range",
        );
    };
    let date_time = match PlainDateTime::try_new(
        year,
        month,
        day,
        hour,
        minute,
        second,
        millisecond,
        microsecond,
        nanosecond,
        calendar,
    ) {
        Ok(date_time) => date_time,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    begin_temporal_plain_date_time_wrapper(
        runtime,
        realm,
        state.new_target,
        date_time,
        return_to,
        origin.clone(),
        execution_budget,
    )
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "ECMA-262 ToIntegerWithTruncation is defined on binary64 values before their bounded Temporal fields are checked"
)]
fn temporal_plain_date_time_integer(
    value: JsNumber,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<i64, NativeFailure> {
    let value = value.as_f64();
    if !value.is_finite() {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "Temporal.PlainDateTime fields must be finite Numbers",
        )?));
    }
    let value = value.trunc();
    if value < i64::MIN as f64 || value > i64::MAX as f64 {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "Temporal.PlainDateTime fields are outside the supported range",
        )?));
    }
    Ok(value as i64)
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "newTarget prototype lookup is a resumable native operation"
)]
fn begin_temporal_plain_date_time_wrapper(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: FunctionId,
    date_time: PlainDateTime,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    begin_intrinsic_get(
        runtime,
        realm,
        HeapReference::Function(new_target),
        StoredValue::Function(new_target),
        &prototype_key,
        IntrinsicGetContinuation::TemporalPlainDateTimeConstructor {
            new_target,
            date_time,
        },
        return_to,
        Some(origin),
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_date_time_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    date_time: PlainDateTime,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        _ => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_temporal_plain_date_time_prototype(realm)?)
        }
    };
    let object = runtime.allocate_temporal_plain_date_time(prototype, date_time)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "Temporal.PlainDateTime static conversion retains the native call context"
)]
pub(in crate::vm) fn begin_temporal_plain_date_time_static(
    runtime: &mut Runtime,
    method: TemporalPlainDateTimeStaticMethod,
    realm: RealmId,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        TemporalPlainDateTimeStaticMethod::From => {
            let value = arguments.take_first_or_undefined();
            let options = arguments.take_first_or_undefined();
            begin_temporal_plain_date_time_like(
                runtime,
                value,
                TemporalPlainDateTimeLikeTarget::From { options },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateTimeStaticMethod::Compare => {
            let first = arguments.take_first_or_undefined();
            let second = arguments.take_first_or_undefined();
            begin_temporal_plain_date_time_like(
                runtime,
                first,
                TemporalPlainDateTimeLikeTarget::CompareFirst { second },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "all accepted Temporal.PlainDateTime inputs share a resumable conversion boundary"
)]
fn begin_temporal_plain_date_time_like(
    runtime: &mut Runtime,
    value: StoredValue,
    target: TemporalPlainDateTimeLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(object) = value {
        if let Some(date_time) = runtime.temporal_plain_date_time(object)? {
            return continue_temporal_plain_date_time_like(
                runtime,
                date_time,
                target,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        if let Some(date) = runtime.temporal_plain_date(object)? {
            let date_time = match PlainDateTime::from_date_and_time(date, PlainTime::default()) {
                Ok(date_time) => date_time,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, &origin, error,
                    )?));
                }
            };
            return continue_temporal_plain_date_time_like(
                runtime,
                date_time,
                target,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
    }
    if let StoredValue::String(value) = value {
        let date_time = temporal_plain_date_time_from_string(realm, &value, &origin)?;
        return continue_temporal_plain_date_time_like(
            runtime,
            date_time,
            target,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if value.heap_reference().is_some() {
        return advance_temporal_plain_date_time_property_bag(
            runtime,
            TemporalPlainDateTimeBagContinuation {
                base: value,
                calendar: None,
                day: None,
                hour: None,
                microsecond: None,
                millisecond: None,
                minute: None,
                month: None,
                month_code: None,
                nanosecond: None,
                second: None,
                year: None,
                next: 0,
                stage: TemporalPlainDateTimeBagStage::ReadField,
                target,
                realm,
                origin,
            },
            None,
            return_to,
            execution_budget,
        );
    }
    temporal_type_error(
        realm,
        &origin,
        "Temporal.PlainDateTime requires a PlainDateTime, PlainDate, ISO string, or property bag",
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the native method retains its fields and options across observable conversion"
)]
fn begin_temporal_plain_date_time_with(
    runtime: &mut Runtime,
    receiver: PlainDateTime,
    fields: StoredValue,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if fields.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.PlainDateTime.with requires a property bag",
        );
    }
    advance_temporal_plain_date_time_property_bag(
        runtime,
        TemporalPlainDateTimeBagContinuation {
            base: fields,
            calendar: None,
            day: None,
            hour: None,
            microsecond: None,
            millisecond: None,
            minute: None,
            month: None,
            month_code: None,
            nanosecond: None,
            second: None,
            year: None,
            next: 0,
            stage: TemporalPlainDateTimeBagStage::ReadField,
            target: TemporalPlainDateTimeLikeTarget::With { receiver, options },
            realm,
            origin,
        },
        None,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the target selects either overflow options or the second compare conversion"
)]
fn continue_temporal_plain_date_time_like(
    runtime: &mut Runtime,
    date_time: PlainDateTime,
    target: TemporalPlainDateTimeLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match target {
        TemporalPlainDateTimeLikeTarget::From { options } => {
            begin_temporal_plain_date_from_options(
                runtime,
                TemporalPlainDateOverflowTarget::FromDateTime(date_time),
                options,
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateTimeLikeTarget::CompareFirst { second } => {
            begin_temporal_plain_date_time_like(
                runtime,
                second,
                TemporalPlainDateTimeLikeTarget::CompareSecond { first: date_time },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateTimeLikeTarget::CompareSecond { first } => {
            let result = match first.compare_iso(&date_time) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            Ok(NativeDispatch::Immediate(StoredValue::Number(
                JsNumber::from_i32(result),
            )))
        }
        TemporalPlainDateTimeLikeTarget::Equals { receiver } => Ok(NativeDispatch::Immediate(
            StoredValue::Boolean(receiver == date_time),
        )),
        TemporalPlainDateTimeLikeTarget::Difference {
            receiver,
            options,
            since,
        } => begin_temporal_plain_date_time_difference(
            runtime,
            receiver,
            date_time,
            options,
            since,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        TemporalPlainDateTimeLikeTarget::With { .. } => {
            unreachable!("Temporal.PlainDateTime.with completes from its property-bag state")
        }
        TemporalPlainDateTimeLikeTarget::FromPlainDate { .. } => {
            unreachable!("Temporal.PlainDate.toPlainDateTime completes from its property-bag state")
        }
    }
}

fn temporal_plain_date_time_from_string(
    realm: RealmId,
    value: &JsString,
    origin: &JsStackFrame,
) -> Result<PlainDateTime, NativeFailure> {
    let source = value.to_utf8_lossy()?;
    match PlainDateTime::from_utf8(source.as_bytes()) {
        Ok(date_time) => Ok(date_time),
        Err(error) => Err(NativeFailure::Abrupt(temporal_exception_from_error(
            realm, origin, error,
        )?)),
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the explicit state machine preserves property access and conversion order"
)]
pub(in crate::vm) fn advance_temporal_plain_date_time_property_bag(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeBagContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalPlainDateTimeBagStage::ReadField => {
                let field_names = temporal_plain_date_time_property_bag_fields(&state.target);
                if state.next == field_names.len() {
                    if matches!(&state.target, TemporalPlainDateTimeLikeTarget::With { .. }) {
                        let fields = temporal_plain_date_time_with_fields_from_bag(&state)?;
                        let TemporalPlainDateTimeLikeTarget::With { receiver, options } =
                            state.target
                        else {
                            unreachable!("checked Temporal.PlainDateTime.with target")
                        };
                        return begin_temporal_plain_date_from_options(
                            runtime,
                            TemporalPlainDateOverflowTarget::DateTimeWith { receiver, fields },
                            options,
                            state.realm,
                            return_to,
                            state.origin,
                            execution_budget,
                        );
                    }
                    if let TemporalPlainDateTimeLikeTarget::FromPlainDate { receiver } =
                        &state.target
                    {
                        let time = temporal_plain_time_from_bag(&state)?;
                        return finish_temporal_plain_date_to_plain_date_time(
                            runtime,
                            receiver,
                            Some(time),
                            state.realm,
                            &state.origin,
                        );
                    }
                    let partial = temporal_plain_date_time_partial_from_bag(&state)?;
                    return match state.target {
                        TemporalPlainDateTimeLikeTarget::From { options } => {
                            begin_temporal_plain_date_from_options(
                                runtime,
                                TemporalPlainDateOverflowTarget::FromPartialDateTime(partial),
                                options,
                                state.realm,
                                return_to,
                                state.origin,
                                execution_budget,
                            )
                        }
                        target => {
                            let date_time = match PlainDateTime::from_partial(
                                partial,
                                Some(Overflow::Constrain),
                            ) {
                                Ok(date_time) => date_time,
                                Err(error) => {
                                    return Err(NativeFailure::Abrupt(
                                        temporal_exception_from_error(
                                            state.realm,
                                            &state.origin,
                                            error,
                                        )?,
                                    ));
                                }
                            };
                            continue_temporal_plain_date_time_like(
                                runtime,
                                date_time,
                                target,
                                state.realm,
                                return_to,
                                state.origin,
                                execution_budget,
                            )
                        }
                    };
                }
                charge_heap_property_lookup(runtime, &state.base, execution_budget)?;
                let name = JsString::from_utf8(field_names[state.next])?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalPlainDateTimeBagStage::AwaitField;
                let dispatch = begin_value_get(
                    runtime,
                    &state.base,
                    key,
                    Some(&name),
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                match continue_get_state_after(
                    dispatch,
                    state,
                    temporal_plain_date_time_bag_continuation,
                    "Temporal.PlainDateTime property bag Get produced a structured result",
                )? {
                    GetContinuationDispatch::Ready {
                        state: resumed,
                        value,
                    } => {
                        state = resumed;
                        completion = Some(value);
                    }
                    GetContinuationDispatch::Suspended(dispatch) => return Ok(dispatch),
                }
            }
            TemporalPlainDateTimeBagStage::AwaitField => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainDateTime property bag Get resumed without a value",
                })?;
                let field = temporal_plain_date_time_property_bag_fields(&state.target)[state.next];
                if matches!(&state.target, TemporalPlainDateTimeLikeTarget::With { .. })
                    && matches!(field, "calendar" | "timeZone")
                {
                    if !matches!(value, StoredValue::Undefined) {
                        return temporal_type_error(
                            state.realm,
                            &state.origin,
                            "Temporal.PlainDateTime.with cannot override calendar or timeZone",
                        );
                    }
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainDateTimeBagStage::ReadField;
                    continue;
                }
                if matches!(value, StoredValue::Undefined) {
                    if field == "calendar" {
                        state.calendar = Some(Calendar::default());
                    }
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainDateTimeBagStage::ReadField;
                    continue;
                }
                if field == "calendar" {
                    let StoredValue::String(value) = value else {
                        return temporal_type_error(
                            state.realm,
                            &state.origin,
                            "Temporal.PlainDateTime calendar must be a string",
                        );
                    };
                    let calendar = match Calendar::from_str(&value.to_utf8_lossy()?) {
                        Ok(calendar) => calendar,
                        Err(error) => {
                            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                                state.realm,
                                &state.origin,
                                error,
                            )?));
                        }
                    };
                    state.calendar = Some(calendar);
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainDateTimeBagStage::ReadField;
                    continue;
                }
                state.stage = TemporalPlainDateTimeBagStage::AwaitConversion;
                let hint = match field {
                    "monthCode" => OperatorPrimitiveHint::String,
                    "day" | "hour" | "microsecond" | "millisecond" | "minute" | "month"
                    | "nanosecond" | "second" | "year" => OperatorPrimitiveHint::Number,
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "unknown Temporal.PlainDateTime property bag field",
                        }
                        .into());
                    }
                };
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    hint,
                    OperatorPrimitiveTarget::TemporalPlainDateTimeBag(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalPlainDateTimeBagStage::AwaitConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainDateTime property bag conversion resumed without a value",
                })?;
                match temporal_plain_date_time_property_bag_fields(&state.target)[state.next] {
                    "monthCode" => {
                        let StoredValue::String(value) = value else {
                            return temporal_type_error(
                                state.realm,
                                &state.origin,
                                "Temporal.PlainDateTime monthCode must be a string",
                            );
                        };
                        state.month_code = Some(match &state.target {
                            TemporalPlainDateTimeLikeTarget::With { .. } => {
                                TemporalPlainDateTimeMonthCode::Raw(value)
                            }
                            TemporalPlainDateTimeLikeTarget::From { .. }
                            | TemporalPlainDateTimeLikeTarget::CompareFirst { .. }
                            | TemporalPlainDateTimeLikeTarget::CompareSecond { .. }
                            | TemporalPlainDateTimeLikeTarget::Equals { .. }
                            | TemporalPlainDateTimeLikeTarget::Difference { .. }
                            | TemporalPlainDateTimeLikeTarget::FromPlainDate { .. } => {
                                let month_code = match MonthCode::from_str(&value.to_utf8_lossy()?)
                                {
                                    Ok(month_code) => month_code,
                                    Err(error) => {
                                        return Err(NativeFailure::Abrupt(
                                            temporal_exception_from_error(
                                                state.realm,
                                                &state.origin,
                                                error,
                                            )?,
                                        ));
                                    }
                                };
                                TemporalPlainDateTimeMonthCode::Parsed(month_code)
                            }
                        });
                    }
                    "day" => {
                        state.day = Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    "hour" => {
                        state.hour = Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    "microsecond" => {
                        state.microsecond =
                            Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    "millisecond" => {
                        state.millisecond =
                            Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    "minute" => {
                        state.minute = Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    "month" => {
                        state.month = Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    "nanosecond" => {
                        state.nanosecond =
                            Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    "second" => {
                        state.second = Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    "year" => {
                        state.year = Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "unknown Temporal.PlainDateTime property bag field",
                        }
                        .into());
                    }
                }
                state.next = state.next.saturating_add(1);
                state.stage = TemporalPlainDateTimeBagStage::ReadField;
            }
        }
    }
}

fn temporal_plain_date_time_bag_continuation(
    state: TemporalPlainDateTimeBagContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainDateTimeBag(Box::new(state))
}

fn temporal_plain_date_time_partial_from_bag(
    state: &TemporalPlainDateTimeBagContinuation,
) -> Result<PartialDateTime, NativeFailure> {
    let year = temporal_plain_date_time_required_field(state.year, state.realm, &state.origin)?;
    let day = temporal_plain_date_time_required_field(state.day, state.realm, &state.origin)?;
    let year = temporal_plain_date_time_i32(year, state.realm, &state.origin)?;
    let day = temporal_plain_date_time_u8(day, state.realm, &state.origin)?;
    let month = temporal_plain_date_time_optional_u8(state.month, state.realm, &state.origin)?;
    let hour = temporal_plain_date_time_optional_u8(state.hour, state.realm, &state.origin)?;
    let minute = temporal_plain_date_time_optional_u8(state.minute, state.realm, &state.origin)?;
    let second = temporal_plain_date_time_optional_u8(state.second, state.realm, &state.origin)?;
    let millisecond =
        temporal_plain_date_time_optional_u16(state.millisecond, state.realm, &state.origin)?;
    let microsecond =
        temporal_plain_date_time_optional_u16(state.microsecond, state.realm, &state.origin)?;
    let nanosecond =
        temporal_plain_date_time_optional_u16(state.nanosecond, state.realm, &state.origin)?;
    let month_code =
        temporal_plain_date_time_parsed_month_code_from_bag(state.month_code.as_ref())?;
    let calendar_fields = CalendarFields::new()
        .with_year(year)
        .with_optional_month(month)
        .with_optional_month_code(month_code)
        .with_day(day);
    let time = PartialTime::new()
        .with_hour(hour)
        .with_minute(minute)
        .with_second(second)
        .with_millisecond(millisecond)
        .with_microsecond(microsecond)
        .with_nanosecond(nanosecond);
    Ok(PartialDateTime {
        fields: DateTimeFields::new()
            .with_partial_date(calendar_fields)
            .with_partial_time(time),
        calendar: state.calendar.clone().unwrap_or_default(),
    })
}

fn temporal_plain_date_time_parsed_month_code_from_bag(
    value: Option<&TemporalPlainDateTimeMonthCode>,
) -> Result<Option<MonthCode>, NativeFailure> {
    match value {
        None => Ok(None),
        Some(TemporalPlainDateTimeMonthCode::Parsed(value)) => Ok(Some(*value)),
        Some(TemporalPlainDateTimeMonthCode::Raw(_)) => Err(EngineFault::RuntimeInvariant {
            message: "Temporal.PlainDateTime property-bag target retained a raw monthCode",
        }
        .into()),
    }
}

fn temporal_plain_date_time_raw_month_code_from_bag(
    value: Option<&TemporalPlainDateTimeMonthCode>,
) -> Result<Option<JsString>, NativeFailure> {
    match value {
        None => Ok(None),
        Some(TemporalPlainDateTimeMonthCode::Raw(value)) => Ok(Some(value.clone())),
        Some(TemporalPlainDateTimeMonthCode::Parsed(_)) => Err(EngineFault::RuntimeInvariant {
            message: "Temporal.PlainDateTime.with retained a parsed monthCode",
        }
        .into()),
    }
}

fn temporal_plain_date_time_optional_month_code(
    value: Option<&JsString>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Option<MonthCode>, NativeFailure> {
    let Some(value) = value else {
        return Ok(None);
    };
    match MonthCode::from_str(&value.to_utf8_lossy()?) {
        Ok(month_code) => Ok(Some(month_code)),
        Err(error) => Err(NativeFailure::Abrupt(temporal_exception_from_error(
            realm, origin, error,
        )?)),
    }
}

fn temporal_plain_date_time_with_fields_from_bag(
    state: &TemporalPlainDateTimeBagContinuation,
) -> Result<TemporalPlainDateTimeWithFields, NativeFailure> {
    let month = temporal_plain_date_time_optional_integer(state.month, state.realm, &state.origin)?;
    let day = temporal_plain_date_time_optional_integer(state.day, state.realm, &state.origin)?;
    let hour = temporal_plain_date_time_optional_integer(state.hour, state.realm, &state.origin)?;
    let minute =
        temporal_plain_date_time_optional_integer(state.minute, state.realm, &state.origin)?;
    let second =
        temporal_plain_date_time_optional_integer(state.second, state.realm, &state.origin)?;
    let millisecond =
        temporal_plain_date_time_optional_integer(state.millisecond, state.realm, &state.origin)?;
    let microsecond =
        temporal_plain_date_time_optional_integer(state.microsecond, state.realm, &state.origin)?;
    let nanosecond =
        temporal_plain_date_time_optional_integer(state.nanosecond, state.realm, &state.origin)?;
    for value in [
        month,
        day,
        hour,
        minute,
        second,
        millisecond,
        microsecond,
        nanosecond,
    ] {
        if value.is_some_and(|value| value < 0) {
            return Err(NativeFailure::Abrupt(temporal_pending_exception(
                state.realm,
                &state.origin,
                ExceptionKind::RangeError,
                "Temporal.PlainDateTime.with fields must be non-negative",
            )?));
        }
    }
    Ok(TemporalPlainDateTimeWithFields {
        year: temporal_plain_date_time_optional_integer(state.year, state.realm, &state.origin)?,
        month,
        month_code: temporal_plain_date_time_raw_month_code_from_bag(state.month_code.as_ref())?,
        day,
        hour,
        minute,
        second,
        millisecond,
        microsecond,
        nanosecond,
    })
}

fn temporal_plain_date_time_optional_integer(
    value: Option<JsNumber>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Option<i64>, NativeFailure> {
    value
        .map(|value| temporal_plain_date_time_integer(value, realm, origin))
        .transpose()
}

pub(in crate::vm) fn temporal_plain_date_time_with_fields(
    fields: &TemporalPlainDateTimeWithFields,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<DateTimeFields, NativeFailure> {
    let year = fields
        .year
        .map(|value| temporal_plain_date_time_i32(value, realm, origin))
        .transpose()?;
    let month = fields
        .month
        .map(|value| temporal_plain_date_time_u8(value, realm, origin))
        .transpose()?;
    let day = fields
        .day
        .map(|value| temporal_plain_date_time_u8(value, realm, origin))
        .transpose()?;
    let hour = fields
        .hour
        .map(|value| temporal_plain_date_time_u8(value, realm, origin))
        .transpose()?;
    let minute = fields
        .minute
        .map(|value| temporal_plain_date_time_u8(value, realm, origin))
        .transpose()?;
    let second = fields
        .second
        .map(|value| temporal_plain_date_time_u8(value, realm, origin))
        .transpose()?;
    let millisecond = fields
        .millisecond
        .map(|value| temporal_plain_date_time_u16(value, realm, origin))
        .transpose()?;
    let microsecond = fields
        .microsecond
        .map(|value| temporal_plain_date_time_u16(value, realm, origin))
        .transpose()?;
    let nanosecond = fields
        .nanosecond
        .map(|value| temporal_plain_date_time_u16(value, realm, origin))
        .transpose()?;
    let month_code =
        temporal_plain_date_time_optional_month_code(fields.month_code.as_ref(), realm, origin)?;
    let date = CalendarFields::new()
        .with_optional_year(year)
        .with_optional_month(month)
        .with_optional_month_code(month_code)
        .with_optional_day(day);
    let time = PartialTime::new()
        .with_hour(hour)
        .with_minute(minute)
        .with_second(second)
        .with_millisecond(millisecond)
        .with_microsecond(microsecond)
        .with_nanosecond(nanosecond);
    Ok(DateTimeFields::new()
        .with_partial_date(date)
        .with_partial_time(time))
}

pub(in crate::vm) fn temporal_plain_date_time_required_field(
    value: Option<JsNumber>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<i64, NativeFailure> {
    let Some(value) = value else {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Temporal.PlainDateTime property bag is missing a required field",
        )?));
    };
    temporal_plain_date_time_integer(value, realm, origin)
}

pub(in crate::vm) fn temporal_plain_date_time_optional_u8(
    value: Option<JsNumber>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Option<u8>, NativeFailure> {
    value
        .map(|value| {
            temporal_plain_date_time_integer(value, realm, origin)
                .and_then(|value| temporal_plain_date_time_u8(value, realm, origin))
        })
        .transpose()
}

pub(in crate::vm) fn temporal_plain_date_time_optional_u16(
    value: Option<JsNumber>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Option<u16>, NativeFailure> {
    value
        .map(|value| {
            temporal_plain_date_time_integer(value, realm, origin)
                .and_then(|value| temporal_plain_date_time_u16(value, realm, origin))
        })
        .transpose()
}

pub(in crate::vm) fn temporal_plain_date_time_i32(
    value: i64,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<i32, NativeFailure> {
    let Ok(value) = i32::try_from(value) else {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "Temporal.PlainDateTime field is outside the supported range",
        )?));
    };
    Ok(value)
}

pub(in crate::vm) fn temporal_plain_date_time_u8(
    value: i64,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<u8, NativeFailure> {
    let Ok(value) = u8::try_from(value) else {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "Temporal.PlainDateTime field is outside the supported range",
        )?));
    };
    Ok(value)
}

fn temporal_plain_date_time_u16(
    value: i64,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<u16, NativeFailure> {
    let Ok(value) = u16::try_from(value) else {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "Temporal.PlainDateTime field is outside the supported range",
        )?));
    };
    Ok(value)
}

fn finish_temporal_plain_date_time_with_calendar(
    runtime: &mut Runtime,
    date_time: &PlainDateTime,
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let calendar = match value {
        StoredValue::String(source) => match Calendar::from_str(&source.to_utf8_lossy()?) {
            Ok(calendar) => calendar,
            Err(error) => {
                return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                    realm, origin, error,
                )?));
            }
        },
        StoredValue::Object(object) => {
            if let Some(date) = runtime.temporal_plain_date(object)? {
                date.calendar().clone()
            } else if let Some(other) = runtime.temporal_plain_date_time(object)? {
                other.calendar().clone()
            } else {
                return temporal_type_error(
                    realm,
                    origin,
                    "Temporal.PlainDateTime.withCalendar requires a calendar identifier or Temporal object",
                );
            }
        }
        _ => {
            return temporal_type_error(
                realm,
                origin,
                "Temporal.PlainDateTime.withCalendar requires a calendar identifier or Temporal object",
            );
        }
    };
    allocate_temporal_plain_date_time_result(runtime, realm, date_time.with_calendar(calendar))
}

fn finish_temporal_plain_date_time_to_zoned_date_time(
    runtime: &mut Runtime,
    date_time: &PlainDateTime,
    time_zone: TimeZone,
    disambiguation: Disambiguation,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let date_time = match date_time.to_zoned_date_time(time_zone, disambiguation) {
        Ok(date_time) => date_time,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    allocate_temporal_zoned_date_time_result(runtime, realm, date_time)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the observable disambiguation Get retains the complete native call context"
)]
fn begin_temporal_plain_date_time_to_zoned_date_time(
    runtime: &mut Runtime,
    date_time: PlainDateTime,
    time_zone_like: StoredValue,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let time_zone =
        temporal_zoned_date_time_time_zone_from_value(runtime, time_zone_like, realm, &origin)?;
    if matches!(options, StoredValue::Undefined) {
        return finish_temporal_plain_date_time_to_zoned_date_time(
            runtime,
            &date_time,
            time_zone,
            Disambiguation::Compatible,
            realm,
            &origin,
        );
    }
    if options.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.PlainDateTime.prototype.toZonedDateTime options must be an object",
        );
    }
    charge_heap_property_lookup(runtime, &options, execution_budget)?;
    let name = JsString::from_utf8("disambiguation")?;
    let key = runtime.property_key_from_string(&name)?;
    let state = TemporalPlainDateTimeToZonedDateTimeContinuation {
        date_time,
        time_zone,
        options,
        stage: TemporalPlainDateTimeToZonedDateTimeStage::AwaitDisambiguation,
        realm,
        origin,
    };
    let dispatch = begin_value_get(
        runtime,
        &state.options,
        key,
        Some(&name),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    match continue_get_state_after(
        dispatch,
        state,
        temporal_plain_date_time_to_zoned_date_time_continuation,
        "Temporal.PlainDateTime toZonedDateTime disambiguation Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_plain_date_time_to_zoned_date_time(
                runtime,
                state,
                value,
                return_to,
                execution_budget,
            )
        }
        GetContinuationDispatch::Suspended(dispatch) => Ok(dispatch),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "disambiguation conversion resumes with the retained date-time and time zone"
)]
pub(in crate::vm) fn advance_temporal_plain_date_time_to_zoned_date_time(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeToZonedDateTimeContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalPlainDateTimeToZonedDateTimeStage::AwaitDisambiguation => {
            if matches!(value, StoredValue::Undefined) {
                return finish_temporal_plain_date_time_to_zoned_date_time(
                    runtime,
                    &state.date_time,
                    state.time_zone,
                    Disambiguation::Compatible,
                    state.realm,
                    &state.origin,
                );
            }
            state.stage = TemporalPlainDateTimeToZonedDateTimeStage::AwaitDisambiguationConversion;
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalPlainDateTimeToZonedDateTime(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateTimeToZonedDateTimeStage::AwaitDisambiguationConversion => {
            let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
            let Ok(disambiguation) = Disambiguation::from_str(&source.to_utf8_lossy()?) else {
                return temporal_range_error(
                    state.realm,
                    &state.origin,
                    "Temporal.PlainDateTime.prototype.toZonedDateTime disambiguation is invalid",
                );
            };
            finish_temporal_plain_date_time_to_zoned_date_time(
                runtime,
                &state.date_time,
                state.time_zone,
                disambiguation,
                state.realm,
                &state.origin,
            )
        }
    }
}

fn temporal_plain_date_time_to_zoned_date_time_continuation(
    state: TemporalPlainDateTimeToZonedDateTimeContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainDateTimeToZonedDateTime(Box::new(state))
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one exhaustive dispatcher preserves receiver validation and method-specific argument order"
)]
pub(in crate::vm) fn dispatch_temporal_plain_date_time_prototype(
    runtime: &mut Runtime,
    method: TemporalPlainDateTimePrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let date_time = require_temporal_plain_date_time(runtime, receiver, realm, origin)?;
    let number = |value| NativeDispatch::Immediate(StoredValue::Number(JsNumber::from_i64(value)));
    match method {
        TemporalPlainDateTimePrototypeMethod::CalendarId => Ok(NativeDispatch::Immediate(
            StoredValue::String(JsString::from_utf8(date_time.calendar().identifier())?),
        )),
        TemporalPlainDateTimePrototypeMethod::Year => Ok(number(i64::from(date_time.year()))),
        TemporalPlainDateTimePrototypeMethod::Month => Ok(number(i64::from(date_time.month()))),
        TemporalPlainDateTimePrototypeMethod::MonthCode => Ok(NativeDispatch::Immediate(
            StoredValue::String(JsString::from_utf8(date_time.month_code().as_str())?),
        )),
        TemporalPlainDateTimePrototypeMethod::Day => Ok(number(i64::from(date_time.day()))),
        TemporalPlainDateTimePrototypeMethod::Hour => Ok(number(i64::from(date_time.hour()))),
        TemporalPlainDateTimePrototypeMethod::Minute => Ok(number(i64::from(date_time.minute()))),
        TemporalPlainDateTimePrototypeMethod::Second => Ok(number(i64::from(date_time.second()))),
        TemporalPlainDateTimePrototypeMethod::Millisecond => {
            Ok(number(i64::from(date_time.millisecond())))
        }
        TemporalPlainDateTimePrototypeMethod::Microsecond => {
            Ok(number(i64::from(date_time.microsecond())))
        }
        TemporalPlainDateTimePrototypeMethod::Nanosecond => {
            Ok(number(i64::from(date_time.nanosecond())))
        }
        TemporalPlainDateTimePrototypeMethod::DayOfWeek => {
            Ok(number(i64::from(date_time.day_of_week())))
        }
        TemporalPlainDateTimePrototypeMethod::DayOfYear => {
            Ok(number(i64::from(date_time.day_of_year())))
        }
        TemporalPlainDateTimePrototypeMethod::WeekOfYear => Ok(match date_time.week_of_year() {
            Some(value) => number(i64::from(value)),
            None => NativeDispatch::Immediate(StoredValue::Undefined),
        }),
        TemporalPlainDateTimePrototypeMethod::YearOfWeek => Ok(match date_time.year_of_week() {
            Some(value) => number(i64::from(value)),
            None => NativeDispatch::Immediate(StoredValue::Undefined),
        }),
        TemporalPlainDateTimePrototypeMethod::DaysInWeek => {
            Ok(number(i64::from(date_time.days_in_week())))
        }
        TemporalPlainDateTimePrototypeMethod::DaysInMonth => {
            Ok(number(i64::from(date_time.days_in_month())))
        }
        TemporalPlainDateTimePrototypeMethod::DaysInYear => {
            Ok(number(i64::from(date_time.days_in_year())))
        }
        TemporalPlainDateTimePrototypeMethod::MonthsInYear => {
            Ok(number(i64::from(date_time.months_in_year())))
        }
        TemporalPlainDateTimePrototypeMethod::InLeapYear => Ok(NativeDispatch::Immediate(
            StoredValue::Boolean(date_time.in_leap_year()),
        )),
        TemporalPlainDateTimePrototypeMethod::Era => Ok(match date_time.era() {
            Some(value) => {
                NativeDispatch::Immediate(StoredValue::String(JsString::from_utf8(value.as_str())?))
            }
            None => NativeDispatch::Immediate(StoredValue::Undefined),
        }),
        TemporalPlainDateTimePrototypeMethod::EraYear => Ok(match date_time.era_year() {
            Some(value) => number(i64::from(value)),
            None => NativeDispatch::Immediate(StoredValue::Undefined),
        }),
        TemporalPlainDateTimePrototypeMethod::With
        | TemporalPlainDateTimePrototypeMethod::Add
        | TemporalPlainDateTimePrototypeMethod::Subtract => begin_temporal_plain_date_time_mutator(
            runtime,
            date_time,
            method,
            arguments,
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainDateTimePrototypeMethod::Round => begin_temporal_plain_date_time_round(
            runtime,
            date_time,
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainDateTimePrototypeMethod::Equals => begin_temporal_plain_date_time_like(
            runtime,
            arguments.take_first_or_undefined(),
            TemporalPlainDateTimeLikeTarget::Equals {
                receiver: date_time,
            },
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainDateTimePrototypeMethod::ToZonedDateTime => {
            begin_temporal_plain_date_time_to_zoned_date_time(
                runtime,
                date_time,
                arguments.take_first_or_undefined(),
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalPlainDateTimePrototypeMethod::ToPlainDate => {
            allocate_temporal_plain_date_result(runtime, realm, date_time.to_plain_date())
        }
        TemporalPlainDateTimePrototypeMethod::ToPlainTime => {
            allocate_temporal_plain_time_result(runtime, realm, PlainTime::from(date_time))
        }
        TemporalPlainDateTimePrototypeMethod::WithCalendar => {
            let calendar = arguments.take_first_or_undefined();
            finish_temporal_plain_date_time_with_calendar(
                runtime, &date_time, calendar, realm, origin,
            )
        }
        TemporalPlainDateTimePrototypeMethod::Until
        | TemporalPlainDateTimePrototypeMethod::Since => {
            let other = arguments.take_first_or_undefined();
            let options = arguments.take_first_or_undefined();
            begin_temporal_plain_date_time_like(
                runtime,
                other,
                TemporalPlainDateTimeLikeTarget::Difference {
                    receiver: date_time,
                    options,
                    since: matches!(method, TemporalPlainDateTimePrototypeMethod::Since),
                },
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalPlainDateTimePrototypeMethod::ToString => begin_temporal_plain_date_time_to_string(
            runtime,
            date_time,
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainDateTimePrototypeMethod::ToJson
        | TemporalPlainDateTimePrototypeMethod::ToLocaleString => {
            render_temporal_plain_date_time(&date_time, realm, origin)
        }
        TemporalPlainDateTimePrototypeMethod::ValueOf => temporal_type_error(
            realm,
            origin,
            "Temporal.PlainDateTime cannot be converted to a primitive value",
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the shared native mutator setup carries the resumed call context explicitly"
)]
fn begin_temporal_plain_date_time_mutator(
    runtime: &mut Runtime,
    receiver: PlainDateTime,
    method: TemporalPlainDateTimePrototypeMethod,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        TemporalPlainDateTimePrototypeMethod::With => begin_temporal_plain_date_time_with(
            runtime,
            receiver,
            arguments.take_first_or_undefined(),
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        TemporalPlainDateTimePrototypeMethod::Add
        | TemporalPlainDateTimePrototypeMethod::Subtract => {
            let duration = arguments.take_first_or_undefined();
            let options = arguments.take_first_or_undefined();
            begin_temporal_duration_like(
                runtime,
                duration,
                TemporalDurationLikeTarget::PlainDateTimeArithmetic {
                    receiver,
                    subtract: matches!(method, TemporalPlainDateTimePrototypeMethod::Subtract),
                    options,
                },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        _ => Err(EngineFault::RuntimeInvariant {
            message: "Temporal.PlainDateTime mutator dispatch received a non-mutator",
        }
        .into()),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the ordered Temporal options reader retains native call context across user code"
)]
fn begin_temporal_plain_date_time_round(
    runtime: &mut Runtime,
    date_time: PlainDateTime,
    round_to: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match round_to {
        StoredValue::Undefined => temporal_type_error(
            realm,
            &origin,
            "Temporal.PlainDateTime.prototype.round requires an options object or smallest-unit string",
        ),
        StoredValue::String(source) => {
            let smallest_unit = temporal_round_unit(&source, realm, &origin)?;
            complete_temporal_plain_date_time_round(
                runtime,
                &date_time,
                RoundingIncrement::ONE,
                RoundingMode::HalfExpand,
                Some(smallest_unit),
                realm,
                &origin,
            )
        }
        options if options.heap_reference().is_some() => begin_temporal_plain_date_time_round_get(
            runtime,
            TemporalPlainDateTimeRoundContinuation {
                date_time,
                options,
                rounding_increment: RoundingIncrement::ONE,
                rounding_mode: RoundingMode::HalfExpand,
                stage: TemporalPlainDateTimeRoundStage::RoundingIncrement,
                realm,
                origin,
            },
            "roundingIncrement",
            TemporalPlainDateTimeRoundStage::RoundingIncrement,
            return_to,
            execution_budget,
        ),
        _ => temporal_type_error(
            realm,
            &origin,
            "Temporal.PlainDateTime.prototype.round requires an options object or smallest-unit string",
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "each observable Temporal options Get retains the complete native continuation state"
)]
fn begin_temporal_plain_date_time_round_get(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeRoundContinuation,
    name: &str,
    next_stage: TemporalPlainDateTimeRoundStage,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = next_stage;
    charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
    let name = JsString::from_utf8(name)?;
    let key = runtime.property_key_from_string(&name)?;
    let realm = state.realm;
    let origin = state.origin.clone();
    let dispatch = begin_value_get(
        runtime,
        &state.options,
        key,
        Some(&name),
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    match continue_get_state_after(
        dispatch,
        state,
        temporal_plain_date_time_round_continuation,
        "Temporal.PlainDateTime round option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_plain_date_time_round_options(
                runtime,
                state,
                value,
                return_to,
                execution_budget,
            )
        }
        GetContinuationDispatch::Suspended(dispatch) => Ok(dispatch),
    }
}

fn temporal_plain_date_time_round_continuation(
    state: TemporalPlainDateTimeRoundContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainDateTimeRoundOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    reason = "one ordered state table preserves observable options and coercions across suspension"
)]
pub(in crate::vm) fn advance_temporal_plain_date_time_round_options(
    runtime: &mut Runtime,
    state: TemporalPlainDateTimeRoundContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalPlainDateTimeRoundStage::RoundingIncrement => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_date_time_round_get(
                    runtime,
                    state,
                    "roundingMode",
                    TemporalPlainDateTimeRoundStage::RoundingMode,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::TemporalPlainDateTimeRoundRoundingIncrement(Box::new(
                    state,
                )),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateTimeRoundStage::RoundingMode => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_date_time_round_get(
                    runtime,
                    state,
                    "smallestUnit",
                    TemporalPlainDateTimeRoundStage::SmallestUnit,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalPlainDateTimeRoundRoundingMode(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateTimeRoundStage::SmallestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return complete_temporal_plain_date_time_round(
                    runtime,
                    &state.date_time,
                    state.rounding_increment,
                    state.rounding_mode,
                    None,
                    state.realm,
                    &state.origin,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalPlainDateTimeRoundSmallestUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

pub(in crate::vm) fn finish_temporal_plain_date_time_round_rounding_increment(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeRoundContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let value = operator_to_number(value, state.realm, &state.origin)?.as_f64();
    state.rounding_increment = match RoundingIncrement::try_from(value) {
        Ok(increment) => increment,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                state.realm,
                &state.origin,
                error,
            )?));
        }
    };
    begin_temporal_plain_date_time_round_get(
        runtime,
        state,
        "roundingMode",
        TemporalPlainDateTimeRoundStage::RoundingMode,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_date_time_round_rounding_mode(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeRoundContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.rounding_mode = temporal_rounding_mode(&source, state.realm, &state.origin)?;
    begin_temporal_plain_date_time_round_get(
        runtime,
        state,
        "smallestUnit",
        TemporalPlainDateTimeRoundStage::SmallestUnit,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_date_time_round_smallest_unit(
    runtime: &mut Runtime,
    state: &TemporalPlainDateTimeRoundContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let smallest_unit = temporal_round_unit(&source, state.realm, &state.origin)?;
    complete_temporal_plain_date_time_round(
        runtime,
        &state.date_time,
        state.rounding_increment,
        state.rounding_mode,
        Some(smallest_unit),
        state.realm,
        &state.origin,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the completed JavaScript option record is passed explicitly to the shared temporal kernel"
)]
fn complete_temporal_plain_date_time_round(
    runtime: &mut Runtime,
    date_time: &PlainDateTime,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    smallest_unit: Option<Unit>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let mut options = RoundingOptions::default();
    options.smallest_unit = smallest_unit;
    options.rounding_mode = Some(rounding_mode);
    options.increment = Some(rounding_increment);
    let rounded = match date_time.round(options) {
        Ok(rounded) => rounded,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    allocate_temporal_plain_date_time_result(runtime, realm, rounded)
}

fn render_temporal_plain_date_time(
    date_time: &PlainDateTime,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let rendered = match date_time
        .to_ixdtf_string(ToStringRoundingOptions::default(), DisplayCalendar::Auto)
    {
        Ok(rendered) => rendered,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::String(
        JsString::from_utf8(&rendered)?,
    )))
}

#[derive(Clone, Copy)]
enum TemporalPlainDateTimeRoundStage {
    RoundingIncrement,
    RoundingMode,
    SmallestUnit,
}

/// Resumable options state for `Temporal.PlainDateTime.prototype.round`.
/// Every Get and primitive conversion may invoke JavaScript.
pub(in crate::vm) struct TemporalPlainDateTimeRoundContinuation {
    date_time: PlainDateTime,
    options: StoredValue,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    stage: TemporalPlainDateTimeRoundStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainDateTimeRoundContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalPlainDateTimeToStringStage {
    CalendarName,
    FractionalSecondDigits,
    RoundingMode,
    SmallestUnit,
}

/// Resumable options state for `Temporal.PlainDateTime.prototype.toString`.
/// Every Get and primitive conversion may invoke JavaScript.
pub(in crate::vm) struct TemporalPlainDateTimeToStringContinuation {
    date_time: PlainDateTime,
    options: StoredValue,
    display_calendar: DisplayCalendar,
    precision: Precision,
    rounding_mode: RoundingMode,
    smallest_unit: Option<Unit>,
    stage: TemporalPlainDateTimeToStringStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainDateTimeToStringContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalPlainDateTimeDifferenceStage {
    LargestUnit,
    RoundingIncrement,
    RoundingMode,
    SmallestUnit,
}

/// Resumable options state for `Temporal.PlainDateTime.prototype.until` and
/// `since`. Each option Get and primitive conversion may invoke JavaScript.
pub(in crate::vm) struct TemporalPlainDateTimeDifferenceContinuation {
    receiver: PlainDateTime,
    other: PlainDateTime,
    options: StoredValue,
    largest_unit: Option<Unit>,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    since: bool,
    stage: TemporalPlainDateTimeDifferenceStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainDateTimeDifferenceContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the date-time operand is converted before the observable options object"
)]
fn begin_temporal_plain_date_time_difference(
    runtime: &mut Runtime,
    receiver: PlainDateTime,
    other: PlainDateTime,
    options: StoredValue,
    since: bool,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return complete_temporal_plain_date_time_difference(
            runtime,
            &receiver,
            &other,
            None,
            RoundingIncrement::ONE,
            RoundingMode::Trunc,
            None,
            since,
            realm,
            &origin,
        );
    }
    if options.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.PlainDateTime.prototype.until options must be an object",
        );
    }
    begin_temporal_plain_date_time_difference_get(
        runtime,
        TemporalPlainDateTimeDifferenceContinuation {
            receiver,
            other,
            options,
            largest_unit: None,
            rounding_increment: RoundingIncrement::ONE,
            rounding_mode: RoundingMode::Trunc,
            since,
            stage: TemporalPlainDateTimeDifferenceStage::LargestUnit,
            realm,
            origin,
        },
        "largestUnit",
        TemporalPlainDateTimeDifferenceStage::LargestUnit,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "each date-time difference option Get owns native call state across suspension"
)]
fn begin_temporal_plain_date_time_difference_get(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeDifferenceContinuation,
    name: &str,
    next_stage: TemporalPlainDateTimeDifferenceStage,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = next_stage;
    charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
    let name = JsString::from_utf8(name)?;
    let key = runtime.property_key_from_string(&name)?;
    let realm = state.realm;
    let origin = state.origin.clone();
    let dispatch = begin_value_get(
        runtime,
        &state.options,
        key,
        Some(&name),
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    match continue_get_state_after(
        dispatch,
        state,
        temporal_plain_date_time_difference_continuation,
        "Temporal.PlainDateTime difference option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_plain_date_time_difference_options(
                runtime,
                state,
                value,
                return_to,
                execution_budget,
            )
        }
        GetContinuationDispatch::Suspended(dispatch) => Ok(dispatch),
    }
}

fn temporal_plain_date_time_difference_continuation(
    state: TemporalPlainDateTimeDifferenceContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainDateTimeDifferenceOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one ordered state table preserves date-time difference option observation across suspension"
)]
pub(in crate::vm) fn advance_temporal_plain_date_time_difference_options(
    runtime: &mut Runtime,
    state: TemporalPlainDateTimeDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalPlainDateTimeDifferenceStage::LargestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_date_time_difference_get(
                    runtime,
                    state,
                    "roundingIncrement",
                    TemporalPlainDateTimeDifferenceStage::RoundingIncrement,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalPlainDateTimeDifferenceLargestUnit(Box::new(
                    state,
                )),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateTimeDifferenceStage::RoundingIncrement => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_date_time_difference_get(
                    runtime,
                    state,
                    "roundingMode",
                    TemporalPlainDateTimeDifferenceStage::RoundingMode,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::TemporalPlainDateTimeDifferenceRoundingIncrement(
                    Box::new(state),
                ),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateTimeDifferenceStage::RoundingMode => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_date_time_difference_get(
                    runtime,
                    state,
                    "smallestUnit",
                    TemporalPlainDateTimeDifferenceStage::SmallestUnit,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalPlainDateTimeDifferenceRoundingMode(Box::new(
                    state,
                )),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateTimeDifferenceStage::SmallestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return complete_temporal_plain_date_time_difference(
                    runtime,
                    &state.receiver,
                    &state.other,
                    state.largest_unit,
                    state.rounding_increment,
                    state.rounding_mode,
                    None,
                    state.since,
                    state.realm,
                    &state.origin,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalPlainDateTimeDifferenceSmallestUnit(Box::new(
                    state,
                )),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

pub(in crate::vm) fn finish_temporal_plain_date_time_difference_largest_unit(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.largest_unit = Some(temporal_round_unit(&source, state.realm, &state.origin)?);
    begin_temporal_plain_date_time_difference_get(
        runtime,
        state,
        "roundingIncrement",
        TemporalPlainDateTimeDifferenceStage::RoundingIncrement,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_date_time_difference_rounding_increment(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let value = operator_to_number(value, state.realm, &state.origin)?.as_f64();
    state.rounding_increment = match RoundingIncrement::try_from(value) {
        Ok(increment) => increment,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                state.realm,
                &state.origin,
                error,
            )?));
        }
    };
    begin_temporal_plain_date_time_difference_get(
        runtime,
        state,
        "roundingMode",
        TemporalPlainDateTimeDifferenceStage::RoundingMode,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_date_time_difference_rounding_mode(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.rounding_mode = temporal_rounding_mode(&source, state.realm, &state.origin)?;
    begin_temporal_plain_date_time_difference_get(
        runtime,
        state,
        "smallestUnit",
        TemporalPlainDateTimeDifferenceStage::SmallestUnit,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_date_time_difference_smallest_unit(
    runtime: &mut Runtime,
    state: &TemporalPlainDateTimeDifferenceContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let smallest_unit = temporal_round_unit(&source, state.realm, &state.origin)?;
    complete_temporal_plain_date_time_difference(
        runtime,
        &state.receiver,
        &state.other,
        state.largest_unit,
        state.rounding_increment,
        state.rounding_mode,
        Some(smallest_unit),
        state.since,
        state.realm,
        &state.origin,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the completed JavaScript difference settings are passed explicitly to the Temporal kernel"
)]
fn complete_temporal_plain_date_time_difference(
    runtime: &mut Runtime,
    receiver: &PlainDateTime,
    other: &PlainDateTime,
    largest_unit: Option<Unit>,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    smallest_unit: Option<Unit>,
    since: bool,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let mut settings = DifferenceSettings::default();
    settings.largest_unit = largest_unit;
    settings.smallest_unit = smallest_unit;
    settings.rounding_mode = Some(rounding_mode);
    settings.increment = Some(rounding_increment);
    let duration = if since {
        receiver.since(other, settings)
    } else {
        receiver.until(other, settings)
    };
    let duration = match duration {
        Ok(duration) => duration,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    allocate_temporal_duration_result(runtime, realm, duration)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the ordered PlainDateTime formatting reader owns its resumable native call context"
)]
fn begin_temporal_plain_date_time_to_string(
    runtime: &mut Runtime,
    date_time: PlainDateTime,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return complete_temporal_plain_date_time_to_string(
            &date_time,
            DisplayCalendar::Auto,
            Precision::Auto,
            RoundingMode::Trunc,
            None,
            realm,
            &origin,
        );
    }
    if options.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.PlainDateTime.prototype.toString options must be an object",
        );
    }
    begin_temporal_plain_date_time_to_string_get(
        runtime,
        TemporalPlainDateTimeToStringContinuation {
            date_time,
            options,
            display_calendar: DisplayCalendar::Auto,
            precision: Precision::Auto,
            rounding_mode: RoundingMode::Trunc,
            smallest_unit: None,
            stage: TemporalPlainDateTimeToStringStage::CalendarName,
            realm,
            origin,
        },
        "calendarName",
        TemporalPlainDateTimeToStringStage::CalendarName,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "each observable PlainDateTime formatting option Get retains native call state"
)]
fn begin_temporal_plain_date_time_to_string_get(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeToStringContinuation,
    name: &str,
    next_stage: TemporalPlainDateTimeToStringStage,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = next_stage;
    charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
    let name = JsString::from_utf8(name)?;
    let key = runtime.property_key_from_string(&name)?;
    let realm = state.realm;
    let origin = state.origin.clone();
    let dispatch = begin_value_get(
        runtime,
        &state.options,
        key,
        Some(&name),
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    match continue_get_state_after(
        dispatch,
        state,
        temporal_plain_date_time_to_string_continuation,
        "Temporal.PlainDateTime toString option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_plain_date_time_to_string_options(
                runtime,
                state,
                value,
                return_to,
                execution_budget,
            )
        }
        GetContinuationDispatch::Suspended(dispatch) => Ok(dispatch),
    }
}

fn temporal_plain_date_time_to_string_continuation(
    state: TemporalPlainDateTimeToStringContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainDateTimeToStringOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one ordered state table preserves PlainDateTime formatting option reads and coercions"
)]
pub(in crate::vm) fn advance_temporal_plain_date_time_to_string_options(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalPlainDateTimeToStringStage::CalendarName => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_date_time_to_string_get(
                    runtime,
                    state,
                    "fractionalSecondDigits",
                    TemporalPlainDateTimeToStringStage::FractionalSecondDigits,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalPlainDateTimeToStringCalendarName(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateTimeToStringStage::FractionalSecondDigits => match value {
            StoredValue::Undefined => begin_temporal_plain_date_time_to_string_get(
                runtime,
                state,
                "roundingMode",
                TemporalPlainDateTimeToStringStage::RoundingMode,
                return_to,
                execution_budget,
            ),
            StoredValue::Number(number) => {
                state.precision =
                    temporal_fractional_second_digits(number, state.realm, &state.origin)?;
                begin_temporal_plain_date_time_to_string_get(
                    runtime,
                    state,
                    "roundingMode",
                    TemporalPlainDateTimeToStringStage::RoundingMode,
                    return_to,
                    execution_budget,
                )
            }
            value => {
                let realm = state.realm;
                let origin = state.origin.clone();
                begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::TemporalPlainDateTimeToStringFractionalSecondDigits(
                        Box::new(state),
                    ),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                )
            }
        },
        TemporalPlainDateTimeToStringStage::RoundingMode => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_date_time_to_string_get(
                    runtime,
                    state,
                    "smallestUnit",
                    TemporalPlainDateTimeToStringStage::SmallestUnit,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalPlainDateTimeToStringRoundingMode(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateTimeToStringStage::SmallestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return complete_temporal_plain_date_time_to_string(
                    &state.date_time,
                    state.display_calendar,
                    state.precision,
                    state.rounding_mode,
                    state.smallest_unit,
                    state.realm,
                    &state.origin,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalPlainDateTimeToStringSmallestUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion continuation retains the native formatting call context"
)]
pub(in crate::vm) fn finish_temporal_plain_date_time_to_string_calendar_name(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.display_calendar = temporal_display_calendar(&source, state.realm, &state.origin)?;
    begin_temporal_plain_date_time_to_string_get(
        runtime,
        state,
        "fractionalSecondDigits",
        TemporalPlainDateTimeToStringStage::FractionalSecondDigits,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion continuation retains the native formatting call context"
)]
pub(in crate::vm) fn finish_temporal_plain_date_time_to_string_fractional_second_digits(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    if source.to_utf8_lossy()?.as_str() != "auto" {
        return temporal_range_error(
            state.realm,
            &state.origin,
            "fractionalSecondDigits must be a Number or the string auto",
        );
    }
    state.precision = Precision::Auto;
    begin_temporal_plain_date_time_to_string_get(
        runtime,
        state,
        "roundingMode",
        TemporalPlainDateTimeToStringStage::RoundingMode,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion continuation retains the native formatting call context"
)]
pub(in crate::vm) fn finish_temporal_plain_date_time_to_string_rounding_mode(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateTimeToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.rounding_mode = temporal_rounding_mode(&source, state.realm, &state.origin)?;
    begin_temporal_plain_date_time_to_string_get(
        runtime,
        state,
        "smallestUnit",
        TemporalPlainDateTimeToStringStage::SmallestUnit,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_date_time_to_string_smallest_unit(
    state: &TemporalPlainDateTimeToStringContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let smallest_unit = temporal_round_unit(&source, state.realm, &state.origin)?;
    complete_temporal_plain_date_time_to_string(
        &state.date_time,
        state.display_calendar,
        state.precision,
        state.rounding_mode,
        Some(smallest_unit),
        state.realm,
        &state.origin,
    )
}

fn complete_temporal_plain_date_time_to_string(
    date_time: &PlainDateTime,
    display_calendar: DisplayCalendar,
    precision: Precision,
    rounding_mode: RoundingMode,
    smallest_unit: Option<Unit>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    match smallest_unit {
        None
        | Some(
            Unit::Minute | Unit::Second | Unit::Millisecond | Unit::Microsecond | Unit::Nanosecond,
        ) => {}
        Some(Unit::Auto | Unit::Hour | Unit::Day | Unit::Week | Unit::Month | Unit::Year) => {
            return temporal_range_error(
                realm,
                origin,
                "smallestUnit must be minute, second, millisecond, microsecond, or nanosecond",
            );
        }
    }
    let options = ToStringRoundingOptions {
        precision,
        smallest_unit,
        rounding_mode: Some(rounding_mode),
    };
    let rendered = match date_time.to_ixdtf_string(options, display_calendar) {
        Ok(rendered) => rendered,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::String(
        JsString::from_utf8(&rendered)?,
    )))
}
