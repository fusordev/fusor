use super::super::conversions::operator_primitive_to_string;
#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use crate::runtime::{TemporalZonedDateTimePrototypeMethod, TemporalZonedDateTimeStaticMethod};
use core::str::FromStr;
use temporal_rs::{
    Calendar, MonthCode, PlainTime, TimeZone, UtcOffset, ZonedDateTime,
    fields::{CalendarFields, ZonedDateTimeFields},
    options::{
        Disambiguation, DisplayCalendar, DisplayOffset, DisplayTimeZone, OffsetDisambiguation,
        Overflow, RoundingMode, ToStringRoundingOptions, Unit,
    },
    parsers::Precision,
    partial::{PartialTime, PartialZonedDateTime},
    provider::TransitionDirection,
};

#[derive(Clone, Copy)]
pub(in crate::vm) enum TemporalZonedDateTimeConstructorStage {
    EpochNanoseconds,
    TimeZone,
    Calendar,
}

/// The `ZonedDateTime` constructor converts the epoch, time-zone identifier, and
/// calendar in source order. The state remains rooted across primitive calls.
pub(in crate::vm) struct TemporalZonedDateTimeConstructorContinuation {
    arguments: Vec<StoredValue>,
    stage: TemporalZonedDateTimeConstructorStage,
    epoch_nanoseconds: Option<i128>,
    time_zone: Option<TimeZone>,
    calendar: Option<Calendar>,
    new_target: FunctionId,
}

const TEMPORAL_ZONED_DATE_TIME_BAG_FIELDS: [&str; 13] = [
    "calendar",
    "day",
    "hour",
    "microsecond",
    "millisecond",
    "minute",
    "month",
    "monthCode",
    "nanosecond",
    "offset",
    "second",
    "timeZone",
    "year",
];

#[derive(Clone, Copy)]
enum TemporalZonedDateTimeBagStage {
    ReadField,
    AwaitField,
    AwaitConversion,
}

enum TemporalZonedDateTimeLikeTarget {
    From { options: StoredValue },
    CompareFirst { second: StoredValue },
    CompareSecond { first: ZonedDateTime },
    Equals { receiver: ZonedDateTime },
}

impl TemporalZonedDateTimeLikeTarget {
    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::From { options } | Self::CompareFirst { second: options } => {
                trace_stored_value_root(options, mark);
            }
            Self::CompareSecond { .. } | Self::Equals { .. } => {}
        }
    }
}

/// Prepared property-bag values for `ToTemporalZonedDateTime`.
///
/// The field-level lexical conversions happen before this record is created;
/// numeric range and calendar resolution stay deferred until all `from`
/// options have been observed.
struct TemporalZonedDateTimeBagFields {
    calendar: Calendar,
    day: Option<JsNumber>,
    hour: Option<JsNumber>,
    microsecond: Option<JsNumber>,
    millisecond: Option<JsNumber>,
    minute: Option<JsNumber>,
    month: Option<JsNumber>,
    month_code: Option<MonthCode>,
    nanosecond: Option<JsNumber>,
    offset: Option<UtcOffset>,
    second: Option<JsNumber>,
    time_zone: Option<TimeZone>,
    year: Option<JsNumber>,
}

/// Resumable `ToTemporalZonedDateTime` property-bag conversion.
///
/// Its field order is deliberately independent from `PlainDateTime`: `offset`
/// and `timeZone` are part of the observable `ZonedDateTime` preparation order.
pub(in crate::vm) struct TemporalZonedDateTimeBagContinuation {
    base: StoredValue,
    calendar: Option<Calendar>,
    day: Option<JsNumber>,
    hour: Option<JsNumber>,
    microsecond: Option<JsNumber>,
    millisecond: Option<JsNumber>,
    minute: Option<JsNumber>,
    month: Option<JsNumber>,
    month_code: Option<MonthCode>,
    nanosecond: Option<JsNumber>,
    offset: Option<UtcOffset>,
    second: Option<JsNumber>,
    time_zone: Option<TimeZone>,
    year: Option<JsNumber>,
    next: usize,
    stage: TemporalZonedDateTimeBagStage,
    target: TemporalZonedDateTimeLikeTarget,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalZonedDateTimeBagContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        2
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.base, mark);
        self.target.trace_roots(mark);
    }

    fn to_fields(&self) -> TemporalZonedDateTimeBagFields {
        TemporalZonedDateTimeBagFields {
            calendar: self.calendar.clone().unwrap_or_default(),
            day: self.day,
            hour: self.hour,
            microsecond: self.microsecond,
            millisecond: self.millisecond,
            minute: self.minute,
            month: self.month,
            month_code: self.month_code,
            nanosecond: self.nanosecond,
            offset: self.offset,
            second: self.second,
            time_zone: self.time_zone,
            year: self.year,
        }
    }
}

enum TemporalZonedDateTimeFromTarget {
    Existing(ZonedDateTime),
    String(JsString),
    PropertyBag(TemporalZonedDateTimeBagFields),
}

impl TemporalZonedDateTimeFromTarget {
    const fn retained_values(&self) -> u64 {
        match self {
            Self::Existing(_) | Self::PropertyBag(_) => 0,
            Self::String(_) => 1,
        }
    }

    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        if let Self::String(value) = self {
            trace_stored_value_root(&StoredValue::String(value.clone()), mark);
        }
    }
}

#[derive(Clone, Copy)]
enum TemporalZonedDateTimeOptionsStage {
    ReadDisambiguation,
    AwaitDisambiguation,
    AwaitDisambiguationConversion,
    ReadOffset,
    AwaitOffset,
    AwaitOffsetConversion,
    ReadOverflow,
    AwaitOverflow,
    AwaitOverflowConversion,
}

/// Ordered `Temporal.ZonedDateTime.from` option processing.
///
/// All three option conversions occur before the kernel validates property-bag
/// numeric fields or calendar-specific field combinations.
pub(in crate::vm) struct TemporalZonedDateTimeOptionsContinuation {
    target: TemporalZonedDateTimeFromTarget,
    options: StoredValue,
    disambiguation: Disambiguation,
    offset: OffsetDisambiguation,
    overflow: Overflow,
    stage: TemporalZonedDateTimeOptionsStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalZonedDateTimeOptionsContinuation {
    pub(in crate::vm) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(self.target.retained_values())
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        self.target.trace_roots(mark);
        trace_stored_value_root(&self.options, mark);
    }
}

