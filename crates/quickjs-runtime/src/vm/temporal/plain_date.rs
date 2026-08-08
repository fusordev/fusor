use super::super::conversions::operator_primitive_to_string;
#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use core::str::FromStr;
use temporal_rs::{
    Calendar, Duration, MonthCode, PlainDate, PlainDateTime, PlainMonthDay, PlainTime,
    PlainYearMonth, TimeZone, ZonedDateTime,
    fields::CalendarFields,
    options::{
        DifferenceSettings, DisplayCalendar, Overflow, RoundingIncrement, RoundingMode, Unit,
    },
    partial::{PartialDate, PartialDateTime},
};

/// The `Temporal.PlainDate` constructor converts its ISO components in
/// source order, then converts the calendar identifier.  Keeping all values
/// here makes each user-defined primitive conversion resumable and GC-safe.
pub(in crate::vm) struct TemporalPlainDateConstructorContinuation {
    arguments: Vec<StoredValue>,
    converted: Vec<JsNumber>,
    new_target: FunctionId,
}

const TEMPORAL_PLAIN_DATE_BAG_FIELDS: [&str; 5] = ["calendar", "day", "month", "monthCode", "year"];

const TEMPORAL_PLAIN_DATE_WITH_FIELDS: [&str; 6] =
    ["calendar", "timeZone", "day", "month", "monthCode", "year"];

#[derive(Clone, Copy)]
enum TemporalPlainDateBagStage {
    ReadField,
    AwaitField,
    AwaitConversion,
}

/// Resumable `ToTemporalDate` conversion for ordinary property bags.
///
/// Each Get and conversion can invoke user code. Retaining the source bag
/// makes these operations GC-safe while preserving the required field order.
pub(in crate::vm) struct TemporalPlainDateBagContinuation {
    base: StoredValue,
    calendar: Option<Calendar>,
    day: Option<JsNumber>,
    month: Option<JsNumber>,
    month_code: Option<MonthCode>,
    year: Option<JsNumber>,
    next: usize,
    stage: TemporalPlainDateBagStage,
    target: TemporalPlainDateLikeTarget,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainDateBagContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        2
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.base, mark);
        self.target.trace_roots(mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalPlainDateToZonedDateTimeStage {
    AwaitTimeZone,
    AwaitPlainTime,
}

/// Resumable property access for `Temporal.PlainDate.prototype.toZonedDateTime`.
///
/// An object argument exposes `timeZone` first and, only when that property is
/// present and valid, `plainTime`. The original item remains rooted because a
/// missing `timeZone` is interpreted as a branded `ZonedDateTime` argument.
pub(in crate::vm) struct TemporalPlainDateToZonedDateTimeContinuation {
    date: PlainDate,
    item: StoredValue,
    time_zone: Option<TimeZone>,
    stage: TemporalPlainDateToZonedDateTimeStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainDateToZonedDateTimeContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.item, mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalPlainDateWithStage {
    ReadField,
    AwaitField,
    AwaitConversion,
}

/// Resumable `Temporal.PlainDate.prototype.with` field preparation.
///
/// Calendar and time-zone values are rejected without coercion; the date
/// fields that follow are copied only when defined and may each invoke user
/// code during their prescribed primitive conversion.
pub(in crate::vm) struct TemporalPlainDateWithContinuation {
    receiver: PlainDate,
    base: StoredValue,
    fields: CalendarFields,
    next: usize,
    stage: TemporalPlainDateWithStage,
    options: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainDateWithContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        2
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.base, mark);
        trace_stored_value_root(&self.options, mark);
    }
}

enum TemporalPlainDateLikeTarget {
    From {
        options: StoredValue,
    },
    CompareFirst {
        second: StoredValue,
    },
    CompareSecond {
        first: PlainDate,
    },
    Equals {
        receiver: PlainDate,
    },
    Difference {
        receiver: PlainDate,
        options: StoredValue,
        since: bool,
    },
}

impl TemporalPlainDateLikeTarget {
    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::From { options }
            | Self::CompareFirst { second: options }
            | Self::Difference { options, .. } => {
                trace_stored_value_root(options, mark);
            }
            Self::CompareSecond { .. } | Self::Equals { .. } => {}
        }
    }
}

pub(in crate::vm) enum TemporalPlainDateOverflowTarget {
    FromDate(PlainDate),
    FromPartial(PartialDate),
    FromMonthDay(PlainMonthDay),
    FromYearMonth(PlainYearMonth),
    FromYearMonthFields(TemporalPlainYearMonthFields),
    YearMonthWith {
        receiver: PlainYearMonth,
        fields: TemporalPlainYearMonthWithFields,
    },
    YearMonthArithmetic {
        receiver: PlainYearMonth,
        duration: Duration,
        subtract: bool,
    },
    FromMonthDayFields(TemporalPlainMonthDayFields),
    MonthDayWith {
        receiver: PlainMonthDay,
        fields: TemporalPlainMonthDayWithFields,
    },
    FromDateTime(PlainDateTime),
    FromPartialDateTime(PartialDateTime),
    DateTimeArithmetic {
        receiver: PlainDateTime,
        duration: Duration,
        subtract: bool,
    },
    ZonedDateTimeArithmetic {
        receiver: ZonedDateTime,
        duration: Duration,
        subtract: bool,
    },
    DateTimeWith {
        receiver: PlainDateTime,
        fields: TemporalPlainDateTimeWithFields,
    },
    Arithmetic {
        receiver: PlainDate,
        duration: Duration,
        subtract: bool,
    },
    With {
        receiver: PlainDate,
        fields: CalendarFields,
    },
}

#[derive(Clone, Copy)]
enum TemporalPlainDateOptionsStage {
    ReadOverflow,
    AwaitOverflow,
    AwaitOverflowConversion,
}

