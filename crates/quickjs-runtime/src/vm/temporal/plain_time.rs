use super::super::conversions::operator_primitive_to_string;
#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use core::str::FromStr;
use temporal_rs::{
    PlainDate, PlainDateTime, PlainTime, TimeZone, ZonedDateTime,
    options::{
        DifferenceSettings, Overflow, RoundingIncrement, RoundingMode, RoundingOptions,
        ToStringRoundingOptions, Unit,
    },
    parsers::Precision,
    partial::PartialTime,
};

/// The `Temporal.PlainTime` constructor retains each ISO time component
/// while user-defined numeric conversion is resumed.
pub(in crate::vm) struct TemporalPlainTimeConstructorContinuation {
    arguments: Vec<StoredValue>,
    converted: Vec<JsNumber>,
    new_target: FunctionId,
}

pub(in crate::vm) const TEMPORAL_PLAIN_TIME_BAG_FIELDS: [&str; 6] = [
    "hour",
    "microsecond",
    "millisecond",
    "minute",
    "nanosecond",
    "second",
];

pub(in crate::vm) enum TemporalPlainTimeLikeTarget {
    From {
        options: StoredValue,
    },
    CompareFirst {
        second: StoredValue,
    },
    CompareSecond {
        first: PlainTime,
    },
    With {
        receiver: PlainTime,
        options: StoredValue,
    },
    Difference {
        receiver: PlainTime,
        options: StoredValue,
        since: bool,
    },
    Equals {
        receiver: PlainTime,
    },
    ZonedDateTimeWithPlainTime {
        receiver: ZonedDateTime,
    },
    PlainDateTimeWithPlainTime {
        receiver: PlainDateTime,
    },
    PlainDateToZonedDateTime {
        receiver: PlainDate,
        time_zone: TimeZone,
    },
}

impl TemporalPlainTimeLikeTarget {
    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::From { options }
            | Self::CompareFirst { second: options }
            | Self::With { options, .. }
            | Self::Difference { options, .. } => {
                trace_stored_value_root(options, mark);
            }
            Self::CompareSecond { .. }
            | Self::Equals { .. }
            | Self::ZonedDateTimeWithPlainTime { .. }
            | Self::PlainDateTimeWithPlainTime { .. }
            | Self::PlainDateToZonedDateTime { .. } => {}
        }
    }
}

#[derive(Clone, Copy)]
enum TemporalPlainTimeBagStage {
    ReadCalendar,
    AwaitCalendar,
    ReadTimeZone,
    AwaitTimeZone,
    ReadField,
    AwaitField,
    AwaitConversion,
}