impl TemporalZonedDateTimeConstructorContinuation {
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
    reason = "the ordered ZonedDateTime constructor conversion is resumable"
)]
pub(in crate::vm) fn begin_temporal_zoned_date_time_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = inputs.new_target else {
        return temporal_type_error(realm, &origin, "Temporal.ZonedDateTime is not callable");
    };
    let mut arguments = inputs.arguments.into_remaining_values();
    arguments.truncate(3);
    arguments
        .try_reserve(3_usize.saturating_sub(arguments.len()))
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 3_usize.saturating_sub(arguments.len()),
        })?;
    while arguments.len() < 3 {
        arguments.push(StoredValue::Undefined);
    }
    advance_temporal_zoned_date_time_constructor(
        runtime,
        TemporalZonedDateTimeConstructorContinuation {
            arguments,
            stage: TemporalZonedDateTimeConstructorStage::EpochNanoseconds,
            epoch_nanoseconds: None,
            time_zone: None,
            calendar: None,
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
    reason = "the constructor retains each converted component across primitive calls"
)]
pub(in crate::vm) fn advance_temporal_zoned_date_time_constructor(
    runtime: &mut Runtime,
    mut state: TemporalZonedDateTimeConstructorContinuation,
    mut completion: Option<StoredValue>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        match state.stage {
            TemporalZonedDateTimeConstructorStage::EpochNanoseconds => {
                if let Some(value) = completion.take() {
                    let bigint = to_bigint_from_primitive(&value, realm, origin)?;
                    let Some(epoch_nanoseconds) = bigint.to_i128() else {
                        return temporal_range_error(
                            realm,
                            origin,
                            "Temporal.ZonedDateTime epochNanoseconds is outside the supported range",
                        );
                    };
                    state.epoch_nanoseconds = Some(epoch_nanoseconds);
                    state.stage = TemporalZonedDateTimeConstructorStage::TimeZone;
                    continue;
                }
                let value = std::mem::replace(&mut state.arguments[0], StoredValue::Undefined);
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::TemporalZonedDateTimeConstructor(Box::new(state)),
                    realm,
                    return_to,
                    origin.clone(),
                    execution_budget,
                );
            }
            TemporalZonedDateTimeConstructorStage::TimeZone => {
                // ToTemporalTimeZoneIdentifier: no primitive conversion runs
                // here; only a ZonedDateTime slot or a string holding a bare
                // time-zone identifier is accepted.
                let value = std::mem::replace(&mut state.arguments[1], StoredValue::Undefined);
                state.time_zone = Some(match value {
                    StoredValue::String(source) => {
                        match TimeZone::try_from_identifier_str(&source.to_utf8_lossy()?) {
                            Ok(time_zone) => time_zone,
                            Err(error) => {
                                return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                                    realm, origin, error,
                                )?));
                            }
                        }
                    }
                    StoredValue::Object(object) => {
                        if let Some(date_time) = runtime.temporal_zoned_date_time(object)? {
                            *date_time.time_zone()
                        } else {
                            return temporal_type_error(
                                realm,
                                origin,
                                "Temporal.ZonedDateTime time zone must be a time zone identifier or Temporal.ZonedDateTime",
                            );
                        }
                    }
                    _ => {
                        return temporal_type_error(
                            realm,
                            origin,
                            "Temporal.ZonedDateTime time zone must be a time zone identifier or Temporal.ZonedDateTime",
                        );
                    }
                });
                state.stage = TemporalZonedDateTimeConstructorStage::Calendar;
                continue;
            }
            TemporalZonedDateTimeConstructorStage::Calendar => {
                // ToTemporalCalendarIdentifier: no primitive conversion runs
                // here; only undefined (ISO 8601 default) or a string holding
                // a bare calendar identifier is accepted.
                let value = std::mem::replace(&mut state.arguments[2], StoredValue::Undefined);
                state.calendar = Some(match value {
                    StoredValue::Undefined => Calendar::default(),
                    StoredValue::String(source) => {
                        match Calendar::try_from_utf8(source.to_utf8_lossy()?.as_bytes()) {
                            Ok(calendar) => calendar,
                            Err(error) => {
                                return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                                    realm, origin, error,
                                )?));
                            }
                        }
                    }
                    _ => {
                        return temporal_type_error(
                            realm,
                            origin,
                            "Temporal.ZonedDateTime calendar must be a calendar identifier",
                        );
                    }
                });
                return complete_temporal_zoned_date_time_constructor(
                    runtime,
                    &state,
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the completed constructor still needs the resumable newTarget prototype lookup"
)]
fn complete_temporal_zoned_date_time_constructor(
    runtime: &mut Runtime,
    state: &TemporalZonedDateTimeConstructorContinuation,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (Some(epoch_nanoseconds), Some(time_zone), Some(calendar)) = (
        state.epoch_nanoseconds,
        state.time_zone,
        state.calendar.clone(),
    ) else {
        return Err(EngineFault::RuntimeInvariant {
            message: "Temporal.ZonedDateTime constructor completed without all slots",
        }
        .into());
    };
    let date_time = match ZonedDateTime::try_new(epoch_nanoseconds, time_zone, calendar) {
        Ok(date_time) => date_time,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    begin_temporal_zoned_date_time_wrapper(
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
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "newTarget prototype lookup is a resumable native operation"
)]
fn begin_temporal_zoned_date_time_wrapper(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: FunctionId,
    date_time: ZonedDateTime,
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
        IntrinsicGetContinuation::TemporalZonedDateTimeConstructor {
            new_target,
            date_time,
        },
        return_to,
        Some(origin),
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_zoned_date_time_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    date_time: ZonedDateTime,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        _ => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_temporal_zoned_date_time_prototype(realm)?)
        }
    };
    let object = runtime.allocate_temporal_zoned_date_time(prototype, date_time)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "the initial ZonedDateTime from boundary owns the static-call context"
)]
pub(in crate::vm) fn begin_temporal_zoned_date_time_static(
    runtime: &mut Runtime,
    method: TemporalZonedDateTimeStaticMethod,
    realm: RealmId,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        TemporalZonedDateTimeStaticMethod::From => {
            let value = arguments.take_first_or_undefined();
            let options = arguments.take_first_or_undefined();
            begin_temporal_zoned_date_time_like(
                runtime,
                value,
                TemporalZonedDateTimeLikeTarget::From { options },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalZonedDateTimeStaticMethod::Compare => {
            let first = arguments.take_first_or_undefined();
            let second = arguments.take_first_or_undefined();
            begin_temporal_zoned_date_time_like(
                runtime,
                first,
                TemporalZonedDateTimeLikeTarget::CompareFirst { second },
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
    reason = "all ZonedDateTime input forms share one resumable conversion boundary"
)]
fn begin_temporal_zoned_date_time_like(
    runtime: &mut Runtime,
    value: StoredValue,
    target: TemporalZonedDateTimeLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(object) = value
        && let Some(date_time) = runtime.temporal_zoned_date_time(object)?
    {
        return continue_temporal_zoned_date_time_like(
            runtime,
            date_time,
            target,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if let StoredValue::String(source) = value {
        return match target {
            TemporalZonedDateTimeLikeTarget::From { options } => {
                // Validate a string before options are observable, but defer the
                // option-dependent interpretation until their ordered reads end.
                let _ = temporal_zoned_date_time_from_string(
                    realm,
                    &origin,
                    &source,
                    Disambiguation::Compatible,
                    OffsetDisambiguation::Use,
                )?;
                begin_temporal_zoned_date_time_from_options(
                    runtime,
                    TemporalZonedDateTimeFromTarget::String(source),
                    options,
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                )
            }
            target => {
                let date_time = temporal_zoned_date_time_from_string(
                    realm,
                    &origin,
                    &source,
                    Disambiguation::Compatible,
                    OffsetDisambiguation::Reject,
                )?;
                continue_temporal_zoned_date_time_like(
                    runtime,
                    date_time,
                    target,
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                )
            }
        };
    }
    if value.heap_reference().is_some() {
        return advance_temporal_zoned_date_time_property_bag(
            runtime,
            TemporalZonedDateTimeBagContinuation {
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
                offset: None,
                second: None,
                time_zone: None,
                year: None,
                next: 0,
                stage: TemporalZonedDateTimeBagStage::ReadField,
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
        "Temporal.ZonedDateTime requires a ZonedDateTime, ISO string, or property bag",
    )
}

fn temporal_zoned_date_time_from_string(
    realm: RealmId,
    origin: &JsStackFrame,
    value: &JsString,
    disambiguation: Disambiguation,
    offset: OffsetDisambiguation,
) -> Result<ZonedDateTime, NativeFailure> {
    match ZonedDateTime::from_utf8(value.to_utf8_lossy()?.as_bytes(), disambiguation, offset) {
        Ok(date_time) => Ok(date_time),
        Err(error) => Err(NativeFailure::Abrupt(temporal_exception_from_error(
            realm, origin, error,
        )?)),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the completed conversion target determines the next temporal operation"
)]
fn continue_temporal_zoned_date_time_like(
    runtime: &mut Runtime,
    date_time: ZonedDateTime,
    target: TemporalZonedDateTimeLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match target {
        TemporalZonedDateTimeLikeTarget::From { options } => {
            begin_temporal_zoned_date_time_from_options(
                runtime,
                TemporalZonedDateTimeFromTarget::Existing(date_time),
                options,
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalZonedDateTimeLikeTarget::CompareFirst { second } => {
            begin_temporal_zoned_date_time_like(
                runtime,
                second,
                TemporalZonedDateTimeLikeTarget::CompareSecond { first: date_time },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalZonedDateTimeLikeTarget::CompareSecond { first } => {
            let result = match first
                .epoch_nanoseconds()
                .as_i128()
                .cmp(&date_time.epoch_nanoseconds().as_i128())
            {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            Ok(NativeDispatch::Immediate(StoredValue::Number(
                JsNumber::from_i64(result),
            )))
        }
        TemporalZonedDateTimeLikeTarget::Equals { receiver } => {
            let equals = match receiver.equals(&date_time) {
                Ok(equals) => equals,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, &origin, error,
                    )?));
                }
            };
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(equals)))
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the three ordered options retain the native call context across observable work"
)]
fn begin_temporal_zoned_date_time_from_options(
    runtime: &mut Runtime,
    target: TemporalZonedDateTimeFromTarget,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return finish_temporal_zoned_date_time_from_options(
            runtime,
            target,
            Disambiguation::Compatible,
            OffsetDisambiguation::Reject,
            Overflow::Constrain,
            realm,
            &origin,
        );
    }
    if options.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.ZonedDateTime.from options must be an object",
        );
    }
    advance_temporal_zoned_date_time_from_options(
        runtime,
        TemporalZonedDateTimeOptionsContinuation {
            target,
            options,
            disambiguation: Disambiguation::Compatible,
            offset: OffsetDisambiguation::Reject,
            overflow: Overflow::Constrain,
            stage: TemporalZonedDateTimeOptionsStage::ReadDisambiguation,
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
    reason = "one helper preserves every ordered ZonedDateTime.from option Get"
)]
fn begin_temporal_zoned_date_time_option_get(
    runtime: &mut Runtime,
    mut state: TemporalZonedDateTimeOptionsContinuation,
    name: &str,
    await_stage: TemporalZonedDateTimeOptionsStage,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
    let name = JsString::from_utf8(name)?;
    let key = runtime.property_key_from_string(&name)?;
    state.stage = await_stage;
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
        temporal_zoned_date_time_options_continuation,
        "Temporal.ZonedDateTime.from option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_zoned_date_time_from_options(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        }
        GetContinuationDispatch::Suspended(dispatch) => Ok(dispatch),
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the explicit state machine preserves ZonedDateTime.from option order"
)]
pub(in crate::vm) fn advance_temporal_zoned_date_time_from_options(
    runtime: &mut Runtime,
    mut state: TemporalZonedDateTimeOptionsContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalZonedDateTimeOptionsStage::ReadDisambiguation => {
                return begin_temporal_zoned_date_time_option_get(
                    runtime,
                    state,
                    "disambiguation",
                    TemporalZonedDateTimeOptionsStage::AwaitDisambiguation,
                    return_to,
                    execution_budget,
                );
            }
            TemporalZonedDateTimeOptionsStage::AwaitDisambiguation => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.ZonedDateTime.from disambiguation Get resumed without a value",
                })?;
                if matches!(value, StoredValue::Undefined) {
                    state.stage = TemporalZonedDateTimeOptionsStage::ReadOffset;
                    continue;
                }
                state.stage = TemporalZonedDateTimeOptionsStage::AwaitDisambiguationConversion;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::TemporalZonedDateTimeOptions(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalZonedDateTimeOptionsStage::AwaitDisambiguationConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.ZonedDateTime.from disambiguation conversion resumed without a value",
                })?;
                let value = operator_primitive_to_string(value, state.realm, &state.origin)?;
                let Ok(disambiguation) = Disambiguation::from_str(&value.to_utf8_lossy()?) else {
                    return temporal_range_error(
                        state.realm,
                        &state.origin,
                        "Temporal.ZonedDateTime.from disambiguation is invalid",
                    );
                };
                state.disambiguation = disambiguation;
                state.stage = TemporalZonedDateTimeOptionsStage::ReadOffset;
            }
            TemporalZonedDateTimeOptionsStage::ReadOffset => {
                return begin_temporal_zoned_date_time_option_get(
                    runtime,
                    state,
                    "offset",
                    TemporalZonedDateTimeOptionsStage::AwaitOffset,
                    return_to,
                    execution_budget,
                );
            }
            TemporalZonedDateTimeOptionsStage::AwaitOffset => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.ZonedDateTime.from offset Get resumed without a value",
                })?;
                if matches!(value, StoredValue::Undefined) {
                    state.stage = TemporalZonedDateTimeOptionsStage::ReadOverflow;
                    continue;
                }
                state.stage = TemporalZonedDateTimeOptionsStage::AwaitOffsetConversion;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::TemporalZonedDateTimeOptions(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalZonedDateTimeOptionsStage::AwaitOffsetConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.ZonedDateTime.from offset conversion resumed without a value",
                })?;
                let value = operator_primitive_to_string(value, state.realm, &state.origin)?;
                let Ok(offset) = OffsetDisambiguation::from_str(&value.to_utf8_lossy()?) else {
                    return temporal_range_error(
                        state.realm,
                        &state.origin,
                        "Temporal.ZonedDateTime.from offset is invalid",
                    );
                };
                state.offset = offset;
                state.stage = TemporalZonedDateTimeOptionsStage::ReadOverflow;
            }
            TemporalZonedDateTimeOptionsStage::ReadOverflow => {
                return begin_temporal_zoned_date_time_option_get(
                    runtime,
                    state,
                    "overflow",
                    TemporalZonedDateTimeOptionsStage::AwaitOverflow,
                    return_to,
                    execution_budget,
                );
            }
            TemporalZonedDateTimeOptionsStage::AwaitOverflow => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.ZonedDateTime.from overflow Get resumed without a value",
                })?;
                if matches!(value, StoredValue::Undefined) {
                    return finish_temporal_zoned_date_time_from_options(
                        runtime,
                        state.target,
                        state.disambiguation,
                        state.offset,
                        state.overflow,
                        state.realm,
                        &state.origin,
                    );
                }
                state.stage = TemporalZonedDateTimeOptionsStage::AwaitOverflowConversion;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::TemporalZonedDateTimeOptions(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalZonedDateTimeOptionsStage::AwaitOverflowConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.ZonedDateTime.from overflow conversion resumed without a value",
                })?;
                let value = operator_primitive_to_string(value, state.realm, &state.origin)?;
                let Ok(overflow) = Overflow::from_str(&value.to_utf8_lossy()?) else {
                    return temporal_range_error(
                        state.realm,
                        &state.origin,
                        "Temporal.ZonedDateTime.from overflow must be constrain or reject",
                    );
                };
                return finish_temporal_zoned_date_time_from_options(
                    runtime,
                    state.target,
                    state.disambiguation,
                    state.offset,
                    overflow,
                    state.realm,
                    &state.origin,
                );
            }
        }
    }
}