/// Shared `GetOptionsObject` / overflow state for `Temporal.PlainDate.from`.
pub(in crate::vm) struct TemporalPlainDateOptionsContinuation {
    target: TemporalPlainDateOverflowTarget,
    options: StoredValue,
    stage: TemporalPlainDateOptionsStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainDateOptionsContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

/// Resumable `calendarName` option state for
/// `Temporal.PlainDate.prototype.toString`.
pub(in crate::vm) struct TemporalPlainDateToStringContinuation {
    date: PlainDate,
    options: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainDateToStringContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

impl TemporalPlainDateConstructorContinuation {
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
pub(in crate::vm) fn begin_temporal_plain_date_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = inputs.new_target else {
        return temporal_type_error(realm, &origin, "Temporal.PlainDate is not callable");
    };
    let mut arguments = inputs.arguments.into_remaining_values();
    arguments.truncate(4);
    arguments
        .try_reserve(4_usize.saturating_sub(arguments.len()))
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 4_usize.saturating_sub(arguments.len()),
        })?;
    while arguments.len() < 4 {
        arguments.push(StoredValue::Undefined);
    }
    let mut converted = Vec::new();
    converted
        .try_reserve_exact(3)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 3,
        })?;
    advance_temporal_plain_date_constructor(
        runtime,
        TemporalPlainDateConstructorContinuation {
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
pub(in crate::vm) fn advance_temporal_plain_date_constructor(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateConstructorContinuation,
    completion: Option<JsNumber>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        // Each field validates right after its own conversion so a later
        // argument's user-defined conversion is never observed after an error.
        temporal_plain_date_integer(value, "component", realm, origin)?;
        state.converted.push(value);
    }
    if state.converted.len() < 3 {
        let index = state.converted.len();
        let argument = std::mem::replace(&mut state.arguments[index], StoredValue::Undefined);
        return begin_operator_primitive_conversion(
            runtime,
            argument,
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::TemporalPlainDateConstructor(Box::new(state)),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        );
    }

    let calendar = std::mem::replace(&mut state.arguments[3], StoredValue::Undefined);
    if matches!(calendar, StoredValue::Undefined) {
        return complete_temporal_plain_date_constructor(
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
            "Temporal.PlainDate calendar must be a string",
        );
    };
    // ToTemporalCalendarIdentifier: a bare calendar identifier only.
    let calendar = match Calendar::try_from_utf8(value.to_utf8_lossy()?.as_bytes()) {
        Ok(calendar) => calendar,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    complete_temporal_plain_date_constructor(
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
fn complete_temporal_plain_date_constructor(
    runtime: &mut Runtime,
    state: &TemporalPlainDateConstructorContinuation,
    calendar: Calendar,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let [year, month, day] = state.converted.as_slice() else {
        return Err(EngineFault::RuntimeInvariant {
            message: "Temporal.PlainDate constructor completed before all components converted",
        }
        .into());
    };
    let year = temporal_plain_date_integer(*year, "year", realm, origin)?;
    let month = temporal_plain_date_integer(*month, "month", realm, origin)?;
    let day = temporal_plain_date_integer(*day, "day", realm, origin)?;
    let Ok(year) = i32::try_from(year) else {
        return temporal_range_error(
            realm,
            origin,
            "Temporal.PlainDate year is outside the supported range",
        );
    };
    let Ok(month) = u8::try_from(month) else {
        return temporal_range_error(
            realm,
            origin,
            "Temporal.PlainDate month is outside the supported range",
        );
    };
    let Ok(day) = u8::try_from(day) else {
        return temporal_range_error(
            realm,
            origin,
            "Temporal.PlainDate day is outside the supported range",
        );
    };
    let date = match PlainDate::try_new(year, month, day, calendar) {
        Ok(date) => date,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    begin_temporal_plain_date_wrapper(
        runtime,
        realm,
        state.new_target,
        date,
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
pub(in crate::vm) fn temporal_plain_date_integer(
    value: JsNumber,
    _field: &str,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<i64, NativeFailure> {
    let value = value.as_f64();
    if !value.is_finite() {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "Temporal.PlainDate fields must be finite Numbers",
        )?));
    }
    let value = value.trunc();
    if value < i64::MIN as f64 || value > i64::MAX as f64 {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "Temporal.PlainDate fields are outside the supported range",
        )?));
    }
    Ok(value as i64)
}

/// `ToPositiveIntegerWithTruncation` for month and day fields: zero and
/// negative values are a RangeError, while values beyond the kernel's `u8`
/// domain saturate and leave constrain-or-reject resolution to the calendar.
fn temporal_plain_date_positive_u8(
    value: i64,
    field: &str,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<u8, NativeFailure> {
    if value < 1 {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            match field {
                "month" => "Temporal.PlainDate month must be a positive integer",
                _ => "Temporal.PlainDate day must be a positive integer",
            },
        )?));
    }
    Ok(u8::try_from(value).unwrap_or(u8::MAX))
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "newTarget prototype lookup is a resumable native operation"
)]
fn begin_temporal_plain_date_wrapper(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: FunctionId,
    date: PlainDate,
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
        IntrinsicGetContinuation::TemporalPlainDateConstructor { new_target, date },
        return_to,
        Some(origin),
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_date_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    date: PlainDate,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        _ => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_temporal_plain_date_prototype(realm)?)
        }
    };
    let object = runtime.allocate_temporal_plain_date(prototype, date)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "Temporal.PlainDate.from preserves conversion and allocation context"
)]
pub(in crate::vm) fn begin_temporal_plain_date_static(
    runtime: &mut Runtime,
    method: TemporalPlainDateStaticMethod,
    realm: RealmId,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        TemporalPlainDateStaticMethod::From => {
            let value = arguments.take_first_or_undefined();
            let options = arguments.take_first_or_undefined();
            begin_temporal_plain_date_from(
                runtime,
                value,
                options,
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateStaticMethod::Compare => {
            let first = arguments.take_first_or_undefined();
            let second = arguments.take_first_or_undefined();
            begin_temporal_plain_date_like(
                runtime,
                first,
                TemporalPlainDateLikeTarget::CompareFirst { second },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "Temporal.PlainDate.from retains the standard native call shape"
)]
fn begin_temporal_plain_date_from(
    runtime: &mut Runtime,
    value: StoredValue,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_temporal_plain_date_like(
        runtime,
        value,
        TemporalPlainDateLikeTarget::From { options },
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "all accepted Temporal.PlainDate inputs share a resumable conversion boundary"
)]
fn begin_temporal_plain_date_like(
    runtime: &mut Runtime,
    value: StoredValue,
    target: TemporalPlainDateLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(object) = value {
        // ToTemporalDate slot fast paths: branded Temporal values contribute
        // their date slots without any observable property reads.
        if let Some(date) = runtime.temporal_plain_date(object)? {
            return continue_temporal_plain_date_like(
                runtime,
                date,
                target,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        if let Some(date_time) = runtime.temporal_plain_date_time(object)? {
            return continue_temporal_plain_date_like(
                runtime,
                date_time.to_plain_date(),
                target,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        if let Some(date_time) = runtime.temporal_zoned_date_time(object)? {
            return continue_temporal_plain_date_like(
                runtime,
                date_time.to_plain_date(),
                target,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
    }
    if let StoredValue::String(value) = value {
        let date = temporal_plain_date_from_string(realm, &value, &origin)?;
        return continue_temporal_plain_date_like(
            runtime,
            date,
            target,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if value.heap_reference().is_some() {
        return advance_temporal_plain_date_property_bag(
            runtime,
            TemporalPlainDateBagContinuation {
                base: value,
                calendar: None,
                day: None,
                month: None,
                month_code: None,
                year: None,
                next: 0,
                stage: TemporalPlainDateBagStage::ReadField,
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
        "Temporal.PlainDate requires a PlainDate, ISO string, or property bag",
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the conversion target carries its own native call context"
)]
fn continue_temporal_plain_date_like(
    runtime: &mut Runtime,
    date: PlainDate,
    target: TemporalPlainDateLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match target {
        TemporalPlainDateLikeTarget::From { options } => begin_temporal_plain_date_from_options(
            runtime,
            TemporalPlainDateOverflowTarget::FromDate(date),
            options,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        TemporalPlainDateLikeTarget::CompareFirst { second } => begin_temporal_plain_date_like(
            runtime,
            second,
            TemporalPlainDateLikeTarget::CompareSecond { first: date },
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        TemporalPlainDateLikeTarget::CompareSecond { first } => {
            let result = match first.compare_iso(&date) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            Ok(NativeDispatch::Immediate(StoredValue::Number(
                JsNumber::from_i32(result),
            )))
        }
        TemporalPlainDateLikeTarget::Equals { receiver } => Ok(NativeDispatch::Immediate(
            StoredValue::Boolean(receiver == date),
        )),
        TemporalPlainDateLikeTarget::Difference {
            receiver,
            options,
            since,
        } => begin_temporal_plain_date_difference(
            runtime,
            receiver,
            date,
            options,
            since,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
    }
}

fn temporal_plain_date_from_string(
    realm: RealmId,
    value: &JsString,
    origin: &JsStackFrame,
) -> Result<PlainDate, NativeFailure> {
    let source = value.to_utf8_lossy()?;
    match PlainDate::from_utf8(source.as_bytes()) {
        Ok(date) => Ok(date),
        Err(error) => Err(NativeFailure::Abrupt(temporal_exception_from_error(
            realm, origin, error,
        )?)),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the shared Temporal option reader keeps native call state explicit"
)]
pub(in crate::vm) fn begin_temporal_plain_date_from_options(
    runtime: &mut Runtime,
    target: TemporalPlainDateOverflowTarget,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return finish_temporal_plain_date_from_options(
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
            "Temporal.PlainDate.from options must be an object",
        );
    }
    advance_temporal_plain_date_from_options(
        runtime,
        TemporalPlainDateOptionsContinuation {
            target,
            options,
            stage: TemporalPlainDateOptionsStage::ReadOverflow,
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
    reason = "the explicit state machine preserves the from-options Get and ToString order"
)]
pub(in crate::vm) fn advance_temporal_plain_date_from_options(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateOptionsContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalPlainDateOptionsStage::ReadOverflow => {
                charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
                let name = JsString::from_utf8("overflow")?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalPlainDateOptionsStage::AwaitOverflow;
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
                    temporal_plain_date_options_continuation,
                    "Temporal.PlainDate.from overflow Get produced a structured result",
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
            TemporalPlainDateOptionsStage::AwaitOverflow => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainDate.from overflow Get resumed without a value",
                })?;
                if matches!(value, StoredValue::Undefined) {
                    return finish_temporal_plain_date_from_options(
                        runtime,
                        state.target,
                        Overflow::Constrain,
                        state.realm,
                        &state.origin,
                    );
                }
                state.stage = TemporalPlainDateOptionsStage::AwaitOverflowConversion;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::TemporalPlainDateOptions(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalPlainDateOptionsStage::AwaitOverflowConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainDate.from overflow conversion resumed without a value",
                })?;
                let value = operator_primitive_to_string(value, state.realm, &state.origin)?;
                let Ok(overflow) = Overflow::from_str(&value.to_utf8_lossy()?) else {
                    return temporal_range_error(
                        state.realm,
                        &state.origin,
                        "Temporal.PlainDate.from overflow must be constrain or reject",
                    );
                };
                return finish_temporal_plain_date_from_options(
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

fn temporal_plain_date_options_continuation(
    state: TemporalPlainDateOptionsContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainDateOptions(Box::new(state))
}

#[allow(
    clippy::too_many_lines,
    reason = "the shared ordered overflow reader completes PlainDate, PlainDateTime, and PlainMonthDay targets"
)]
fn finish_temporal_plain_date_from_options(
    runtime: &mut Runtime,
    target: TemporalPlainDateOverflowTarget,
    overflow: Overflow,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    match target {
        TemporalPlainDateOverflowTarget::YearMonthWith { receiver, fields } => {
            let fields = temporal_plain_year_month_with_fields(&fields, overflow, realm, origin)?;
            let year_month = match receiver.with(fields, Some(overflow)) {
                Ok(year_month) => year_month,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            return allocate_temporal_plain_year_month_result(runtime, realm, year_month);
        }
        TemporalPlainDateOverflowTarget::YearMonthArithmetic {
            receiver,
            duration,
            subtract,
        } => {
            let result = if subtract {
                receiver.subtract(&duration, overflow)
            } else {
                receiver.add(&duration, overflow)
            };
            let year_month = match result {
                Ok(year_month) => year_month,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            return allocate_temporal_plain_year_month_result(runtime, realm, year_month);
        }
        TemporalPlainDateOverflowTarget::MonthDayWith { receiver, fields } => {
            let fields = temporal_plain_month_day_with_fields(&fields, overflow, realm, origin)?;
            let month_day = match receiver.with(fields, Some(overflow)) {
                Ok(month_day) => month_day,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            return allocate_temporal_plain_month_day_result(runtime, realm, month_day);
        }
        TemporalPlainDateOverflowTarget::FromMonthDay(month_day) => {
            return allocate_temporal_plain_month_day_result(runtime, realm, month_day);
        }
        TemporalPlainDateOverflowTarget::FromYearMonth(year_month) => {
            return allocate_temporal_plain_year_month_result(runtime, realm, year_month);
        }
        TemporalPlainDateOverflowTarget::FromYearMonthFields(fields) => {
            let year_month =
                temporal_plain_year_month_from_fields(&fields, overflow, realm, origin)?;
            return allocate_temporal_plain_year_month_result(runtime, realm, year_month);
        }
        TemporalPlainDateOverflowTarget::FromMonthDayFields(fields) => {
            let month_day = temporal_plain_month_day_from_fields(&fields, overflow, realm, origin)?;
            return allocate_temporal_plain_month_day_result(runtime, realm, month_day);
        }
        TemporalPlainDateOverflowTarget::FromDateTime(date_time) => {
            return allocate_temporal_plain_date_time_result(runtime, realm, date_time);
        }
        TemporalPlainDateOverflowTarget::FromPartialDateTime(partial) => {
            let date_time = match PlainDateTime::from_partial(partial, Some(overflow)) {
                Ok(date_time) => date_time,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            return allocate_temporal_plain_date_time_result(runtime, realm, date_time);
        }
        TemporalPlainDateOverflowTarget::DateTimeArithmetic {
            receiver,
            duration,
            subtract,
        } => {
            let result = if subtract {
                receiver.subtract(&duration, Some(overflow))
            } else {
                receiver.add(&duration, Some(overflow))
            };
            let date_time = match result {
                Ok(date_time) => date_time,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            return allocate_temporal_plain_date_time_result(runtime, realm, date_time);
        }
        TemporalPlainDateOverflowTarget::ZonedDateTimeArithmetic {
            receiver,
            duration,
            subtract,
        } => {
            let result = if subtract {
                receiver.subtract(&duration, Some(overflow))
            } else {
                receiver.add(&duration, Some(overflow))
            };
            let date_time = match result {
                Ok(date_time) => date_time,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            return allocate_temporal_zoned_date_time_result(runtime, realm, date_time);
        }
        TemporalPlainDateOverflowTarget::DateTimeWith { receiver, fields } => {
            let fields = temporal_plain_date_time_with_fields(&fields, realm, origin)?;
            let date_time = match receiver.with(fields, Some(overflow)) {
                Ok(date_time) => date_time,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            return allocate_temporal_plain_date_time_result(runtime, realm, date_time);
        }
        _ => {}
    }
    let date = match target {
        TemporalPlainDateOverflowTarget::FromDate(date) => date,
        TemporalPlainDateOverflowTarget::FromPartial(partial) => {
            match PlainDate::from_partial(partial, Some(overflow)) {
                Ok(date) => date,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            }
        }
        TemporalPlainDateOverflowTarget::Arithmetic {
            receiver,
            duration,
            subtract,
        } => {
            let result = if subtract {
                receiver.subtract(&duration, Some(overflow))
            } else {
                receiver.add(&duration, Some(overflow))
            };
            match result {
                Ok(date) => date,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            }
        }
        TemporalPlainDateOverflowTarget::With { receiver, fields } => {
            match receiver.with(fields, Some(overflow)) {
                Ok(date) => date,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            }
        }
        TemporalPlainDateOverflowTarget::FromDateTime(_)
        | TemporalPlainDateOverflowTarget::FromPartialDateTime(_)
        | TemporalPlainDateOverflowTarget::FromMonthDay(_)
        | TemporalPlainDateOverflowTarget::FromYearMonth(_)
        | TemporalPlainDateOverflowTarget::FromYearMonthFields(_)
        | TemporalPlainDateOverflowTarget::YearMonthWith { .. }
        | TemporalPlainDateOverflowTarget::YearMonthArithmetic { .. }
        | TemporalPlainDateOverflowTarget::FromMonthDayFields(_)
        | TemporalPlainDateOverflowTarget::MonthDayWith { .. }
        | TemporalPlainDateOverflowTarget::DateTimeArithmetic { .. }
        | TemporalPlainDateOverflowTarget::ZonedDateTimeArithmetic { .. }
        | TemporalPlainDateOverflowTarget::DateTimeWith { .. } => {
            unreachable!("Temporal PlainDateTime overflow targets return above")
        }
    };
    allocate_temporal_plain_date_result(runtime, realm, date)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the state machine retains the property bag across observable Gets and conversions"
)]
pub(in crate::vm) fn advance_temporal_plain_date_property_bag(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateBagContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalPlainDateBagStage::ReadField => {
                if state.next == TEMPORAL_PLAIN_DATE_BAG_FIELDS.len() {
                    let partial = temporal_plain_date_partial_from_bag(&state)?;
                    return match state.target {
                        TemporalPlainDateLikeTarget::From { options } => {
                            begin_temporal_plain_date_from_options(
                                runtime,
                                TemporalPlainDateOverflowTarget::FromPartial(partial),
                                options,
                                state.realm,
                                return_to,
                                state.origin,
                                execution_budget,
                            )
                        }
                        target => {
                            let date =
                                match PlainDate::from_partial(partial, Some(Overflow::Constrain)) {
                                    Ok(date) => date,
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
                            continue_temporal_plain_date_like(
                                runtime,
                                date,
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
                let name = JsString::from_utf8(TEMPORAL_PLAIN_DATE_BAG_FIELDS[state.next])?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalPlainDateBagStage::AwaitField;
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
                    temporal_plain_date_bag_continuation,
                    "Temporal.PlainDate property bag Get produced a structured result",
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
            TemporalPlainDateBagStage::AwaitField => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainDate property bag Get resumed without a value",
                })?;
                if matches!(value, StoredValue::Undefined) {
                    if TEMPORAL_PLAIN_DATE_BAG_FIELDS[state.next] == "calendar" {
                        state.calendar = Some(Calendar::default());
                    }
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainDateBagStage::ReadField;
                    continue;
                }
                if TEMPORAL_PLAIN_DATE_BAG_FIELDS[state.next] == "calendar" {
                    // ToTemporalCalendarSlotValue: branded Temporal objects
                    // contribute their calendar slot; strings parse as a
                    // calendar string; anything else is a TypeError.
                    let calendar = match value {
                        StoredValue::String(value) => {
                            match Calendar::from_str(&value.to_utf8_lossy()?) {
                                Ok(calendar) => calendar,
                                Err(error) => {
                                    return Err(NativeFailure::Abrupt(
                                        temporal_exception_from_error(
                                            state.realm,
                                            &state.origin,
                                            error,
                                        )?,
                                    ));
                                }
                            }
                        }
                        StoredValue::Object(_) => {
                            match temporal_calendar_from_object(runtime, &value)? {
                                Some(calendar) => calendar,
                                None => {
                                    return temporal_type_error(
                                        state.realm,
                                        &state.origin,
                                        "Temporal.PlainDate calendar must be a calendar identifier or Temporal object",
                                    );
                                }
                            }
                        }
                        _ => {
                            return temporal_type_error(
                                state.realm,
                                &state.origin,
                                "Temporal.PlainDate calendar must be a calendar identifier or Temporal object",
                            );
                        }
                    };
                    state.calendar = Some(calendar);
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainDateBagStage::ReadField;
                    continue;
                }
                state.stage = TemporalPlainDateBagStage::AwaitConversion;
                let hint = match TEMPORAL_PLAIN_DATE_BAG_FIELDS[state.next] {
                    "monthCode" => OperatorPrimitiveHint::String,
                    "day" | "month" | "year" => OperatorPrimitiveHint::Number,
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "unknown Temporal.PlainDate property bag field",
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
                    OperatorPrimitiveTarget::TemporalPlainDateBag(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalPlainDateBagStage::AwaitConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainDate property bag conversion resumed without a value",
                })?;
                match TEMPORAL_PLAIN_DATE_BAG_FIELDS[state.next] {
                    "monthCode" => {
                        let StoredValue::String(value) = value else {
                            return temporal_type_error(
                                state.realm,
                                &state.origin,
                                "Temporal.PlainDate monthCode must be a string",
                            );
                        };
                        let month_code = match MonthCode::from_str(&value.to_utf8_lossy()?) {
                            Ok(month_code) => month_code,
                            Err(error) => {
                                return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                                    state.realm,
                                    &state.origin,
                                    error,
                                )?));
                            }
                        };
                        state.month_code = Some(month_code);
                    }
                    "day" => {
                        state.day = Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    "month" => {
                        state.month = Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    "year" => {
                        state.year = Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "unknown Temporal.PlainDate property bag field",
                        }
                        .into());
                    }
                }
                state.next = state.next.saturating_add(1);
                state.stage = TemporalPlainDateBagStage::ReadField;
            }
        }
    }
}

fn temporal_plain_date_bag_continuation(
    state: TemporalPlainDateBagContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainDateBag(Box::new(state))
}

fn temporal_plain_date_partial_from_bag(
    state: &TemporalPlainDateBagContinuation,
) -> Result<PartialDate, NativeFailure> {
    let year = temporal_plain_date_required_field(state.year, "year", state.realm, &state.origin)?;
    let day = temporal_plain_date_required_field(state.day, "day", state.realm, &state.origin)?;
    let month = state
        .month
        .map(|value| temporal_plain_date_integer(value, "month", state.realm, &state.origin))
        .transpose()?;
    let Ok(year) = i32::try_from(year) else {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            state.realm,
            &state.origin,
            ExceptionKind::RangeError,
            "Temporal.PlainDate year is outside the supported range",
        )?));
    };
    let month = match month {
        Some(month) => Some(temporal_plain_date_positive_u8(
            month,
            "month",
            state.realm,
            &state.origin,
        )?),
        None => None,
    };
    let day = temporal_plain_date_positive_u8(day, "day", state.realm, &state.origin)?;
    Ok(PartialDate::new()
        .with_calendar(state.calendar.clone().unwrap_or_default())
        .with_year(Some(year))
        .with_month(month)
        .with_month_code(state.month_code)
        .with_day(Some(day)))
}

pub(in crate::vm) fn temporal_plain_date_required_field(
    value: Option<JsNumber>,
    field: &str,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<i64, NativeFailure> {
    let Some(value) = value else {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Temporal.PlainDate property bag is missing a required field",
        )?));
    };
    temporal_plain_date_integer(value, field, realm, origin)
}

fn finish_temporal_plain_date_with_calendar(
    runtime: &mut Runtime,
    date: &PlainDate,
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
            if let Some(other) = runtime.temporal_plain_date(object)? {
                other.calendar().clone()
            } else if let Some(other) = runtime.temporal_plain_date_time(object)? {
                other.calendar().clone()
            } else {
                return temporal_type_error(
                    realm,
                    origin,
                    "Temporal.PlainDate.withCalendar requires a calendar identifier or Temporal object",
                );
            }
        }
        _ => {
            return temporal_type_error(
                realm,
                origin,
                "Temporal.PlainDate.withCalendar requires a calendar identifier or Temporal object",
            );
        }
    };
    allocate_temporal_plain_date_result(runtime, realm, date.with_calendar(calendar))
}

pub(in crate::vm) fn finish_temporal_plain_date_to_plain_date_time(
    runtime: &mut Runtime,
    date: &PlainDate,
    time: Option<PlainTime>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let date_time = match date.to_plain_date_time(time) {
        Ok(date_time) => date_time,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    allocate_temporal_plain_date_time_result(runtime, realm, date_time)
}

pub(in crate::vm) fn finish_temporal_plain_date_to_zoned_date_time(
    runtime: &mut Runtime,
    date: &PlainDate,
    time_zone: TimeZone,
    plain_time: Option<PlainTime>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let date_time = match date.to_zoned_date_time(time_zone, plain_time) {
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
    reason = "the observable timeZone Get retains the complete native call context"
)]
fn begin_temporal_plain_date_to_zoned_date_time(
    runtime: &mut Runtime,
    date: PlainDate,
    item: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if item.heap_reference().is_none() {
        let time_zone =
            temporal_zoned_date_time_time_zone_from_value(runtime, item, realm, &origin)?;
        return finish_temporal_plain_date_to_zoned_date_time(
            runtime, &date, time_zone, None, realm, &origin,
        );
    }
    charge_heap_property_lookup(runtime, &item, execution_budget)?;
    let name = JsString::from_utf8("timeZone")?;
    let key = runtime.property_key_from_string(&name)?;
    let state = TemporalPlainDateToZonedDateTimeContinuation {
        date,
        item,
        time_zone: None,
        stage: TemporalPlainDateToZonedDateTimeStage::AwaitTimeZone,
        realm,
        origin,
    };
    let dispatch = begin_value_get(
        runtime,
        &state.item,
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
        temporal_plain_date_to_zoned_date_time_continuation,
        "Temporal.PlainDate toZonedDateTime timeZone Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_plain_date_to_zoned_date_time(
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
    reason = "the plainTime Get resumes with its retained date and time zone"
)]
pub(in crate::vm) fn advance_temporal_plain_date_to_zoned_date_time(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateToZonedDateTimeContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalPlainDateToZonedDateTimeStage::AwaitTimeZone => {
            if matches!(value, StoredValue::Undefined) {
                let time_zone = temporal_zoned_date_time_time_zone_from_value(
                    runtime,
                    state.item,
                    state.realm,
                    &state.origin,
                )?;
                return finish_temporal_plain_date_to_zoned_date_time(
                    runtime,
                    &state.date,
                    time_zone,
                    None,
                    state.realm,
                    &state.origin,
                );
            }
            state.time_zone = Some(temporal_zoned_date_time_time_zone_from_value(
                runtime,
                value,
                state.realm,
                &state.origin,
            )?);
            charge_heap_property_lookup(runtime, &state.item, execution_budget)?;
            let name = JsString::from_utf8("plainTime")?;
            let key = runtime.property_key_from_string(&name)?;
            state.stage = TemporalPlainDateToZonedDateTimeStage::AwaitPlainTime;
            let dispatch = begin_value_get(
                runtime,
                &state.item,
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
                temporal_plain_date_to_zoned_date_time_continuation,
                "Temporal.PlainDate toZonedDateTime plainTime Get produced a structured result",
            )? {
                GetContinuationDispatch::Ready { state, value } => {
                    advance_temporal_plain_date_to_zoned_date_time(
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
        TemporalPlainDateToZonedDateTimeStage::AwaitPlainTime => {
            let Some(time_zone) = state.time_zone else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainDate toZonedDateTime lost its time zone",
                }
                .into());
            };
            if matches!(value, StoredValue::Undefined) {
                return finish_temporal_plain_date_to_zoned_date_time(
                    runtime,
                    &state.date,
                    time_zone,
                    None,
                    state.realm,
                    &state.origin,
                );
            }
            begin_temporal_plain_time_like(
                runtime,
                value,
                TemporalPlainTimeLikeTarget::PlainDateToZonedDateTime {
                    receiver: state.date,
                    time_zone,
                },
                state.realm,
                return_to,
                state.origin,
                execution_budget,
            )
        }
    }
}

fn temporal_plain_date_to_zoned_date_time_continuation(
    state: TemporalPlainDateToZonedDateTimeContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainDateToZonedDateTime(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    reason = "ToTemporalTime property bags retain every observable conversion across suspension"
)]
fn begin_temporal_plain_date_to_plain_date_time(
    runtime: &mut Runtime,
    date: PlainDate,
    value: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(value, StoredValue::Undefined) {
        return finish_temporal_plain_date_to_plain_date_time(runtime, &date, None, realm, &origin);
    }
    if let StoredValue::Object(object) = value
        && let Some(date_time) = runtime.temporal_plain_date_time(object)?
    {
        return finish_temporal_plain_date_to_plain_date_time(
            runtime,
            &date,
            Some(PlainTime::from(date_time)),
            realm,
            &origin,
        );
    }
    if let StoredValue::Object(object) = value
        && let Some(time) = runtime.temporal_plain_time(object)?
    {
        return finish_temporal_plain_date_to_plain_date_time(
            runtime,
            &date,
            Some(time),
            realm,
            &origin,
        );
    }
    if let StoredValue::String(source) = &value {
        let time = match PlainTime::from_str(&source.to_utf8_lossy()?) {
            Ok(time) => time,
            Err(error) => {
                return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                    realm, &origin, error,
                )?));
            }
        };
        return finish_temporal_plain_date_to_plain_date_time(
            runtime,
            &date,
            Some(time),
            realm,
            &origin,
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
                target: TemporalPlainDateTimeLikeTarget::FromPlainDate { receiver: date },
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
        "Temporal.PlainDate.toPlainDateTime requires a PlainDateTime, string, or property bag",
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the native method owns its field/options values across observable conversion"
)]
fn begin_temporal_plain_date_with(
    runtime: &mut Runtime,
    receiver: PlainDate,
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
            "Temporal.PlainDate.with requires a property bag",
        );
    }
    // RejectObjectWithCalendarOrTimeZone: branded Temporal objects are
    // rejected from their internal slots before any property is observed.
    if let StoredValue::Object(object) = fields
        && (runtime.temporal_plain_date(object)?.is_some()
            || runtime.temporal_plain_date_time(object)?.is_some()
            || runtime.temporal_plain_time(object)?.is_some()
            || runtime.temporal_plain_month_day(object)?.is_some()
            || runtime.temporal_plain_year_month(object)?.is_some()
            || runtime.temporal_zoned_date_time(object)?.is_some())
    {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.PlainDate.with cannot be called with a Temporal object",
        );
    }
    advance_temporal_plain_date_with(
        runtime,
        TemporalPlainDateWithContinuation {
            receiver,
            base: fields,
            fields: CalendarFields::new(),
            next: 0,
            stage: TemporalPlainDateWithStage::ReadField,
            options,
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
    reason = "the explicit state machine preserves with-field Get and conversion order"
)]
pub(in crate::vm) fn advance_temporal_plain_date_with(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateWithContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalPlainDateWithStage::ReadField => {
                if state.next == TEMPORAL_PLAIN_DATE_WITH_FIELDS.len() {
                    return begin_temporal_plain_date_from_options(
                        runtime,
                        TemporalPlainDateOverflowTarget::With {
                            receiver: state.receiver,
                            fields: state.fields,
                        },
                        state.options,
                        state.realm,
                        return_to,
                        state.origin,
                        execution_budget,
                    );
                }
                charge_heap_property_lookup(runtime, &state.base, execution_budget)?;
                let name = JsString::from_utf8(TEMPORAL_PLAIN_DATE_WITH_FIELDS[state.next])?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalPlainDateWithStage::AwaitField;
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
                    temporal_plain_date_with_continuation,
                    "Temporal.PlainDate.with field Get produced a structured result",
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
            TemporalPlainDateWithStage::AwaitField => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainDate.with field Get resumed without a value",
                })?;
                let field = TEMPORAL_PLAIN_DATE_WITH_FIELDS[state.next];
                if matches!(field, "calendar" | "timeZone") {
                    if !matches!(value, StoredValue::Undefined) {
                        return temporal_type_error(
                            state.realm,
                            &state.origin,
                            "Temporal.PlainDate.with cannot override calendar or timeZone",
                        );
                    }
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainDateWithStage::ReadField;
                    continue;
                }
                if matches!(value, StoredValue::Undefined) {
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainDateWithStage::ReadField;
                    continue;
                }
                state.stage = TemporalPlainDateWithStage::AwaitConversion;
                let hint = match field {
                    "monthCode" => OperatorPrimitiveHint::String,
                    "day" | "month" | "year" => OperatorPrimitiveHint::Number,
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "unknown Temporal.PlainDate.with field",
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
                    OperatorPrimitiveTarget::TemporalPlainDateWith(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalPlainDateWithStage::AwaitConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainDate.with field conversion resumed without a value",
                })?;
                match TEMPORAL_PLAIN_DATE_WITH_FIELDS[state.next] {
                    "year" => {
                        let value = temporal_plain_date_integer(
                            operator_to_number(value, state.realm, &state.origin)?,
                            "year",
                            state.realm,
                            &state.origin,
                        )?;
                        let Ok(value) = i32::try_from(value) else {
                            return temporal_range_error(
                                state.realm,
                                &state.origin,
                                "Temporal.PlainDate year is outside the supported range",
                            );
                        };
                        state.fields.year = Some(value);
                    }
                    "month" => {
                        let value = temporal_plain_date_integer(
                            operator_to_number(value, state.realm, &state.origin)?,
                            "month",
                            state.realm,
                            &state.origin,
                        )?;
                        state.fields.month = Some(temporal_plain_date_positive_u8(
                            value,
                            "month",
                            state.realm,
                            &state.origin,
                        )?);
                    }
                    "day" => {
                        let value = temporal_plain_date_integer(
                            operator_to_number(value, state.realm, &state.origin)?,
                            "day",
                            state.realm,
                            &state.origin,
                        )?;
                        state.fields.day = Some(temporal_plain_date_positive_u8(
                            value,
                            "day",
                            state.realm,
                            &state.origin,
                        )?);
                    }
                    "monthCode" => {
                        let StoredValue::String(value) = value else {
                            return temporal_type_error(
                                state.realm,
                                &state.origin,
                                "Temporal.PlainDate monthCode must be a string",
                            );
                        };
                        let month_code = match MonthCode::from_str(&value.to_utf8_lossy()?) {
                            Ok(month_code) => month_code,
                            Err(error) => {
                                return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                                    state.realm,
                                    &state.origin,
                                    error,
                                )?));
                            }
                        };
                        state.fields.month_code = Some(month_code);
                    }
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "unknown Temporal.PlainDate.with field",
                        }
                        .into());
                    }
                }
                state.next = state.next.saturating_add(1);
                state.stage = TemporalPlainDateWithStage::ReadField;
            }
        }
    }
}