/// Resumable `ToTemporalTime` conversion for ordinary property bags.
///
/// `PrepareTemporalFields` requires these Gets and numeric conversions in
/// order, and each can execute JavaScript. The state owns the source bag
/// until a `PartialTime` can be passed to its target.
pub(in crate::vm) struct TemporalPlainTimeBagContinuation {
    base: StoredValue,
    hour: Option<JsNumber>,
    microsecond: Option<JsNumber>,
    millisecond: Option<JsNumber>,
    minute: Option<JsNumber>,
    nanosecond: Option<JsNumber>,
    second: Option<JsNumber>,
    next: usize,
    stage: TemporalPlainTimeBagStage,
    target: TemporalPlainTimeLikeTarget,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainTimeBagContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        2
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.base, mark);
        self.target.trace_roots(mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalPlainTimeOptionsTarget {
    Existing(PlainTime),
    Partial(PartialTime),
    With {
        receiver: PlainTime,
        partial: PartialTime,
    },
}

#[derive(Clone, Copy)]
enum TemporalPlainTimeOptionsStage {
    ReadOverflow,
    AwaitOverflow,
    AwaitOverflowConversion,
}

/// Resumable `PlainTime` overflow options state.
///
/// The input field conversion always precedes `GetOptionsObject`, while the
/// overflow Get/conversion precedes construction or partial-field replacement.
pub(in crate::vm) struct TemporalPlainTimeOptionsContinuation {
    target: TemporalPlainTimeOptionsTarget,
    options: StoredValue,
    stage: TemporalPlainTimeOptionsStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainTimeOptionsContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

impl TemporalPlainTimeConstructorContinuation {
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
pub(in crate::vm) fn begin_temporal_plain_time_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = inputs.new_target else {
        return temporal_type_error(realm, &origin, "Temporal.PlainTime is not callable");
    };
    let mut arguments = inputs.arguments.into_remaining_values();
    arguments.truncate(6);
    arguments
        .try_reserve(6_usize.saturating_sub(arguments.len()))
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 6_usize.saturating_sub(arguments.len()),
        })?;
    while arguments.len() < 6 {
        arguments.push(StoredValue::Undefined);
    }
    let mut converted = Vec::new();
    converted
        .try_reserve_exact(6)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 6,
        })?;
    advance_temporal_plain_time_constructor(
        runtime,
        TemporalPlainTimeConstructorContinuation {
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
pub(in crate::vm) fn advance_temporal_plain_time_constructor(
    runtime: &mut Runtime,
    mut state: TemporalPlainTimeConstructorContinuation,
    completion: Option<JsNumber>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        // ToIntegerWithTruncation rejects a non-finite component before the
        // next argument's observable conversion begins.
        temporal_plain_time_integer(value, realm, origin)?;
        state.converted.push(value);
    }
    while state.converted.len() < 6 {
        let index = state.converted.len();
        let argument = std::mem::replace(&mut state.arguments[index], StoredValue::Undefined);
        if matches!(argument, StoredValue::Undefined) {
            state.converted.push(JsNumber::from_i32(0));
            continue;
        }
        return begin_operator_primitive_conversion(
            runtime,
            argument,
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::TemporalPlainTimeConstructor(Box::new(state)),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        );
    }
    complete_temporal_plain_time_constructor(
        runtime,
        &state,
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
fn complete_temporal_plain_time_constructor(
    runtime: &mut Runtime,
    state: &TemporalPlainTimeConstructorContinuation,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let [hour, minute, second, millisecond, microsecond, nanosecond] = state.converted.as_slice()
    else {
        return Err(EngineFault::RuntimeInvariant {
            message: "Temporal.PlainTime constructor completed before all components converted",
        }
        .into());
    };
    let hour = temporal_plain_time_integer(*hour, realm, origin)?;
    let minute = temporal_plain_time_integer(*minute, realm, origin)?;
    let second = temporal_plain_time_integer(*second, realm, origin)?;
    let millisecond = temporal_plain_time_integer(*millisecond, realm, origin)?;
    let microsecond = temporal_plain_time_integer(*microsecond, realm, origin)?;
    let nanosecond = temporal_plain_time_integer(*nanosecond, realm, origin)?;
    let (Ok(hour), Ok(minute), Ok(second)) = (
        u8::try_from(hour),
        u8::try_from(minute),
        u8::try_from(second),
    ) else {
        return temporal_range_error(
            realm,
            origin,
            "Temporal.PlainTime fields are outside the supported range",
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
            "Temporal.PlainTime fields are outside the supported range",
        );
    };
    let time = match PlainTime::try_new(hour, minute, second, millisecond, microsecond, nanosecond)
    {
        Ok(time) => time,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    begin_temporal_plain_time_wrapper(
        runtime,
        realm,
        state.new_target,
        time,
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
fn temporal_plain_time_integer(
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
            "Temporal.PlainTime fields must be finite Numbers",
        )?));
    }
    let value = value.trunc();
    if value < i64::MIN as f64 || value > i64::MAX as f64 {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "Temporal.PlainTime fields are outside the supported range",
        )?));
    }
    Ok(value as i64)
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "newTarget prototype lookup is a resumable native operation"
)]
fn begin_temporal_plain_time_wrapper(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: FunctionId,
    time: PlainTime,
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
        IntrinsicGetContinuation::TemporalPlainTimeConstructor { new_target, time },
        return_to,
        Some(origin),
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_time_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    time: PlainTime,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        _ => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_temporal_plain_time_prototype(realm)?)
        }
    };
    let object = runtime.allocate_temporal_plain_time(prototype, time)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "Temporal.PlainTime static conversion retains the native call context"
)]
pub(in crate::vm) fn begin_temporal_plain_time_static(
    runtime: &mut Runtime,
    method: TemporalPlainTimeStaticMethod,
    realm: RealmId,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        TemporalPlainTimeStaticMethod::From => {
            let value = arguments.take_first_or_undefined();
            let options = arguments.take_first_or_undefined();
            begin_temporal_plain_time_like(
                runtime,
                value,
                TemporalPlainTimeLikeTarget::From { options },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainTimeStaticMethod::Compare => {
            let first = arguments.take_first_or_undefined();
            let second = arguments.take_first_or_undefined();
            begin_temporal_plain_time_like(
                runtime,
                first,
                TemporalPlainTimeLikeTarget::CompareFirst { second },
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
    reason = "all accepted Temporal.PlainTime inputs share a resumable conversion boundary"
)]
pub(in crate::vm) fn begin_temporal_plain_time_like(
    runtime: &mut Runtime,
    value: StoredValue,
    target: TemporalPlainTimeLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(object) = value {
        if let Some(time) = runtime.temporal_plain_time(object)? {
            return continue_temporal_plain_time_like(
                runtime,
                time,
                target,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        if let Some(date_time) = runtime.temporal_plain_date_time(object)? {
            return continue_temporal_plain_time_like(
                runtime,
                PlainTime::from(date_time),
                target,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        if let Some(date_time) = runtime.temporal_zoned_date_time(object)? {
            return continue_temporal_plain_time_like(
                runtime,
                date_time.to_plain_time(),
                target,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
    }
    if let StoredValue::String(value) = value {
        let source = value.to_utf8_lossy()?;
        let time = match PlainTime::from_utf8(source.as_bytes()) {
            Ok(time) => time,
            Err(error) => {
                return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                    realm, &origin, error,
                )?));
            }
        };
        return continue_temporal_plain_time_like(
            runtime,
            time,
            target,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if value.heap_reference().is_some() {
        return advance_temporal_plain_time_property_bag(
            runtime,
            TemporalPlainTimeBagContinuation {
                base: value,
                hour: None,
                microsecond: None,
                millisecond: None,
                minute: None,
                nanosecond: None,
                second: None,
                next: 0,
                stage: TemporalPlainTimeBagStage::ReadField,
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
        "Temporal.PlainTime requires a PlainTime, PlainDateTime, ISO string, or property bag",
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the conversion target owns observable arguments across resumption"
)]
fn continue_temporal_plain_time_like(
    runtime: &mut Runtime,
    time: PlainTime,
    target: TemporalPlainTimeLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match target {
        TemporalPlainTimeLikeTarget::From { options } => begin_temporal_plain_time_options(
            runtime,
            TemporalPlainTimeOptionsTarget::Existing(time),
            options,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        TemporalPlainTimeLikeTarget::CompareFirst { second } => begin_temporal_plain_time_like(
            runtime,
            second,
            TemporalPlainTimeLikeTarget::CompareSecond { first: time },
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        TemporalPlainTimeLikeTarget::CompareSecond { first } => {
            let result = match first.cmp(&time) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            Ok(NativeDispatch::Immediate(StoredValue::Number(
                JsNumber::from_i32(result),
            )))
        }
        TemporalPlainTimeLikeTarget::With { .. } => {
            unreachable!("Temporal.PlainTime.with completes from its property-bag state")
        }
        TemporalPlainTimeLikeTarget::Difference {
            receiver,
            options,
            since,
        } => begin_temporal_plain_time_difference(
            runtime,
            receiver,
            time,
            options,
            since,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        TemporalPlainTimeLikeTarget::Equals { receiver } => Ok(NativeDispatch::Immediate(
            StoredValue::Boolean(receiver == time),
        )),
        TemporalPlainTimeLikeTarget::ZonedDateTimeWithPlainTime { receiver } => {
            finish_temporal_zoned_date_time_with_plain_time(
                runtime,
                &receiver,
                Some(time),
                realm,
                &origin,
            )
        }
        TemporalPlainTimeLikeTarget::PlainDateTimeWithPlainTime { receiver } => {
            finish_temporal_plain_date_time_with_plain_time(
                runtime,
                &receiver,
                Some(time),
                realm,
                &origin,
            )
        }
        TemporalPlainTimeLikeTarget::PlainDateToZonedDateTime {
            receiver,
            time_zone,
        } => finish_temporal_plain_date_to_zoned_date_time(
            runtime,
            &receiver,
            time_zone,
            Some(time),
            realm,
            &origin,
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the native method retains fields and options through observable property access"
)]
fn begin_temporal_plain_time_with(
    runtime: &mut Runtime,
    receiver: PlainTime,
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
            "Temporal.PlainTime.with requires a property bag",
        );
    }
    if let StoredValue::Object(object) = fields
        && (runtime.temporal_plain_date(object)?.is_some()
            || runtime.temporal_plain_date_time(object)?.is_some()
            || runtime.temporal_plain_time(object)?.is_some())
    {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.PlainTime.with does not accept a Temporal object",
        );
    }
    advance_temporal_plain_time_property_bag(
        runtime,
        TemporalPlainTimeBagContinuation {
            base: fields,
            hour: None,
            microsecond: None,
            millisecond: None,
            minute: None,
            nanosecond: None,
            second: None,
            next: 0,
            stage: TemporalPlainTimeBagStage::ReadCalendar,
            target: TemporalPlainTimeLikeTarget::With { receiver, options },
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
    clippy::too_many_lines,
    reason = "the explicit state machine preserves property access and conversion order"
)]
pub(in crate::vm) fn advance_temporal_plain_time_property_bag(
    runtime: &mut Runtime,
    mut state: TemporalPlainTimeBagContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalPlainTimeBagStage::ReadCalendar => {
                charge_heap_property_lookup(runtime, &state.base, execution_budget)?;
                let name = JsString::from_utf8("calendar")?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalPlainTimeBagStage::AwaitCalendar;
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
                    temporal_plain_time_bag_continuation,
                    "Temporal.PlainTime.with calendar Get produced a structured result",
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
            TemporalPlainTimeBagStage::AwaitCalendar => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainTime.with calendar Get resumed without a value",
                })?;
                if !matches!(value, StoredValue::Undefined) {
                    return temporal_type_error(
                        state.realm,
                        &state.origin,
                        "Temporal.PlainTime.with cannot override calendar",
                    );
                }
                state.stage = TemporalPlainTimeBagStage::ReadTimeZone;
            }
            TemporalPlainTimeBagStage::ReadTimeZone => {
                charge_heap_property_lookup(runtime, &state.base, execution_budget)?;
                let name = JsString::from_utf8("timeZone")?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalPlainTimeBagStage::AwaitTimeZone;
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
                    temporal_plain_time_bag_continuation,
                    "Temporal.PlainTime.with timeZone Get produced a structured result",
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
            TemporalPlainTimeBagStage::AwaitTimeZone => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainTime.with timeZone Get resumed without a value",
                })?;
                if !matches!(value, StoredValue::Undefined) {
                    return temporal_type_error(
                        state.realm,
                        &state.origin,
                        "Temporal.PlainTime.with cannot override timeZone",
                    );
                }
                state.stage = TemporalPlainTimeBagStage::ReadField;
            }
            TemporalPlainTimeBagStage::ReadField => {
                if state.next == TEMPORAL_PLAIN_TIME_BAG_FIELDS.len() {
                    let partial = temporal_plain_time_partial_from_bag(&state)?;
                    return match state.target {
                        TemporalPlainTimeLikeTarget::From { options } => {
                            begin_temporal_plain_time_options(
                                runtime,
                                TemporalPlainTimeOptionsTarget::Partial(partial),
                                options,
                                state.realm,
                                return_to,
                                state.origin,
                                execution_budget,
                            )
                        }
                        TemporalPlainTimeLikeTarget::With { receiver, options } => {
                            if partial.is_empty() {
                                return temporal_type_error(
                                    state.realm,
                                    &state.origin,
                                    "Temporal.PlainTime.with requires at least one time field",
                                );
                            }
                            begin_temporal_plain_time_options(
                                runtime,
                                TemporalPlainTimeOptionsTarget::With { receiver, partial },
                                options,
                                state.realm,
                                return_to,
                                state.origin,
                                execution_budget,
                            )
                        }
                        target => {
                            let time =
                                match PlainTime::from_partial(partial, Some(Overflow::Constrain)) {
                                    Ok(time) => time,
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
                            continue_temporal_plain_time_like(
                                runtime,
                                time,
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
                let name = JsString::from_utf8(TEMPORAL_PLAIN_TIME_BAG_FIELDS[state.next])?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalPlainTimeBagStage::AwaitField;
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
                    temporal_plain_time_bag_continuation,
                    "Temporal.PlainTime property bag Get produced a structured result",
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
            TemporalPlainTimeBagStage::AwaitField => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainTime property bag Get resumed without a value",
                })?;
                if matches!(value, StoredValue::Undefined) {
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainTimeBagStage::ReadField;
                    continue;
                }
                state.stage = TemporalPlainTimeBagStage::AwaitConversion;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::TemporalPlainTimeBag(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalPlainTimeBagStage::AwaitConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainTime property bag conversion resumed without a value",
                })?;
                let value = operator_to_number(value, state.realm, &state.origin)?;
                match TEMPORAL_PLAIN_TIME_BAG_FIELDS[state.next] {
                    "hour" => state.hour = Some(value),
                    "microsecond" => state.microsecond = Some(value),
                    "millisecond" => state.millisecond = Some(value),
                    "minute" => state.minute = Some(value),
                    "nanosecond" => state.nanosecond = Some(value),
                    "second" => state.second = Some(value),
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "unknown Temporal.PlainTime property-bag field",
                        }
                        .into());
                    }
                }
                state.next = state.next.saturating_add(1);
                state.stage = TemporalPlainTimeBagStage::ReadField;
            }
        }
    }
}

fn temporal_plain_time_bag_continuation(
    state: TemporalPlainTimeBagContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainTimeBag(Box::new(state))
}

fn temporal_plain_time_partial_from_bag(
    state: &TemporalPlainTimeBagContinuation,
) -> Result<PartialTime, NativeFailure> {
    let convert_u8 = |value: Option<JsNumber>| temporal_plain_time_optional_u8(value, state);
    let convert_u16 = |value: Option<JsNumber>| temporal_plain_time_optional_u16(value, state);
    Ok(PartialTime::new()
        .with_hour(convert_u8(state.hour)?)
        .with_minute(convert_u8(state.minute)?)
        .with_second(convert_u8(state.second)?)
        .with_millisecond(convert_u16(state.millisecond)?)
        .with_microsecond(convert_u16(state.microsecond)?)
        .with_nanosecond(convert_u16(state.nanosecond)?))
}

fn temporal_plain_time_optional_u8(
    value: Option<JsNumber>,
    state: &TemporalPlainTimeBagContinuation,
) -> Result<Option<u8>, NativeFailure> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = temporal_plain_time_integer(value, state.realm, &state.origin)?;
    let Ok(value) = u8::try_from(value) else {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            state.realm,
            &state.origin,
            ExceptionKind::RangeError,
            "Temporal.PlainTime fields are outside the supported range",
        )?));
    };
    Ok(Some(value))
}

fn temporal_plain_time_optional_u16(
    value: Option<JsNumber>,
    state: &TemporalPlainTimeBagContinuation,
) -> Result<Option<u16>, NativeFailure> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = temporal_plain_time_integer(value, state.realm, &state.origin)?;
    let Ok(value) = u16::try_from(value) else {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            state.realm,
            &state.origin,
            ExceptionKind::RangeError,
            "Temporal.PlainTime fields are outside the supported range",
        )?));
    };
    Ok(Some(value))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the explicit state machine preserves GetOptionsObject and overflow conversion order"
)]
fn begin_temporal_plain_time_options(
    runtime: &mut Runtime,
    target: TemporalPlainTimeOptionsTarget,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return finish_temporal_plain_time_options(
            runtime,
            target,
            Overflow::Constrain,
            realm,
            &origin,
        );
    }
    if options.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.PlainTime options must be an object",
        );
    }
    advance_temporal_plain_time_options(
        runtime,
        TemporalPlainTimeOptionsContinuation {
            target,
            options,
            stage: TemporalPlainTimeOptionsStage::ReadOverflow,
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
    clippy::too_many_lines,
    reason = "the explicit state machine preserves GetOptionsObject and overflow conversion order"
)]
pub(in crate::vm) fn advance_temporal_plain_time_options(
    runtime: &mut Runtime,
    mut state: TemporalPlainTimeOptionsContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalPlainTimeOptionsStage::ReadOverflow => {
                charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
                let name = JsString::from_utf8("overflow")?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalPlainTimeOptionsStage::AwaitOverflow;
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
                    temporal_plain_time_options_continuation,
                    "Temporal.PlainTime overflow Get produced a structured result",
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
            TemporalPlainTimeOptionsStage::AwaitOverflow => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainTime overflow Get resumed without a value",
                })?;
                if matches!(value, StoredValue::Undefined) {
                    return finish_temporal_plain_time_options(
                        runtime,
                        state.target,
                        Overflow::Constrain,
                        state.realm,
                        &state.origin,
                    );
                }
                state.stage = TemporalPlainTimeOptionsStage::AwaitOverflowConversion;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::TemporalPlainTimeOptions(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalPlainTimeOptionsStage::AwaitOverflowConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainTime overflow conversion resumed without a value",
                })?;
                let value = operator_primitive_to_string(value, state.realm, &state.origin)?;
                let Ok(overflow) = Overflow::from_str(&value.to_utf8_lossy()?) else {
                    return temporal_range_error(
                        state.realm,
                        &state.origin,
                        "Temporal.PlainTime overflow must be constrain or reject",
                    );
                };
                return finish_temporal_plain_time_options(
                    runtime,
                    state.target,
                    overflow,
                    state.realm,
                    &state.origin,
                );
            }
        }
    }
}