fn temporal_zoned_date_time_options_continuation(
    state: TemporalZonedDateTimeOptionsContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalZonedDateTimeOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the fully observed from options are passed directly to temporal_rs"
)]
fn finish_temporal_zoned_date_time_from_options(
    runtime: &mut Runtime,
    target: TemporalZonedDateTimeFromTarget,
    disambiguation: Disambiguation,
    offset: OffsetDisambiguation,
    overflow: Overflow,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let date_time = match target {
        TemporalZonedDateTimeFromTarget::Existing(date_time) => date_time,
        TemporalZonedDateTimeFromTarget::String(source) => {
            temporal_zoned_date_time_from_string(realm, origin, &source, disambiguation, offset)?
        }
        TemporalZonedDateTimeFromTarget::PropertyBag(fields) => {
            temporal_zoned_date_time_from_bag_fields(
                fields,
                overflow,
                disambiguation,
                offset,
                realm,
                origin,
            )?
        }
    };
    allocate_temporal_zoned_date_time_result(runtime, realm, date_time)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the property-bag state machine preserves the exact observable preparation order"
)]
pub(in crate::vm) fn advance_temporal_zoned_date_time_property_bag(
    runtime: &mut Runtime,
    mut state: TemporalZonedDateTimeBagContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalZonedDateTimeBagStage::ReadField => {
                if state.next == TEMPORAL_ZONED_DATE_TIME_BAG_FIELDS.len() {
                    let fields = state.to_fields();
                    return match state.target {
                        TemporalZonedDateTimeLikeTarget::From { options } => {
                            begin_temporal_zoned_date_time_from_options(
                                runtime,
                                TemporalZonedDateTimeFromTarget::PropertyBag(fields),
                                options,
                                state.realm,
                                return_to,
                                state.origin,
                                execution_budget,
                            )
                        }
                        target => {
                            let date_time = temporal_zoned_date_time_from_bag_fields(
                                fields,
                                Overflow::Constrain,
                                Disambiguation::Compatible,
                                OffsetDisambiguation::Reject,
                                state.realm,
                                &state.origin,
                            )?;
                            continue_temporal_zoned_date_time_like(
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
                let name = JsString::from_utf8(TEMPORAL_ZONED_DATE_TIME_BAG_FIELDS[state.next])?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalZonedDateTimeBagStage::AwaitField;
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
                    temporal_zoned_date_time_bag_continuation,
                    "Temporal.ZonedDateTime property bag Get produced a structured result",
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
            TemporalZonedDateTimeBagStage::AwaitField => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.ZonedDateTime property bag Get resumed without a value",
                })?;
                let field = TEMPORAL_ZONED_DATE_TIME_BAG_FIELDS[state.next];
                if matches!(value, StoredValue::Undefined) {
                    if field == "calendar" {
                        state.calendar = Some(Calendar::default());
                    }
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalZonedDateTimeBagStage::ReadField;
                    continue;
                }
                if field == "calendar" {
                    state.calendar = Some(temporal_zoned_date_time_calendar_from_value(
                        runtime,
                        value,
                        state.realm,
                        &state.origin,
                    )?);
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalZonedDateTimeBagStage::ReadField;
                    continue;
                }
                if field == "timeZone" {
                    state.time_zone = Some(temporal_zoned_date_time_time_zone_from_value(
                        runtime,
                        value,
                        state.realm,
                        &state.origin,
                    )?);
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalZonedDateTimeBagStage::ReadField;
                    continue;
                }
                if field == "offset"
                    && !matches!(value, StoredValue::String(_))
                    && value.heap_reference().is_none()
                {
                    return temporal_type_error(
                        state.realm,
                        &state.origin,
                        "Temporal.ZonedDateTime offset must be a string or object",
                    );
                }
                state.stage = TemporalZonedDateTimeBagStage::AwaitConversion;
                let hint = match field {
                    "monthCode" | "offset" => OperatorPrimitiveHint::String,
                    "day" | "hour" | "microsecond" | "millisecond" | "minute" | "month"
                    | "nanosecond" | "second" | "year" => OperatorPrimitiveHint::Number,
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "unknown Temporal.ZonedDateTime property bag field",
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
                    OperatorPrimitiveTarget::TemporalZonedDateTimeBag(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalZonedDateTimeBagStage::AwaitConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.ZonedDateTime property bag conversion resumed without a value",
                })?;
                match TEMPORAL_ZONED_DATE_TIME_BAG_FIELDS[state.next] {
                    "monthCode" => {
                        let StoredValue::String(value) = value else {
                            return temporal_type_error(
                                state.realm,
                                &state.origin,
                                "Temporal.ZonedDateTime monthCode must be a string",
                            );
                        };
                        state.month_code =
                            Some(match MonthCode::from_str(&value.to_utf8_lossy()?) {
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
                            });
                    }
                    "offset" => {
                        let value =
                            operator_primitive_to_string(value, state.realm, &state.origin)?;
                        state.offset = Some(match UtcOffset::from_str(&value.to_utf8_lossy()?) {
                            Ok(offset) => offset,
                            Err(error) => {
                                return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                                    state.realm,
                                    &state.origin,
                                    error,
                                )?));
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
                            message: "unknown Temporal.ZonedDateTime property bag field",
                        }
                        .into());
                    }
                }
                state.next = state.next.saturating_add(1);
                state.stage = TemporalZonedDateTimeBagStage::ReadField;
            }
        }
    }
}