fn temporal_plain_date_with_continuation(
    state: TemporalPlainDateWithContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainDateWith(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the shared dispatcher carries the native call context explicitly"
)]
pub(in crate::vm) fn dispatch_temporal_plain_date_prototype(
    runtime: &mut Runtime,
    method: TemporalPlainDatePrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let date = require_temporal_plain_date(runtime, receiver, realm, origin)?;
    let number = |value| NativeDispatch::Immediate(StoredValue::Number(JsNumber::from_i64(value)));
    match method {
        TemporalPlainDatePrototypeMethod::CalendarId => Ok(NativeDispatch::Immediate(
            StoredValue::String(JsString::from_utf8(date.calendar().identifier())?),
        )),
        TemporalPlainDatePrototypeMethod::Year => Ok(number(i64::from(date.year()))),
        TemporalPlainDatePrototypeMethod::Month => Ok(number(i64::from(date.month()))),
        TemporalPlainDatePrototypeMethod::MonthCode => Ok(NativeDispatch::Immediate(
            StoredValue::String(JsString::from_utf8(date.month_code().as_str())?),
        )),
        TemporalPlainDatePrototypeMethod::Day => Ok(number(i64::from(date.day()))),
        TemporalPlainDatePrototypeMethod::DayOfWeek => Ok(number(i64::from(date.day_of_week()))),
        TemporalPlainDatePrototypeMethod::DayOfYear => Ok(number(i64::from(date.day_of_year()))),
        TemporalPlainDatePrototypeMethod::WeekOfYear => Ok(match date.week_of_year() {
            Some(value) => number(i64::from(value)),
            None => NativeDispatch::Immediate(StoredValue::Undefined),
        }),
        TemporalPlainDatePrototypeMethod::YearOfWeek => Ok(match date.year_of_week() {
            Some(value) => number(i64::from(value)),
            None => NativeDispatch::Immediate(StoredValue::Undefined),
        }),
        TemporalPlainDatePrototypeMethod::DaysInWeek => Ok(number(i64::from(date.days_in_week()))),
        TemporalPlainDatePrototypeMethod::DaysInMonth => {
            Ok(number(i64::from(date.days_in_month())))
        }
        TemporalPlainDatePrototypeMethod::DaysInYear => Ok(number(i64::from(date.days_in_year()))),
        TemporalPlainDatePrototypeMethod::MonthsInYear => {
            Ok(number(i64::from(date.months_in_year())))
        }
        TemporalPlainDatePrototypeMethod::InLeapYear => Ok(NativeDispatch::Immediate(
            StoredValue::Boolean(date.in_leap_year()),
        )),
        TemporalPlainDatePrototypeMethod::Era => Ok(match date.era() {
            Some(value) => {
                NativeDispatch::Immediate(StoredValue::String(JsString::from_utf8(value.as_str())?))
            }
            None => NativeDispatch::Immediate(StoredValue::Undefined),
        }),
        TemporalPlainDatePrototypeMethod::EraYear => Ok(match date.era_year() {
            Some(value) => number(i64::from(value)),
            None => NativeDispatch::Immediate(StoredValue::Undefined),
        }),
        TemporalPlainDatePrototypeMethod::Add | TemporalPlainDatePrototypeMethod::Subtract => {
            let duration = arguments.take_first_or_undefined();
            let options = arguments.take_first_or_undefined();
            begin_temporal_duration_like(
                runtime,
                duration,
                TemporalDurationLikeTarget::PlainDateArithmetic {
                    receiver: date,
                    subtract: matches!(method, TemporalPlainDatePrototypeMethod::Subtract),
                    options,
                },
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalPlainDatePrototypeMethod::With => begin_temporal_plain_date_with(
            runtime,
            date,
            arguments.take_first_or_undefined(),
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainDatePrototypeMethod::ToPlainDateTime => {
            begin_temporal_plain_date_to_plain_date_time(
                runtime,
                date,
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalPlainDatePrototypeMethod::ToPlainMonthDay => {
            let month_day = match date.to_plain_month_day() {
                Ok(month_day) => month_day,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            allocate_temporal_plain_month_day_result(runtime, realm, month_day)
        }
        TemporalPlainDatePrototypeMethod::ToPlainYearMonth => {
            let year_month = match date.to_plain_year_month() {
                Ok(year_month) => year_month,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            allocate_temporal_plain_year_month_result(runtime, realm, year_month)
        }
        TemporalPlainDatePrototypeMethod::ToZonedDateTime => {
            begin_temporal_plain_date_to_zoned_date_time(
                runtime,
                date,
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalPlainDatePrototypeMethod::WithCalendar => {
            let calendar = arguments.take_first_or_undefined();
            finish_temporal_plain_date_with_calendar(runtime, &date, calendar, realm, origin)
        }
        TemporalPlainDatePrototypeMethod::Until | TemporalPlainDatePrototypeMethod::Since => {
            let other = arguments.take_first_or_undefined();
            let options = arguments.take_first_or_undefined();
            begin_temporal_plain_date_like(
                runtime,
                other,
                TemporalPlainDateLikeTarget::Difference {
                    receiver: date,
                    options,
                    since: matches!(method, TemporalPlainDatePrototypeMethod::Since),
                },
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalPlainDatePrototypeMethod::Equals => begin_temporal_plain_date_like(
            runtime,
            arguments.take_first_or_undefined(),
            TemporalPlainDateLikeTarget::Equals { receiver: date },
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainDatePrototypeMethod::ToString => begin_temporal_plain_date_to_string(
            runtime,
            date,
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainDatePrototypeMethod::ToJson
        | TemporalPlainDatePrototypeMethod::ToLocaleString => {
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&date.to_ixdtf_string(DisplayCalendar::Auto))?,
            )))
        }
        TemporalPlainDatePrototypeMethod::ValueOf => temporal_type_error(
            realm,
            origin,
            "Temporal.PlainDate cannot be converted to a primitive value",
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the ordered PlainDate calendar formatting reader owns its resumable native call context"
)]
fn begin_temporal_plain_date_to_string(
    runtime: &mut Runtime,
    date: PlainDate,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return complete_temporal_plain_date_to_string(&date, DisplayCalendar::Auto);
    }
    if options.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.PlainDate.prototype.toString options must be an object",
        );
    }
    begin_temporal_plain_date_to_string_get(
        runtime,
        TemporalPlainDateToStringContinuation {
            date,
            options,
            realm,
            origin,
        },
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the observable PlainDate calendarName Get retains native call state"
)]
fn begin_temporal_plain_date_to_string_get(
    runtime: &mut Runtime,
    state: TemporalPlainDateToStringContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
    let name = JsString::from_utf8("calendarName")?;
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
        temporal_plain_date_to_string_continuation,
        "Temporal.PlainDate toString calendarName Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => advance_temporal_plain_date_to_string(
            runtime,
            state,
            value,
            return_to,
            execution_budget,
        ),
        GetContinuationDispatch::Suspended(dispatch) => Ok(dispatch),
    }
}

fn temporal_plain_date_to_string_continuation(
    state: TemporalPlainDateToStringContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainDateToStringOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the PlainDate calendarName option may complete through observable primitive conversion"
)]
pub(in crate::vm) fn advance_temporal_plain_date_to_string(
    runtime: &mut Runtime,
    state: TemporalPlainDateToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(value, StoredValue::Undefined) {
        return complete_temporal_plain_date_to_string(&state.date, DisplayCalendar::Auto);
    }
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::String,
        OperatorPrimitiveTarget::TemporalPlainDateToStringCalendarName(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_date_to_string_calendar_name(
    state: &TemporalPlainDateToStringContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let display_calendar = temporal_display_calendar(&source, state.realm, &state.origin)?;
    complete_temporal_plain_date_to_string(&state.date, display_calendar)
}

fn complete_temporal_plain_date_to_string(
    date: &PlainDate,
    display_calendar: DisplayCalendar,
) -> Result<NativeDispatch, NativeFailure> {
    Ok(NativeDispatch::Immediate(StoredValue::String(
        JsString::from_utf8(&date.to_ixdtf_string(display_calendar))?,
    )))
}

#[derive(Clone, Copy)]
enum TemporalPlainDateDifferenceStage {
    LargestUnit,
    RoundingIncrement,
    RoundingMode,
    SmallestUnit,
}

pub(in crate::vm) struct TemporalPlainDateDifferenceContinuation {
    receiver: PlainDate,
    other: PlainDate,
    options: StoredValue,
    largest_unit: Option<Unit>,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    since: bool,
    stage: TemporalPlainDateDifferenceStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainDateDifferenceContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the date operand is converted before the observable options object"
)]
fn begin_temporal_plain_date_difference(
    runtime: &mut Runtime,
    receiver: PlainDate,
    other: PlainDate,
    options: StoredValue,
    since: bool,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return complete_temporal_plain_date_difference(
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
            "Temporal.PlainDate.prototype.until options must be an object",
        );
    }
    begin_temporal_plain_date_difference_get(
        runtime,
        TemporalPlainDateDifferenceContinuation {
            receiver,
            other,
            options,
            largest_unit: None,
            rounding_increment: RoundingIncrement::ONE,
            rounding_mode: RoundingMode::Trunc,
            since,
            stage: TemporalPlainDateDifferenceStage::LargestUnit,
            realm,
            origin,
        },
        "largestUnit",
        TemporalPlainDateDifferenceStage::LargestUnit,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "each date-difference option Get owns the native call state across suspension"
)]
fn begin_temporal_plain_date_difference_get(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateDifferenceContinuation,
    name: &str,
    next_stage: TemporalPlainDateDifferenceStage,
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
        temporal_plain_date_difference_continuation,
        "Temporal.PlainDate difference option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_plain_date_difference_options(
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

fn temporal_plain_date_difference_continuation(
    state: TemporalPlainDateDifferenceContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainDateDifferenceOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one ordered state table preserves date-difference option observation across suspension"
)]
pub(in crate::vm) fn advance_temporal_plain_date_difference_options(
    runtime: &mut Runtime,
    state: TemporalPlainDateDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalPlainDateDifferenceStage::LargestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_date_difference_get(
                    runtime,
                    state,
                    "roundingIncrement",
                    TemporalPlainDateDifferenceStage::RoundingIncrement,
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
                OperatorPrimitiveTarget::TemporalPlainDateDifferenceLargestUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateDifferenceStage::RoundingIncrement => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_date_difference_get(
                    runtime,
                    state,
                    "roundingMode",
                    TemporalPlainDateDifferenceStage::RoundingMode,
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
                OperatorPrimitiveTarget::TemporalPlainDateDifferenceRoundingIncrement(Box::new(
                    state,
                )),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateDifferenceStage::RoundingMode => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_date_difference_get(
                    runtime,
                    state,
                    "smallestUnit",
                    TemporalPlainDateDifferenceStage::SmallestUnit,
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
                OperatorPrimitiveTarget::TemporalPlainDateDifferenceRoundingMode(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainDateDifferenceStage::SmallestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return complete_temporal_plain_date_difference(
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
                OperatorPrimitiveTarget::TemporalPlainDateDifferenceSmallestUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

pub(in crate::vm) fn finish_temporal_plain_date_difference_largest_unit(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.largest_unit = Some(temporal_round_unit(&source, state.realm, &state.origin)?);
    begin_temporal_plain_date_difference_get(
        runtime,
        state,
        "roundingIncrement",
        TemporalPlainDateDifferenceStage::RoundingIncrement,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_date_difference_rounding_increment(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateDifferenceContinuation,
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
    begin_temporal_plain_date_difference_get(
        runtime,
        state,
        "roundingMode",
        TemporalPlainDateDifferenceStage::RoundingMode,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_date_difference_rounding_mode(
    runtime: &mut Runtime,
    mut state: TemporalPlainDateDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.rounding_mode = temporal_rounding_mode(&source, state.realm, &state.origin)?;
    begin_temporal_plain_date_difference_get(
        runtime,
        state,
        "smallestUnit",
        TemporalPlainDateDifferenceStage::SmallestUnit,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_date_difference_smallest_unit(
    runtime: &mut Runtime,
    state: &TemporalPlainDateDifferenceContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let smallest_unit = temporal_round_unit(&source, state.realm, &state.origin)?;
    complete_temporal_plain_date_difference(
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
fn complete_temporal_plain_date_difference(
    runtime: &mut Runtime,
    receiver: &PlainDate,
    other: &PlainDate,
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