fn temporal_plain_time_options_continuation(
    state: TemporalPlainTimeOptionsContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainTimeOptions(Box::new(state))
}

fn finish_temporal_plain_time_options(
    runtime: &mut Runtime,
    target: TemporalPlainTimeOptionsTarget,
    overflow: Overflow,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let time = match target {
        TemporalPlainTimeOptionsTarget::Existing(time) => time,
        TemporalPlainTimeOptionsTarget::Partial(partial) => {
            match PlainTime::from_partial(partial, Some(overflow)) {
                Ok(time) => time,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            }
        }
        TemporalPlainTimeOptionsTarget::With { receiver, partial } => {
            match receiver.with(partial, Some(overflow)) {
                Ok(time) => time,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            }
        }
    };
    allocate_temporal_plain_time_result(runtime, realm, time)
}

pub(in crate::vm) fn temporal_plain_time_from_bag(
    state: &TemporalPlainDateTimeBagContinuation,
) -> Result<PlainTime, NativeFailure> {
    let hour = temporal_plain_date_time_optional_u8(state.hour, state.realm, &state.origin)?;
    let minute = temporal_plain_date_time_optional_u8(state.minute, state.realm, &state.origin)?;
    let second = temporal_plain_date_time_optional_u8(state.second, state.realm, &state.origin)?;
    let millisecond =
        temporal_plain_date_time_optional_u16(state.millisecond, state.realm, &state.origin)?;
    let microsecond =
        temporal_plain_date_time_optional_u16(state.microsecond, state.realm, &state.origin)?;
    let nanosecond =
        temporal_plain_date_time_optional_u16(state.nanosecond, state.realm, &state.origin)?;
    let partial = PartialTime::new()
        .with_hour(hour)
        .with_minute(minute)
        .with_second(second)
        .with_millisecond(millisecond)
        .with_microsecond(microsecond)
        .with_nanosecond(nanosecond);
    match PlainTime::from_partial(partial, Some(Overflow::Constrain)) {
        Ok(time) => Ok(time),
        Err(error) => Err(NativeFailure::Abrupt(temporal_exception_from_error(
            state.realm,
            &state.origin,
            error,
        )?)),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "one exhaustive dispatcher preserves receiver validation and method-specific argument order"
)]
pub(in crate::vm) fn dispatch_temporal_plain_time_prototype(
    runtime: &mut Runtime,
    method: TemporalPlainTimePrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let time = require_temporal_plain_time(runtime, receiver, realm, origin)?;
    let number = |value| NativeDispatch::Immediate(StoredValue::Number(JsNumber::from_i64(value)));
    match method {
        TemporalPlainTimePrototypeMethod::Hour => Ok(number(i64::from(time.hour()))),
        TemporalPlainTimePrototypeMethod::Minute => Ok(number(i64::from(time.minute()))),
        TemporalPlainTimePrototypeMethod::Second => Ok(number(i64::from(time.second()))),
        TemporalPlainTimePrototypeMethod::Millisecond => Ok(number(i64::from(time.millisecond()))),
        TemporalPlainTimePrototypeMethod::Microsecond => Ok(number(i64::from(time.microsecond()))),
        TemporalPlainTimePrototypeMethod::Nanosecond => Ok(number(i64::from(time.nanosecond()))),
        TemporalPlainTimePrototypeMethod::Add | TemporalPlainTimePrototypeMethod::Subtract => {
            begin_temporal_duration_like(
                runtime,
                arguments.take_first_or_undefined(),
                TemporalDurationLikeTarget::PlainTimeArithmetic {
                    receiver: time,
                    subtract: matches!(method, TemporalPlainTimePrototypeMethod::Subtract),
                },
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalPlainTimePrototypeMethod::With => begin_temporal_plain_time_with(
            runtime,
            time,
            arguments.take_first_or_undefined(),
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainTimePrototypeMethod::Round => begin_temporal_plain_time_round(
            runtime,
            time,
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainTimePrototypeMethod::Until | TemporalPlainTimePrototypeMethod::Since => {
            let other = arguments.take_first_or_undefined();
            let options = arguments.take_first_or_undefined();
            begin_temporal_plain_time_like(
                runtime,
                other,
                TemporalPlainTimeLikeTarget::Difference {
                    receiver: time,
                    options,
                    since: matches!(method, TemporalPlainTimePrototypeMethod::Since),
                },
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalPlainTimePrototypeMethod::Equals => begin_temporal_plain_time_like(
            runtime,
            arguments.take_first_or_undefined(),
            TemporalPlainTimeLikeTarget::Equals { receiver: time },
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainTimePrototypeMethod::ToString => begin_temporal_plain_time_to_string(
            runtime,
            time,
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainTimePrototypeMethod::ToJson
        | TemporalPlainTimePrototypeMethod::ToLocaleString => {
            let text = match time.to_ixdtf_string(ToStringRoundingOptions::default()) {
                Ok(text) => text,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&text)?,
            )))
        }
        TemporalPlainTimePrototypeMethod::ValueOf => temporal_type_error(
            realm,
            origin,
            "Temporal.PlainTime cannot be converted to a primitive value",
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the ordered PlainTime options reader retains native call context across user code"
)]
fn begin_temporal_plain_time_round(
    runtime: &mut Runtime,
    time: PlainTime,
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
            "Temporal.PlainTime.prototype.round requires an options object or smallest-unit string",
        ),
        StoredValue::String(source) => {
            let smallest_unit = temporal_round_unit(&source, realm, &origin)?;
            complete_temporal_plain_time_round(
                runtime,
                &time,
                RoundingIncrement::ONE,
                RoundingMode::HalfExpand,
                Some(smallest_unit),
                realm,
                &origin,
            )
        }
        options if options.heap_reference().is_some() => begin_temporal_plain_time_round_get(
            runtime,
            TemporalPlainTimeRoundContinuation {
                time,
                options,
                rounding_increment: RoundingIncrement::ONE,
                rounding_mode: RoundingMode::HalfExpand,
                stage: TemporalPlainTimeRoundStage::RoundingIncrement,
                realm,
                origin,
            },
            "roundingIncrement",
            TemporalPlainTimeRoundStage::RoundingIncrement,
            return_to,
            execution_budget,
        ),
        _ => temporal_type_error(
            realm,
            &origin,
            "Temporal.PlainTime.prototype.round requires an options object or smallest-unit string",
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "each observable PlainTime round Get retains the complete native continuation state"
)]
fn begin_temporal_plain_time_round_get(
    runtime: &mut Runtime,
    mut state: TemporalPlainTimeRoundContinuation,
    name: &str,
    next_stage: TemporalPlainTimeRoundStage,
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
        temporal_plain_time_round_continuation,
        "Temporal.PlainTime round option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_plain_time_round_options(
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

fn temporal_plain_time_round_continuation(
    state: TemporalPlainTimeRoundContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainTimeRoundOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    reason = "one ordered state table preserves PlainTime round options and coercions across suspension"
)]
pub(in crate::vm) fn advance_temporal_plain_time_round_options(
    runtime: &mut Runtime,
    state: TemporalPlainTimeRoundContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalPlainTimeRoundStage::RoundingIncrement => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_time_round_get(
                    runtime,
                    state,
                    "roundingMode",
                    TemporalPlainTimeRoundStage::RoundingMode,
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
                OperatorPrimitiveTarget::TemporalPlainTimeRoundRoundingIncrement(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainTimeRoundStage::RoundingMode => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_time_round_get(
                    runtime,
                    state,
                    "smallestUnit",
                    TemporalPlainTimeRoundStage::SmallestUnit,
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
                OperatorPrimitiveTarget::TemporalPlainTimeRoundRoundingMode(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainTimeRoundStage::SmallestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return complete_temporal_plain_time_round(
                    runtime,
                    &state.time,
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
                OperatorPrimitiveTarget::TemporalPlainTimeRoundSmallestUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

pub(in crate::vm) fn finish_temporal_plain_time_round_rounding_increment(
    runtime: &mut Runtime,
    mut state: TemporalPlainTimeRoundContinuation,
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
    begin_temporal_plain_time_round_get(
        runtime,
        state,
        "roundingMode",
        TemporalPlainTimeRoundStage::RoundingMode,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_time_round_rounding_mode(
    runtime: &mut Runtime,
    mut state: TemporalPlainTimeRoundContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.rounding_mode = temporal_rounding_mode(&source, state.realm, &state.origin)?;
    begin_temporal_plain_time_round_get(
        runtime,
        state,
        "smallestUnit",
        TemporalPlainTimeRoundStage::SmallestUnit,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_time_round_smallest_unit(
    runtime: &mut Runtime,
    state: &TemporalPlainTimeRoundContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let smallest_unit = temporal_round_unit(&source, state.realm, &state.origin)?;
    complete_temporal_plain_time_round(
        runtime,
        &state.time,
        state.rounding_increment,
        state.rounding_mode,
        Some(smallest_unit),
        state.realm,
        &state.origin,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the completed PlainTime option record is passed explicitly to the shared temporal kernel"
)]
fn complete_temporal_plain_time_round(
    runtime: &mut Runtime,
    time: &PlainTime,
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
    let rounded = match time.round(options) {
        Ok(rounded) => rounded,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    allocate_temporal_plain_time_result(runtime, realm, rounded)
}

#[derive(Clone, Copy)]
enum TemporalPlainTimeRoundStage {
    RoundingIncrement,
    RoundingMode,
    SmallestUnit,
}

/// Resumable options state for `Temporal.PlainTime.prototype.round`.
/// Every Get and primitive conversion may invoke JavaScript.
pub(in crate::vm) struct TemporalPlainTimeRoundContinuation {
    time: PlainTime,
    options: StoredValue,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    stage: TemporalPlainTimeRoundStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainTimeRoundContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalPlainTimeDifferenceStage {
    LargestUnit,
    RoundingIncrement,
    RoundingMode,
    SmallestUnit,
}

/// Resumable options state for `Temporal.PlainTime.prototype.until` and
/// `since`. The converted time operand is retained before the first options
/// Get, and every option conversion remains observable across suspension.
pub(in crate::vm) struct TemporalPlainTimeDifferenceContinuation {
    receiver: PlainTime,
    other: PlainTime,
    options: StoredValue,
    largest_unit: Option<Unit>,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    since: bool,
    stage: TemporalPlainTimeDifferenceStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainTimeDifferenceContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalPlainTimeToStringStage {
    FractionalSecondDigits,
    RoundingMode,
    SmallestUnit,
}

/// Resumable options state for `Temporal.PlainTime.prototype.toString`.
/// Every Get and primitive conversion may invoke JavaScript.
pub(in crate::vm) struct TemporalPlainTimeToStringContinuation {
    time: PlainTime,
    options: StoredValue,
    precision: Precision,
    rounding_mode: RoundingMode,
    smallest_unit: Option<Unit>,
    stage: TemporalPlainTimeToStringStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainTimeToStringContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the time operand is converted before the observable options object"
)]
fn begin_temporal_plain_time_difference(
    runtime: &mut Runtime,
    receiver: PlainTime,
    other: PlainTime,
    options: StoredValue,
    since: bool,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return complete_temporal_plain_time_difference(
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
            "Temporal.PlainTime.prototype.until options must be an object",
        );
    }
    begin_temporal_plain_time_difference_get(
        runtime,
        TemporalPlainTimeDifferenceContinuation {
            receiver,
            other,
            options,
            largest_unit: None,
            rounding_increment: RoundingIncrement::ONE,
            rounding_mode: RoundingMode::Trunc,
            since,
            stage: TemporalPlainTimeDifferenceStage::LargestUnit,
            realm,
            origin,
        },
        "largestUnit",
        TemporalPlainTimeDifferenceStage::LargestUnit,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "each time difference option Get owns native call state across suspension"
)]
fn begin_temporal_plain_time_difference_get(
    runtime: &mut Runtime,
    mut state: TemporalPlainTimeDifferenceContinuation,
    name: &str,
    next_stage: TemporalPlainTimeDifferenceStage,
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
        temporal_plain_time_difference_continuation,
        "Temporal.PlainTime difference option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_plain_time_difference_options(
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

fn temporal_plain_time_difference_continuation(
    state: TemporalPlainTimeDifferenceContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainTimeDifferenceOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one ordered state table preserves time difference option observation across suspension"
)]
pub(in crate::vm) fn advance_temporal_plain_time_difference_options(
    runtime: &mut Runtime,
    state: TemporalPlainTimeDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalPlainTimeDifferenceStage::LargestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_time_difference_get(
                    runtime,
                    state,
                    "roundingIncrement",
                    TemporalPlainTimeDifferenceStage::RoundingIncrement,
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
                OperatorPrimitiveTarget::TemporalPlainTimeDifferenceLargestUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainTimeDifferenceStage::RoundingIncrement => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_time_difference_get(
                    runtime,
                    state,
                    "roundingMode",
                    TemporalPlainTimeDifferenceStage::RoundingMode,
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
                OperatorPrimitiveTarget::TemporalPlainTimeDifferenceRoundingIncrement(Box::new(
                    state,
                )),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainTimeDifferenceStage::RoundingMode => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_time_difference_get(
                    runtime,
                    state,
                    "smallestUnit",
                    TemporalPlainTimeDifferenceStage::SmallestUnit,
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
                OperatorPrimitiveTarget::TemporalPlainTimeDifferenceRoundingMode(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainTimeDifferenceStage::SmallestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return complete_temporal_plain_time_difference(
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
                OperatorPrimitiveTarget::TemporalPlainTimeDifferenceSmallestUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

pub(in crate::vm) fn finish_temporal_plain_time_difference_largest_unit(
    runtime: &mut Runtime,
    mut state: TemporalPlainTimeDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.largest_unit = Some(temporal_round_unit(&source, state.realm, &state.origin)?);
    begin_temporal_plain_time_difference_get(
        runtime,
        state,
        "roundingIncrement",
        TemporalPlainTimeDifferenceStage::RoundingIncrement,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_time_difference_rounding_increment(
    runtime: &mut Runtime,
    mut state: TemporalPlainTimeDifferenceContinuation,
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
    begin_temporal_plain_time_difference_get(
        runtime,
        state,
        "roundingMode",
        TemporalPlainTimeDifferenceStage::RoundingMode,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_time_difference_rounding_mode(
    runtime: &mut Runtime,
    mut state: TemporalPlainTimeDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.rounding_mode = temporal_rounding_mode(&source, state.realm, &state.origin)?;
    begin_temporal_plain_time_difference_get(
        runtime,
        state,
        "smallestUnit",
        TemporalPlainTimeDifferenceStage::SmallestUnit,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_time_difference_smallest_unit(
    runtime: &mut Runtime,
    state: &TemporalPlainTimeDifferenceContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let smallest_unit = temporal_round_unit(&source, state.realm, &state.origin)?;
    complete_temporal_plain_time_difference(
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
    reason = "the completed JavaScript time difference settings are passed explicitly to the Temporal kernel"
)]
fn complete_temporal_plain_time_difference(
    runtime: &mut Runtime,
    receiver: &PlainTime,
    other: &PlainTime,
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
    reason = "the ordered PlainTime formatting reader owns its resumable native call context"
)]
fn begin_temporal_plain_time_to_string(
    runtime: &mut Runtime,
    time: PlainTime,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return complete_temporal_plain_time_to_string(
            time,
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
            "Temporal.PlainTime.prototype.toString options must be an object",
        );
    }
    begin_temporal_plain_time_to_string_get(
        runtime,
        TemporalPlainTimeToStringContinuation {
            time,
            options,
            precision: Precision::Auto,
            rounding_mode: RoundingMode::Trunc,
            smallest_unit: None,
            stage: TemporalPlainTimeToStringStage::FractionalSecondDigits,
            realm,
            origin,
        },
        "fractionalSecondDigits",
        TemporalPlainTimeToStringStage::FractionalSecondDigits,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "each observable PlainTime formatting option Get retains native call state"
)]
fn begin_temporal_plain_time_to_string_get(
    runtime: &mut Runtime,
    mut state: TemporalPlainTimeToStringContinuation,
    name: &str,
    next_stage: TemporalPlainTimeToStringStage,
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
        temporal_plain_time_to_string_continuation,
        "Temporal.PlainTime toString option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_plain_time_to_string_options(
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

fn temporal_plain_time_to_string_continuation(
    state: TemporalPlainTimeToStringContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainTimeToStringOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    reason = "one ordered state table preserves PlainTime formatting option reads and coercions"
)]
pub(in crate::vm) fn advance_temporal_plain_time_to_string_options(
    runtime: &mut Runtime,
    mut state: TemporalPlainTimeToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalPlainTimeToStringStage::FractionalSecondDigits => match value {
            StoredValue::Undefined => begin_temporal_plain_time_to_string_get(
                runtime,
                state,
                "roundingMode",
                TemporalPlainTimeToStringStage::RoundingMode,
                return_to,
                execution_budget,
            ),
            StoredValue::Number(number) => {
                state.precision =
                    temporal_fractional_second_digits(number, state.realm, &state.origin)?;
                begin_temporal_plain_time_to_string_get(
                    runtime,
                    state,
                    "roundingMode",
                    TemporalPlainTimeToStringStage::RoundingMode,
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
                    OperatorPrimitiveTarget::TemporalPlainTimeToStringFractionalSecondDigits(
                        Box::new(state),
                    ),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                )
            }
        },
        TemporalPlainTimeToStringStage::RoundingMode => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_time_to_string_get(
                    runtime,
                    state,
                    "smallestUnit",
                    TemporalPlainTimeToStringStage::SmallestUnit,
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
                OperatorPrimitiveTarget::TemporalPlainTimeToStringRoundingMode(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainTimeToStringStage::SmallestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return complete_temporal_plain_time_to_string(
                    state.time,
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
                OperatorPrimitiveTarget::TemporalPlainTimeToStringSmallestUnit(Box::new(state)),
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
pub(in crate::vm) fn finish_temporal_plain_time_to_string_fractional_second_digits(
    runtime: &mut Runtime,
    mut state: TemporalPlainTimeToStringContinuation,
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
    begin_temporal_plain_time_to_string_get(
        runtime,
        state,
        "roundingMode",
        TemporalPlainTimeToStringStage::RoundingMode,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion continuation retains the native formatting call context"
)]
pub(in crate::vm) fn finish_temporal_plain_time_to_string_rounding_mode(
    runtime: &mut Runtime,
    mut state: TemporalPlainTimeToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.rounding_mode = temporal_rounding_mode(&source, state.realm, &state.origin)?;
    begin_temporal_plain_time_to_string_get(
        runtime,
        state,
        "smallestUnit",
        TemporalPlainTimeToStringStage::SmallestUnit,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_time_to_string_smallest_unit(
    state: &TemporalPlainTimeToStringContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let smallest_unit = temporal_round_unit(&source, state.realm, &state.origin)?;
    complete_temporal_plain_time_to_string(
        state.time,
        state.precision,
        state.rounding_mode,
        Some(smallest_unit),
        state.realm,
        &state.origin,
    )
}

fn complete_temporal_plain_time_to_string(
    time: PlainTime,
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
    let rendered = match time.to_ixdtf_string(options) {
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