fn temporal_zoned_date_time_bag_continuation(
    state: TemporalZonedDateTimeBagContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalZonedDateTimeBag(Box::new(state))
}

fn temporal_zoned_date_time_calendar_from_value(
    runtime: &Runtime,
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Calendar, NativeFailure> {
    match value {
        StoredValue::String(value) => match Calendar::from_str(&value.to_utf8_lossy()?) {
            Ok(calendar) => Ok(calendar),
            Err(error) => Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?)),
        },
        StoredValue::Object(object) => {
            if let Some(value) = runtime.temporal_plain_date(object)? {
                return Ok(value.calendar().clone());
            }
            if let Some(value) = runtime.temporal_plain_date_time(object)? {
                return Ok(value.calendar().clone());
            }
            if let Some(value) = runtime.temporal_plain_month_day(object)? {
                return Ok(value.calendar().clone());
            }
            if let Some(value) = runtime.temporal_plain_year_month(object)? {
                return Ok(value.calendar().clone());
            }
            if let Some(value) = runtime.temporal_zoned_date_time(object)? {
                return Ok(value.calendar().clone());
            }
            Err(NativeFailure::Abrupt(temporal_pending_exception(
                realm,
                origin,
                ExceptionKind::TypeError,
                "Temporal.ZonedDateTime calendar must be a calendar identifier or Temporal object",
            )?))
        }
        _ => Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Temporal.ZonedDateTime calendar must be a calendar identifier or Temporal object",
        )?)),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "all prepared property-bag fields are passed explicitly into temporal_rs"
)]
fn temporal_zoned_date_time_from_bag_fields(
    fields: TemporalZonedDateTimeBagFields,
    overflow: Overflow,
    disambiguation: Disambiguation,
    offset_option: OffsetDisambiguation,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<ZonedDateTime, NativeFailure> {
    if fields.year.is_none()
        || fields.day.is_none()
        || (fields.month.is_none() && fields.month_code.is_none())
    {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Temporal.ZonedDateTime property bag is missing a required field",
        )?));
    }
    let Some(time_zone) = fields.time_zone else {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Temporal.ZonedDateTime property bag is missing timeZone",
        )?));
    };
    let year = temporal_plain_date_time_i32(
        temporal_plain_date_time_required_field(fields.year, realm, origin)?,
        realm,
        origin,
    )?;
    let day = temporal_plain_date_time_u8(
        temporal_plain_date_time_required_field(fields.day, realm, origin)?,
        realm,
        origin,
    )?;
    let month = temporal_plain_date_time_optional_u8(fields.month, realm, origin)?;
    let hour = temporal_plain_date_time_optional_u8(fields.hour, realm, origin)?;
    let minute = temporal_plain_date_time_optional_u8(fields.minute, realm, origin)?;
    let second = temporal_plain_date_time_optional_u8(fields.second, realm, origin)?;
    let millisecond = temporal_plain_date_time_optional_u16(fields.millisecond, realm, origin)?;
    let microsecond = temporal_plain_date_time_optional_u16(fields.microsecond, realm, origin)?;
    let nanosecond = temporal_plain_date_time_optional_u16(fields.nanosecond, realm, origin)?;
    let calendar_fields = CalendarFields::new()
        .with_year(year)
        .with_optional_month(month)
        .with_optional_month_code(fields.month_code)
        .with_day(day);
    let time = PartialTime::new()
        .with_hour(hour)
        .with_minute(minute)
        .with_second(second)
        .with_millisecond(millisecond)
        .with_microsecond(microsecond)
        .with_nanosecond(nanosecond);
    let partial = PartialZonedDateTime {
        fields: ZonedDateTimeFields {
            calendar_fields,
            time,
            offset: fields.offset,
        },
        timezone: Some(time_zone),
        calendar: fields.calendar,
    };
    match ZonedDateTime::from_partial(
        partial,
        Some(overflow),
        Some(disambiguation),
        Some(offset_option),
    ) {
        Ok(date_time) => Ok(date_time),
        Err(error) => Err(NativeFailure::Abrupt(temporal_exception_from_error(
            realm, origin, error,
        )?)),
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one exhaustive accessor dispatcher keeps branded-slot reads and errors auditable"
)]
pub(in crate::vm) fn dispatch_temporal_zoned_date_time_prototype(
    runtime: &mut Runtime,
    method: TemporalZonedDateTimePrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let date_time = require_temporal_zoned_date_time(runtime, receiver, realm, origin)?;
    let number = |value| NativeDispatch::Immediate(StoredValue::Number(JsNumber::from_i64(value)));
    match method {
        TemporalZonedDateTimePrototypeMethod::CalendarId => Ok(NativeDispatch::Immediate(
            StoredValue::String(JsString::from_utf8(date_time.calendar().identifier())?),
        )),
        TemporalZonedDateTimePrototypeMethod::TimeZoneId => {
            let identifier = match date_time.time_zone().identifier() {
                Ok(identifier) => identifier,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&identifier)?,
            )))
        }
        TemporalZonedDateTimePrototypeMethod::Year => Ok(number(i64::from(date_time.year()))),
        TemporalZonedDateTimePrototypeMethod::Month => Ok(number(i64::from(date_time.month()))),
        TemporalZonedDateTimePrototypeMethod::MonthCode => Ok(NativeDispatch::Immediate(
            StoredValue::String(JsString::from_utf8(date_time.month_code().as_str())?),
        )),
        TemporalZonedDateTimePrototypeMethod::Day => Ok(number(i64::from(date_time.day()))),
        TemporalZonedDateTimePrototypeMethod::Hour => Ok(number(i64::from(date_time.hour()))),
        TemporalZonedDateTimePrototypeMethod::Minute => Ok(number(i64::from(date_time.minute()))),
        TemporalZonedDateTimePrototypeMethod::Second => Ok(number(i64::from(date_time.second()))),
        TemporalZonedDateTimePrototypeMethod::Millisecond => {
            Ok(number(i64::from(date_time.millisecond())))
        }
        TemporalZonedDateTimePrototypeMethod::Microsecond => {
            Ok(number(i64::from(date_time.microsecond())))
        }
        TemporalZonedDateTimePrototypeMethod::Nanosecond => {
            Ok(number(i64::from(date_time.nanosecond())))
        }
        TemporalZonedDateTimePrototypeMethod::Offset => Ok(NativeDispatch::Immediate(
            StoredValue::String(JsString::from_utf8(&date_time.offset())?),
        )),
        TemporalZonedDateTimePrototypeMethod::OffsetNanoseconds => {
            Ok(number(date_time.offset_nanoseconds()))
        }
        TemporalZonedDateTimePrototypeMethod::DayOfWeek => {
            Ok(number(i64::from(date_time.day_of_week())))
        }
        TemporalZonedDateTimePrototypeMethod::DayOfYear => {
            Ok(number(i64::from(date_time.day_of_year())))
        }
        TemporalZonedDateTimePrototypeMethod::WeekOfYear => Ok(match date_time.week_of_year() {
            Some(value) => number(i64::from(value)),
            None => NativeDispatch::Immediate(StoredValue::Undefined),
        }),
        TemporalZonedDateTimePrototypeMethod::YearOfWeek => Ok(match date_time.year_of_week() {
            Some(value) => number(i64::from(value)),
            None => NativeDispatch::Immediate(StoredValue::Undefined),
        }),
        TemporalZonedDateTimePrototypeMethod::DaysInWeek => {
            Ok(number(i64::from(date_time.days_in_week())))
        }
        TemporalZonedDateTimePrototypeMethod::DaysInMonth => {
            Ok(number(i64::from(date_time.days_in_month())))
        }
        TemporalZonedDateTimePrototypeMethod::DaysInYear => {
            Ok(number(i64::from(date_time.days_in_year())))
        }
        TemporalZonedDateTimePrototypeMethod::MonthsInYear => {
            Ok(number(i64::from(date_time.months_in_year())))
        }
        TemporalZonedDateTimePrototypeMethod::InLeapYear => Ok(NativeDispatch::Immediate(
            StoredValue::Boolean(date_time.in_leap_year()),
        )),
        TemporalZonedDateTimePrototypeMethod::Era => Ok(match date_time.era() {
            Some(value) => {
                NativeDispatch::Immediate(StoredValue::String(JsString::from_utf8(value.as_str())?))
            }
            None => NativeDispatch::Immediate(StoredValue::Undefined),
        }),
        TemporalZonedDateTimePrototypeMethod::EraYear => Ok(match date_time.era_year() {
            Some(value) => number(i64::from(value)),
            None => NativeDispatch::Immediate(StoredValue::Undefined),
        }),
        TemporalZonedDateTimePrototypeMethod::EpochMilliseconds => Ok(NativeDispatch::Immediate(
            StoredValue::Number(JsNumber::from_i64(date_time.epoch_milliseconds())),
        )),
        TemporalZonedDateTimePrototypeMethod::EpochNanoseconds => {
            Ok(NativeDispatch::Immediate(StoredValue::BigInt(Arc::new(
                JsBigInt::from_i128(date_time.epoch_nanoseconds().as_i128()),
            ))))
        }
        TemporalZonedDateTimePrototypeMethod::HoursInDay => {
            let hours = match date_time.hours_in_day() {
                Ok(hours) => hours,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            Ok(NativeDispatch::Immediate(StoredValue::Number(
                JsNumber::from_f64(hours),
            )))
        }
        TemporalZonedDateTimePrototypeMethod::ToInstant => {
            allocate_temporal_instant_result(runtime, realm, date_time.to_instant())
        }
        TemporalZonedDateTimePrototypeMethod::ToPlainDate => {
            allocate_temporal_plain_date_result(runtime, realm, date_time.to_plain_date())
        }
        TemporalZonedDateTimePrototypeMethod::ToPlainTime => {
            allocate_temporal_plain_time_result(runtime, realm, date_time.to_plain_time())
        }
        TemporalZonedDateTimePrototypeMethod::ToPlainDateTime => {
            allocate_temporal_plain_date_time_result(runtime, realm, date_time.to_plain_date_time())
        }
        TemporalZonedDateTimePrototypeMethod::StartOfDay => {
            let start = match date_time.start_of_day() {
                Ok(start) => start,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            allocate_temporal_zoned_date_time_result(runtime, realm, start)
        }
        TemporalZonedDateTimePrototypeMethod::Equals => begin_temporal_zoned_date_time_like(
            runtime,
            arguments.take_first_or_undefined(),
            TemporalZonedDateTimeLikeTarget::Equals {
                receiver: date_time,
            },
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalZonedDateTimePrototypeMethod::GetTimeZoneTransition => {
            begin_temporal_zoned_date_time_get_time_zone_transition(
                runtime,
                date_time,
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalZonedDateTimePrototypeMethod::WithPlainTime => {
            begin_temporal_zoned_date_time_with_plain_time(
                runtime,
                date_time,
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalZonedDateTimePrototypeMethod::WithTimeZone => {
            let value = arguments.take_first_or_undefined();
            let StoredValue::String(value) = value else {
                return temporal_type_error(
                    realm,
                    origin,
                    "Temporal.ZonedDateTime.withTimeZone requires a time-zone string",
                );
            };
            let time_zone = match TimeZone::try_from_str(&value.to_utf8_lossy()?) {
                Ok(time_zone) => time_zone,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            let result = match ZonedDateTime::try_new(
                date_time.epoch_nanoseconds().as_i128(),
                time_zone,
                date_time.calendar().clone(),
            ) {
                Ok(result) => result,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            allocate_temporal_zoned_date_time_result(runtime, realm, result)
        }
        TemporalZonedDateTimePrototypeMethod::Add
        | TemporalZonedDateTimePrototypeMethod::Subtract => {
            let duration = arguments.take_first_or_undefined();
            let options = arguments.take_first_or_undefined();
            begin_temporal_duration_like(
                runtime,
                duration,
                TemporalDurationLikeTarget::ZonedDateTimeArithmetic {
                    receiver: date_time,
                    subtract: matches!(method, TemporalZonedDateTimePrototypeMethod::Subtract),
                    options,
                },
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalZonedDateTimePrototypeMethod::ToString => begin_temporal_zoned_date_time_to_string(
            runtime,
            date_time,
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalZonedDateTimePrototypeMethod::ToJson
        | TemporalZonedDateTimePrototypeMethod::ToLocaleString => {
            render_temporal_zoned_date_time(&date_time, realm, origin)
        }
        TemporalZonedDateTimePrototypeMethod::ValueOf => temporal_type_error(
            realm,
            origin,
            "Temporal.ZonedDateTime cannot be converted to a primitive value",
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the shared ToTemporalTime conversion retains the ZonedDateTime call context"
)]
fn begin_temporal_zoned_date_time_with_plain_time(
    runtime: &mut Runtime,
    receiver: ZonedDateTime,
    value: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(value, StoredValue::Undefined) {
        return finish_temporal_zoned_date_time_with_plain_time(
            runtime, &receiver, None, realm, &origin,
        );
    }
    begin_temporal_plain_time_like(
        runtime,
        value,
        TemporalPlainTimeLikeTarget::ZonedDateTimeWithPlainTime { receiver },
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the options-object direction Get retains the complete native transition call context"
)]
fn begin_temporal_zoned_date_time_get_time_zone_transition(
    runtime: &mut Runtime,
    date_time: ZonedDateTime,
    direction_param: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match direction_param {
        StoredValue::String(direction) => complete_temporal_zoned_date_time_transition(
            runtime, &date_time, &direction, realm, &origin,
        ),
        StoredValue::Undefined => temporal_type_error(
            realm,
            &origin,
            "Temporal.ZonedDateTime.getTimeZoneTransition requires a direction",
        ),
        options if options.heap_reference().is_some() => {
            begin_temporal_zoned_date_time_transition_direction_get(
                runtime,
                TemporalZonedDateTimeTransitionContinuation {
                    date_time,
                    options,
                    realm,
                    origin,
                },
                return_to,
                execution_budget,
            )
        }
        _ => temporal_type_error(
            realm,
            &origin,
            "Temporal.ZonedDateTime.getTimeZoneTransition direction must be a string or object",
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the observable direction Get retains its Temporal native call context"
)]
fn begin_temporal_zoned_date_time_transition_direction_get(
    runtime: &mut Runtime,
    state: TemporalZonedDateTimeTransitionContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
    let name = JsString::from_utf8("direction")?;
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
        temporal_zoned_date_time_transition_continuation,
        "Temporal.ZonedDateTime getTimeZoneTransition direction Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_zoned_date_time_transition(
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

fn temporal_zoned_date_time_transition_continuation(
    state: TemporalZonedDateTimeTransitionContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalZonedDateTimeTransition(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the direction primitive conversion remains resumable and GC-safe"
)]
pub(in crate::vm) fn advance_temporal_zoned_date_time_transition(
    runtime: &mut Runtime,
    state: TemporalZonedDateTimeTransitionContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(value, StoredValue::Undefined) {
        return temporal_range_error(
            state.realm,
            &state.origin,
            "Temporal.ZonedDateTime.getTimeZoneTransition direction is required",
        );
    }
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::String,
        OperatorPrimitiveTarget::TemporalZonedDateTimeTransition(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion continuation retains the native transition call context"
)]
pub(in crate::vm) fn finish_temporal_zoned_date_time_transition(
    runtime: &mut Runtime,
    state: &TemporalZonedDateTimeTransitionContinuation,
    value: StoredValue,
    _return_to: Option<CallReturn>,
    _execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let direction = operator_primitive_to_string(value, state.realm, &state.origin)?;
    complete_temporal_zoned_date_time_transition(
        runtime,
        &state.date_time,
        &direction,
        state.realm,
        &state.origin,
    )
}

fn complete_temporal_zoned_date_time_transition(
    runtime: &mut Runtime,
    date_time: &ZonedDateTime,
    direction: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let direction = match direction.to_utf8_lossy()?.as_str() {
        "next" => TransitionDirection::Next,
        "previous" => TransitionDirection::Previous,
        _ => {
            return temporal_range_error(
                realm,
                origin,
                "invalid Temporal time-zone transition direction",
            );
        }
    };
    let transition = match date_time.get_time_zone_transition(direction) {
        Ok(transition) => transition,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    match transition {
        Some(transition) => allocate_temporal_zoned_date_time_result(runtime, realm, transition),
        None => Ok(NativeDispatch::Immediate(StoredValue::Null)),
    }
}

fn render_temporal_zoned_date_time(
    date_time: &ZonedDateTime,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let rendered = match date_time.to_ixdtf_string(
        DisplayOffset::Auto,
        DisplayTimeZone::Auto,
        DisplayCalendar::Auto,
        ToStringRoundingOptions::default(),
    ) {
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

#[allow(
    clippy::too_many_arguments,
    reason = "the ordered ZonedDateTime formatting reader owns its resumable native call context"
)]
fn begin_temporal_zoned_date_time_to_string(
    runtime: &mut Runtime,
    date_time: ZonedDateTime,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return render_temporal_zoned_date_time(&date_time, realm, &origin);
    }
    if options.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.ZonedDateTime.prototype.toString options must be an object",
        );
    }
    begin_temporal_zoned_date_time_to_string_get(
        runtime,
        TemporalZonedDateTimeToStringContinuation {
            date_time,
            options,
            calendar_name: None,
            fractional_second_digits: None,
            offset: None,
            rounding_mode: None,
            smallest_unit: None,
            time_zone_name: None,
            stage: TemporalZonedDateTimeToStringStage::CalendarName,
            realm,
            origin,
        },
        "calendarName",
        TemporalZonedDateTimeToStringStage::CalendarName,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "each observable ZonedDateTime formatting option Get retains native call state"
)]
fn begin_temporal_zoned_date_time_to_string_get(
    runtime: &mut Runtime,
    mut state: TemporalZonedDateTimeToStringContinuation,
    name: &str,
    next_stage: TemporalZonedDateTimeToStringStage,
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
        temporal_zoned_date_time_to_string_continuation,
        "Temporal.ZonedDateTime toString option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_zoned_date_time_to_string_options(
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

fn temporal_zoned_date_time_to_string_continuation(
    state: TemporalZonedDateTimeToStringContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalZonedDateTimeToStringOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    reason = "one ordered state table preserves ZonedDateTime formatting option reads and coercions"
)]
pub(in crate::vm) fn advance_temporal_zoned_date_time_to_string_options(
    runtime: &mut Runtime,
    mut state: TemporalZonedDateTimeToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(value, StoredValue::Undefined) {
        return continue_temporal_zoned_date_time_to_string_options(
            runtime,
            state,
            return_to,
            execution_budget,
        );
    }
    let value = match (state.stage, value) {
        (
            TemporalZonedDateTimeToStringStage::FractionalSecondDigits,
            StoredValue::Number(value),
        ) => {
            state.fractional_second_digits =
                Some(TemporalZonedDateTimeFractionalSecondDigits::Number(value));
            return continue_temporal_zoned_date_time_to_string_options(
                runtime,
                state,
                return_to,
                execution_budget,
            );
        }
        (_, value) => value,
    };
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::String,
        OperatorPrimitiveTarget::TemporalZonedDateTimeToString(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion continuation retains the native formatting call context"
)]
pub(in crate::vm) fn finish_temporal_zoned_date_time_to_string_option(
    runtime: &mut Runtime,
    mut state: TemporalZonedDateTimeToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    match state.stage {
        TemporalZonedDateTimeToStringStage::CalendarName => state.calendar_name = Some(source),
        TemporalZonedDateTimeToStringStage::FractionalSecondDigits => {
            state.fractional_second_digits =
                Some(TemporalZonedDateTimeFractionalSecondDigits::String(source));
        }
        TemporalZonedDateTimeToStringStage::Offset => state.offset = Some(source),
        TemporalZonedDateTimeToStringStage::RoundingMode => state.rounding_mode = Some(source),
        TemporalZonedDateTimeToStringStage::SmallestUnit => state.smallest_unit = Some(source),
        TemporalZonedDateTimeToStringStage::TimeZoneName => state.time_zone_name = Some(source),
    }
    continue_temporal_zoned_date_time_to_string_options(runtime, state, return_to, execution_budget)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the next observable ZonedDateTime formatting option is selected explicitly"
)]
fn continue_temporal_zoned_date_time_to_string_options(
    runtime: &mut Runtime,
    state: TemporalZonedDateTimeToStringContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (name, next_stage) = match state.stage {
        TemporalZonedDateTimeToStringStage::CalendarName => (
            "fractionalSecondDigits",
            TemporalZonedDateTimeToStringStage::FractionalSecondDigits,
        ),
        TemporalZonedDateTimeToStringStage::FractionalSecondDigits => {
            ("offset", TemporalZonedDateTimeToStringStage::Offset)
        }
        TemporalZonedDateTimeToStringStage::Offset => (
            "roundingMode",
            TemporalZonedDateTimeToStringStage::RoundingMode,
        ),
        TemporalZonedDateTimeToStringStage::RoundingMode => (
            "smallestUnit",
            TemporalZonedDateTimeToStringStage::SmallestUnit,
        ),
        TemporalZonedDateTimeToStringStage::SmallestUnit => (
            "timeZoneName",
            TemporalZonedDateTimeToStringStage::TimeZoneName,
        ),
        TemporalZonedDateTimeToStringStage::TimeZoneName => {
            return complete_temporal_zoned_date_time_to_string(&state);
        }
    };
    begin_temporal_zoned_date_time_to_string_get(
        runtime,
        state,
        name,
        next_stage,
        return_to,
        execution_budget,
    )
}

fn complete_temporal_zoned_date_time_to_string(
    state: &TemporalZonedDateTimeToStringContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    let display_calendar = match state.calendar_name.as_ref() {
        Some(source) => temporal_display_calendar(source, state.realm, &state.origin)?,
        None => DisplayCalendar::Auto,
    };
    let precision = match state.fractional_second_digits.as_ref() {
        None => Precision::Auto,
        Some(TemporalZonedDateTimeFractionalSecondDigits::Number(value)) => {
            temporal_fractional_second_digits(*value, state.realm, &state.origin)?
        }
        Some(TemporalZonedDateTimeFractionalSecondDigits::String(source))
            if source.to_utf8_lossy()?.as_str() == "auto" =>
        {
            Precision::Auto
        }
        Some(TemporalZonedDateTimeFractionalSecondDigits::String(_)) => {
            return temporal_range_error(
                state.realm,
                &state.origin,
                "fractionalSecondDigits must be a Number or the string auto",
            );
        }
    };
    let display_offset = match state.offset.as_ref() {
        Some(source) => temporal_display_offset(source, state.realm, &state.origin)?,
        None => DisplayOffset::Auto,
    };
    let rounding_mode = match state.rounding_mode.as_ref() {
        Some(source) => temporal_rounding_mode(source, state.realm, &state.origin)?,
        None => RoundingMode::Trunc,
    };
    let smallest_unit = match state.smallest_unit.as_ref() {
        Some(source) => Some(temporal_round_unit(source, state.realm, &state.origin)?),
        None => None,
    };
    let display_time_zone = match state.time_zone_name.as_ref() {
        Some(source) => temporal_display_time_zone(source, state.realm, &state.origin)?,
        None => DisplayTimeZone::Auto,
    };
    match smallest_unit {
        None
        | Some(
            Unit::Minute | Unit::Second | Unit::Millisecond | Unit::Microsecond | Unit::Nanosecond,
        ) => {}
        Some(Unit::Auto | Unit::Hour | Unit::Day | Unit::Week | Unit::Month | Unit::Year) => {
            return temporal_range_error(
                state.realm,
                &state.origin,
                "smallestUnit must be minute, second, millisecond, microsecond, or nanosecond",
            );
        }
    }
    let options = ToStringRoundingOptions {
        precision,
        smallest_unit,
        rounding_mode: Some(rounding_mode),
    };
    let rendered = match state.date_time.to_ixdtf_string(
        display_offset,
        display_time_zone,
        display_calendar,
        options,
    ) {
        Ok(rendered) => rendered,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                state.realm,
                &state.origin,
                error,
            )?));
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::String(
        JsString::from_utf8(&rendered)?,
    )))
}

fn temporal_display_offset(
    source: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<DisplayOffset, NativeFailure> {
    match source.to_utf8_lossy()?.parse::<DisplayOffset>() {
        Ok(display_offset) => Ok(display_offset),
        Err(_) => Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "invalid Temporal offset",
        )?)),
    }
}

fn temporal_display_time_zone(
    source: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<DisplayTimeZone, NativeFailure> {
    match source.to_utf8_lossy()?.parse::<DisplayTimeZone>() {
        Ok(display_time_zone) => Ok(display_time_zone),
        Err(_) => Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "invalid Temporal timeZoneName",
        )?)),
    }
}

pub(in crate::vm) fn finish_temporal_zoned_date_time_with_plain_time(
    runtime: &mut Runtime,
    receiver: &ZonedDateTime,
    time: Option<PlainTime>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let result = match receiver.with_plain_time(time) {
        Ok(result) => result,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    allocate_temporal_zoned_date_time_result(runtime, realm, result)
}

#[derive(Clone, Copy)]
enum TemporalZonedDateTimeToStringStage {
    CalendarName,
    FractionalSecondDigits,
    Offset,
    RoundingMode,
    SmallestUnit,
    TimeZoneName,
}

enum TemporalZonedDateTimeFractionalSecondDigits {
    Number(JsNumber),
    String(JsString),
}

/// Resumable options state for `Temporal.ZonedDateTime.prototype.toString`.
/// The specification requires all option Gets and primitive coercions before
/// formatting validation, so the complete call state stays rooted here.
pub(in crate::vm) struct TemporalZonedDateTimeToStringContinuation {
    date_time: ZonedDateTime,
    options: StoredValue,
    calendar_name: Option<JsString>,
    fractional_second_digits: Option<TemporalZonedDateTimeFractionalSecondDigits>,
    offset: Option<JsString>,
    rounding_mode: Option<JsString>,
    smallest_unit: Option<JsString>,
    time_zone_name: Option<JsString>,
    stage: TemporalZonedDateTimeToStringStage,
    realm: RealmId,
    origin: JsStackFrame,
}

/// Resumable options-object state for
/// `Temporal.ZonedDateTime.prototype.getTimeZoneTransition`.
/// Its sole `direction` Get and string coercion may invoke JavaScript.
pub(in crate::vm) struct TemporalZonedDateTimeTransitionContinuation {
    date_time: ZonedDateTime,
    options: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalZonedDateTimeTransitionContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

impl TemporalZonedDateTimeToStringContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}
