use super::super::conversions::operator_primitive_to_string;
#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use core::str::FromStr;
use temporal_rs::{
    Calendar, MonthCode, PlainMonthDay, TinyAsciiStr,
    fields::CalendarFields,
    options::{DisplayCalendar, Overflow},
    partial::PartialDate,
};

/// The `Temporal.PlainMonthDay` constructor converts month/day, then the
/// calendar identifier, and finally the optional reference ISO year.  The
/// state retains every observable conversion so it remains safe across GC.
pub(in crate::vm) struct TemporalPlainMonthDayConstructorContinuation {
    arguments: Vec<StoredValue>,
    converted: Vec<JsNumber>,
    calendar: Option<Calendar>,
    new_target: FunctionId,
}

const TEMPORAL_PLAIN_MONTH_DAY_BAG_FIELDS: [&str; 7] = [
    "calendar",
    "day",
    "era",
    "eraYear",
    "month",
    "monthCode",
    "year",
];

const TEMPORAL_PLAIN_MONTH_DAY_WITH_FIELDS: [&str; 8] = [
    "calendar",
    "timeZone",
    "day",
    "era",
    "eraYear",
    "month",
    "monthCode",
    "year",
];

#[derive(Clone, Copy)]
enum TemporalPlainMonthDayBagStage {
    ReadField,
    AwaitField,
    AwaitConversion,
}

enum TemporalPlainMonthDayLikeTarget {
    From { options: StoredValue },
    CompareFirst { second: StoredValue },
    CompareSecond { first: PlainMonthDay },
    Equals { receiver: PlainMonthDay },
}

impl TemporalPlainMonthDayLikeTarget {
    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::From { options } | Self::CompareFirst { second: options } => {
                trace_stored_value_root(options, mark);
            }
            Self::CompareSecond { .. } | Self::Equals { .. } => {}
        }
    }
}

pub(in crate::vm) struct TemporalPlainMonthDayFields {
    calendar: Option<Calendar>,
    day: Option<JsNumber>,
    era: Option<TinyAsciiStr<19>>,
    era_year: Option<JsNumber>,
    month: Option<JsNumber>,
    month_code: Option<MonthCode>,
    year: Option<JsNumber>,
}

/// Resumable `ToTemporalMonthDay` conversion for ordinary property bags.
///
/// Each property Get and component conversion can execute JavaScript, so this
/// state retains the source and downstream conversion target across suspension.
pub(in crate::vm) struct TemporalPlainMonthDayBagContinuation {
    base: StoredValue,
    calendar: Option<Calendar>,
    day: Option<JsNumber>,
    era: Option<TinyAsciiStr<19>>,
    era_year: Option<JsNumber>,
    month: Option<JsNumber>,
    month_code: Option<MonthCode>,
    year: Option<JsNumber>,
    next: usize,
    stage: TemporalPlainMonthDayBagStage,
    target: TemporalPlainMonthDayLikeTarget,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainMonthDayBagContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        2
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.base, mark);
        self.target.trace_roots(mark);
    }
}

/// Resumable `calendarName` option state for
/// `Temporal.PlainMonthDay.prototype.toString`.
pub(in crate::vm) struct TemporalPlainMonthDayToStringContinuation {
    month_day: PlainMonthDay,
    options: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

#[derive(Clone, Copy)]
enum TemporalPlainMonthDayToPlainDateStage {
    ReadYear,
    AwaitYear,
    AwaitYearConversion,
}

/// Resumable `Temporal.PlainMonthDay.prototype.toPlainDate` field access.
///
/// The spec reads and converts only `year`; retaining the property bag makes
/// a getter or user-defined numeric conversion safe to suspend and resume.
pub(in crate::vm) struct TemporalPlainMonthDayToPlainDateContinuation {
    month_day: PlainMonthDay,
    fields: StoredValue,
    stage: TemporalPlainMonthDayToPlainDateStage,
    realm: RealmId,
    origin: JsStackFrame,
}

#[derive(Clone, Copy)]
enum TemporalPlainMonthDayWithStage {
    ReadField,
    AwaitField,
    AwaitConversion,
}

pub(in crate::vm) struct TemporalPlainMonthDayWithFields {
    day: Option<i64>,
    era: Option<TinyAsciiStr<19>>,
    era_year: Option<i64>,
    month: Option<i64>,
    month_code: Option<MonthCode>,
    year: Option<i64>,
}

/// Resumable `Temporal.PlainMonthDay.prototype.with` field handling.
///
/// Numeric fields are retained after `ToNumber` / integer validation but before
/// overflow processing, so an observable `options.overflow` read occurs before
/// calendar-specific validation and constraining.
pub(in crate::vm) struct TemporalPlainMonthDayWithContinuation {
    receiver: PlainMonthDay,
    base: StoredValue,
    fields: TemporalPlainMonthDayWithFields,
    next: usize,
    stage: TemporalPlainMonthDayWithStage,
    options: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainMonthDayToStringContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

impl TemporalPlainMonthDayToPlainDateContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.fields, mark);
    }
}

impl TemporalPlainMonthDayWithContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        2
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.base, mark);
        trace_stored_value_root(&self.options, mark);
    }
}

impl TemporalPlainMonthDayConstructorContinuation {
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
pub(in crate::vm) fn begin_temporal_plain_month_day_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = inputs.new_target else {
        return temporal_type_error(realm, &origin, "Temporal.PlainMonthDay is not callable");
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
    advance_temporal_plain_month_day_constructor(
        runtime,
        TemporalPlainMonthDayConstructorContinuation {
            arguments,
            converted,
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
    reason = "Temporal component conversion is resumable across user-defined primitive conversion"
)]
pub(in crate::vm) fn advance_temporal_plain_month_day_constructor(
    runtime: &mut Runtime,
    mut state: TemporalPlainMonthDayConstructorContinuation,
    completion: Option<JsNumber>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        let field = match state.converted.len() {
            0 => "month",
            1 => "day",
            2 => "reference ISO year",
            _ => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainMonthDay constructor resumed after all components converted",
                }
                .into());
            }
        };
        let _ = temporal_plain_date_integer(value, field, realm, origin)?;
        state.converted.push(value);
    }
    if state.converted.len() < 2 {
        let index = state.converted.len();
        let argument = std::mem::replace(&mut state.arguments[index], StoredValue::Undefined);
        return begin_operator_primitive_conversion(
            runtime,
            argument,
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::TemporalPlainMonthDayConstructor(Box::new(state)),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        );
    }
    if state.calendar.is_none() {
        let calendar = std::mem::replace(&mut state.arguments[2], StoredValue::Undefined);
        let calendar = if matches!(calendar, StoredValue::Undefined) {
            Calendar::default()
        } else {
            let StoredValue::String(value) = calendar else {
                return temporal_type_error(
                    realm,
                    origin,
                    "Temporal.PlainMonthDay calendar must be a string",
                );
            };
            match Calendar::try_from_utf8(value.to_utf8_lossy()?.as_bytes()) {
                Ok(calendar) => calendar,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            }
        };
        state.calendar = Some(calendar);
    }
    if state.converted.len() < 3 {
        let argument = std::mem::replace(&mut state.arguments[3], StoredValue::Undefined);
        if matches!(argument, StoredValue::Undefined) {
            state.converted.push(JsNumber::from_i32(1972));
        } else {
            return begin_operator_primitive_conversion(
                runtime,
                argument,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::TemporalPlainMonthDayConstructor(Box::new(state)),
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            );
        }
    }
    complete_temporal_plain_month_day_constructor(
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
fn complete_temporal_plain_month_day_constructor(
    runtime: &mut Runtime,
    state: &TemporalPlainMonthDayConstructorContinuation,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let [month, day, reference_year] = state.converted.as_slice() else {
        return Err(EngineFault::RuntimeInvariant {
            message: "Temporal.PlainMonthDay constructor completed before all components converted",
        }
        .into());
    };
    let month = temporal_plain_date_integer(*month, "month", realm, origin)?;
    let day = temporal_plain_date_integer(*day, "day", realm, origin)?;
    let reference_year =
        temporal_plain_date_integer(*reference_year, "reference ISO year", realm, origin)?;
    let (Ok(month), Ok(day), Ok(reference_year)) = (
        u8::try_from(month),
        u8::try_from(day),
        i32::try_from(reference_year),
    ) else {
        return temporal_range_error(
            realm,
            origin,
            "Temporal.PlainMonthDay fields are outside the supported range",
        );
    };
    let month_day = match PlainMonthDay::new_with_overflow(
        month,
        day,
        state
            .calendar
            .clone()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Temporal.PlainMonthDay constructor completed without a calendar",
            })?,
        Overflow::Reject,
        Some(reference_year),
    ) {
        Ok(month_day) => month_day,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    begin_temporal_plain_month_day_wrapper(
        runtime,
        realm,
        state.new_target,
        month_day,
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
fn begin_temporal_plain_month_day_wrapper(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: FunctionId,
    month_day: PlainMonthDay,
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
        IntrinsicGetContinuation::TemporalPlainMonthDayConstructor {
            new_target,
            month_day,
        },
        return_to,
        Some(origin),
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_month_day_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    month_day: PlainMonthDay,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        _ => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_temporal_plain_month_day_prototype(realm)?)
        }
    };
    let object = runtime.allocate_temporal_plain_month_day(prototype, month_day)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "the static dispatch keeps the Temporal native call shape uniform"
)]
pub(in crate::vm) fn begin_temporal_plain_month_day_static(
    runtime: &mut Runtime,
    method: TemporalPlainMonthDayStaticMethod,
    realm: RealmId,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        TemporalPlainMonthDayStaticMethod::From => begin_temporal_plain_month_day_like(
            runtime,
            arguments.take_first_or_undefined(),
            TemporalPlainMonthDayLikeTarget::From {
                options: arguments.take_first_or_undefined(),
            },
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        TemporalPlainMonthDayStaticMethod::Compare => {
            let first = arguments.take_first_or_undefined();
            let second = arguments.take_first_or_undefined();
            begin_temporal_plain_month_day_like(
                runtime,
                first,
                TemporalPlainMonthDayLikeTarget::CompareFirst { second },
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
    reason = "one dispatcher preserves receiver validation before all PlainMonthDay operations"
)]
pub(in crate::vm) fn dispatch_temporal_plain_month_day_prototype(
    runtime: &mut Runtime,
    method: TemporalPlainMonthDayPrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let month_day = require_temporal_plain_month_day(runtime, receiver, realm, origin)?;
    let number = |value| NativeDispatch::Immediate(StoredValue::Number(JsNumber::from_i64(value)));
    match method {
        TemporalPlainMonthDayPrototypeMethod::CalendarId => Ok(NativeDispatch::Immediate(
            StoredValue::String(JsString::from_utf8(month_day.calendar_id())?),
        )),
        TemporalPlainMonthDayPrototypeMethod::MonthCode => Ok(NativeDispatch::Immediate(
            StoredValue::String(JsString::from_utf8(month_day.month_code().as_str())?),
        )),
        TemporalPlainMonthDayPrototypeMethod::Day => Ok(number(i64::from(month_day.day()))),
        TemporalPlainMonthDayPrototypeMethod::Equals => begin_temporal_plain_month_day_like(
            runtime,
            arguments.take_first_or_undefined(),
            TemporalPlainMonthDayLikeTarget::Equals {
                receiver: month_day,
            },
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainMonthDayPrototypeMethod::ToString => begin_temporal_plain_month_day_to_string(
            runtime,
            month_day,
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainMonthDayPrototypeMethod::ToJson => {
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&month_day.to_ixdtf_string(DisplayCalendar::Auto))?,
            )))
        }
        TemporalPlainMonthDayPrototypeMethod::ToLocaleString => {
            begin_intl_temporal_to_locale_string(
                runtime,
                IntlDateTimeFormatLocaleValue::PlainMonthDay(month_day),
                arguments,
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalPlainMonthDayPrototypeMethod::ToPlainDate => {
            begin_temporal_plain_month_day_to_plain_date(
                runtime,
                month_day,
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalPlainMonthDayPrototypeMethod::With => begin_temporal_plain_month_day_with(
            runtime,
            month_day,
            arguments.take_first_or_undefined(),
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainMonthDayPrototypeMethod::ValueOf => temporal_type_error(
            realm,
            origin,
            "Temporal.PlainMonthDay cannot be converted to a primitive value",
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "all accepted Temporal.PlainMonthDay inputs share a resumable conversion boundary"
)]
fn begin_temporal_plain_month_day_like(
    runtime: &mut Runtime,
    value: StoredValue,
    target: TemporalPlainMonthDayLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(object) = value
        && let Some(month_day) = runtime.temporal_plain_month_day(object)?
    {
        return continue_temporal_plain_month_day_like(
            runtime,
            month_day,
            target,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if let StoredValue::String(value) = value {
        let month_day = match PlainMonthDay::from_utf8(value.to_utf8_lossy()?.as_bytes()) {
            Ok(month_day) => month_day,
            Err(error) => {
                return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                    realm, &origin, error,
                )?));
            }
        };
        return continue_temporal_plain_month_day_like(
            runtime,
            month_day,
            target,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if value.heap_reference().is_some() {
        return advance_temporal_plain_month_day_property_bag(
            runtime,
            TemporalPlainMonthDayBagContinuation {
                base: value,
                calendar: None,
                day: None,
                era: None,
                era_year: None,
                month: None,
                month_code: None,
                year: None,
                next: 0,
                stage: TemporalPlainMonthDayBagStage::ReadField,
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
        "Temporal.PlainMonthDay requires a PlainMonthDay, ISO string, or property bag",
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the conversion target carries its own native call context"
)]
fn continue_temporal_plain_month_day_like(
    runtime: &mut Runtime,
    month_day: PlainMonthDay,
    target: TemporalPlainMonthDayLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match target {
        TemporalPlainMonthDayLikeTarget::From { options } => {
            begin_temporal_plain_date_from_options(
                runtime,
                TemporalPlainDateOverflowTarget::FromMonthDay(month_day),
                options,
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainMonthDayLikeTarget::CompareFirst { second } => {
            begin_temporal_plain_month_day_like(
                runtime,
                second,
                TemporalPlainMonthDayLikeTarget::CompareSecond { first: month_day },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainMonthDayLikeTarget::CompareSecond { first } => {
            let result = first
                .to_ixdtf_string(DisplayCalendar::Always)
                .cmp(&month_day.to_ixdtf_string(DisplayCalendar::Always));
            let result = match result {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            Ok(NativeDispatch::Immediate(StoredValue::Number(
                JsNumber::from_i32(result),
            )))
        }
        TemporalPlainMonthDayLikeTarget::Equals { receiver } => Ok(NativeDispatch::Immediate(
            StoredValue::Boolean(receiver == month_day),
        )),
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the state machine retains the property bag across observable Gets and conversions"
)]
pub(in crate::vm) fn advance_temporal_plain_month_day_property_bag(
    runtime: &mut Runtime,
    mut state: TemporalPlainMonthDayBagContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalPlainMonthDayBagStage::ReadField => {
                if state.next == TEMPORAL_PLAIN_MONTH_DAY_BAG_FIELDS.len() {
                    let TemporalPlainMonthDayBagContinuation {
                        calendar,
                        day,
                        era,
                        era_year,
                        month,
                        month_code,
                        year,
                        target,
                        realm,
                        origin,
                        ..
                    } = state;
                    let fields = TemporalPlainMonthDayFields {
                        calendar,
                        day,
                        era,
                        era_year,
                        month,
                        month_code,
                        year,
                    };
                    return match target {
                        TemporalPlainMonthDayLikeTarget::From { options } => {
                            begin_temporal_plain_date_from_options(
                                runtime,
                                TemporalPlainDateOverflowTarget::FromMonthDayFields(fields),
                                options,
                                realm,
                                return_to,
                                origin,
                                execution_budget,
                            )
                        }
                        target => {
                            let month_day = temporal_plain_month_day_from_fields(
                                &fields,
                                Overflow::Constrain,
                                realm,
                                &origin,
                            )?;
                            continue_temporal_plain_month_day_like(
                                runtime,
                                month_day,
                                target,
                                realm,
                                return_to,
                                origin,
                                execution_budget,
                            )
                        }
                    };
                }
                charge_heap_property_lookup(runtime, &state.base, execution_budget)?;
                let name = JsString::from_utf8(TEMPORAL_PLAIN_MONTH_DAY_BAG_FIELDS[state.next])?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalPlainMonthDayBagStage::AwaitField;
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
                    temporal_plain_month_day_bag_continuation,
                    "Temporal.PlainMonthDay property bag Get produced a structured result",
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
            TemporalPlainMonthDayBagStage::AwaitField => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainMonthDay property bag Get resumed without a value",
                })?;
                let field = TEMPORAL_PLAIN_MONTH_DAY_BAG_FIELDS[state.next];
                if matches!(value, StoredValue::Undefined) {
                    if field == "calendar" {
                        state.calendar = Some(Calendar::default());
                    }
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainMonthDayBagStage::ReadField;
                    continue;
                }
                if field == "calendar" {
                    if let Some(calendar) = temporal_calendar_from_object(runtime, &value)? {
                        state.calendar = Some(calendar);
                        state.next = state.next.saturating_add(1);
                        state.stage = TemporalPlainMonthDayBagStage::ReadField;
                        continue;
                    }
                    let StoredValue::String(value) = value else {
                        return temporal_type_error(
                            state.realm,
                            &state.origin,
                            "Temporal.PlainMonthDay calendar must be a string",
                        );
                    };
                    state.calendar = Some(temporal_calendar_from_string(
                        &value,
                        state.realm,
                        &state.origin,
                    )?);
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainMonthDayBagStage::ReadField;
                    continue;
                }
                state.stage = TemporalPlainMonthDayBagStage::AwaitConversion;
                let hint = match field {
                    "era" | "monthCode" => OperatorPrimitiveHint::String,
                    "day" | "eraYear" | "month" | "year" => OperatorPrimitiveHint::Number,
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "unknown Temporal.PlainMonthDay property bag field",
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
                    OperatorPrimitiveTarget::TemporalPlainMonthDayBag(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalPlainMonthDayBagStage::AwaitConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainMonthDay property bag conversion resumed without a value",
                })?;
                match TEMPORAL_PLAIN_MONTH_DAY_BAG_FIELDS[state.next] {
                    "era" => {
                        let StoredValue::String(value) = value else {
                            return temporal_type_error(
                                state.realm,
                                &state.origin,
                                "Temporal.PlainMonthDay era must be a string",
                            );
                        };
                        state.era =
                            Some(temporal_calendar_era(&value, state.realm, &state.origin)?);
                    }
                    "eraYear" => {
                        state.era_year =
                            Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    "monthCode" => {
                        let StoredValue::String(value) = value else {
                            return temporal_type_error(
                                state.realm,
                                &state.origin,
                                "Temporal.PlainMonthDay monthCode must be a string",
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
                            message: "unknown Temporal.PlainMonthDay property bag field",
                        }
                        .into());
                    }
                }
                state.next = state.next.saturating_add(1);
                state.stage = TemporalPlainMonthDayBagStage::ReadField;
            }
        }
    }
}

fn temporal_plain_month_day_bag_continuation(
    state: TemporalPlainMonthDayBagContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainMonthDayBag(Box::new(state))
}

pub(in crate::vm) fn temporal_plain_month_day_from_fields(
    fields: &TemporalPlainMonthDayFields,
    overflow: Overflow,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<PlainMonthDay, NativeFailure> {
    let day = temporal_plain_date_required_field(fields.day, "day", realm, origin)?;
    let month = fields
        .month
        .map(|value| temporal_plain_date_integer(value, "month", realm, origin))
        .transpose()?;
    let year = fields.year;
    let year = temporal_plain_date_time_optional_i32(year, realm, origin)?;
    let era_year = temporal_plain_date_time_optional_i32(fields.era_year, realm, origin)?;
    let day = temporal_plain_month_day_field_u8(day, "day", overflow, realm, origin)?;
    let month = match month {
        Some(month) => Some(temporal_plain_month_day_field_u8(
            month, "month", overflow, realm, origin,
        )?),
        None => None,
    };
    let calendar = fields.calendar.clone().unwrap_or_default();
    let (era, era_year) = if temporal_calendar_supports_eras(&calendar) {
        (fields.era, era_year)
    } else {
        (None, None)
    };
    let partial = PartialDate::new()
        .with_calendar(calendar)
        .with_era(era)
        .with_era_year(era_year)
        .with_year(year)
        .with_month(month)
        .with_month_code(fields.month_code)
        .with_day(Some(day));
    match PlainMonthDay::from_partial(partial, Some(overflow)) {
        Ok(month_day) => Ok(month_day),
        Err(error) => Err(NativeFailure::Abrupt(temporal_exception_from_error(
            realm, origin, error,
        )?)),
    }
}

fn temporal_plain_month_day_field_u8(
    value: i64,
    field: &str,
    overflow: Overflow,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<u8, NativeFailure> {
    if value < 1 {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            &format!("Temporal.PlainMonthDay {field} is outside the supported range"),
        )?));
    }
    let value = match overflow {
        Overflow::Constrain => value.min(i64::from(u8::MAX)),
        Overflow::Reject => value,
    };
    let Ok(value) = u8::try_from(value) else {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            &format!("Temporal.PlainMonthDay {field} is outside the supported range"),
        )?));
    };
    Ok(value)
}

pub(in crate::vm) fn temporal_plain_month_day_with_fields(
    fields: &TemporalPlainMonthDayWithFields,
    overflow: Overflow,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<CalendarFields, NativeFailure> {
    let year = match fields.year {
        Some(value) => match i32::try_from(value) {
            Ok(value) => Some(value),
            Err(_) => {
                return Err(NativeFailure::Abrupt(temporal_pending_exception(
                    realm,
                    origin,
                    ExceptionKind::RangeError,
                    "Temporal.PlainMonthDay.with year is outside the supported range",
                )?));
            }
        },
        None => None,
    };
    let month = fields
        .month
        .map(|value| temporal_plain_month_day_field_u8(value, "month", overflow, realm, origin))
        .transpose()?;
    let day = fields
        .day
        .map(|value| temporal_plain_month_day_field_u8(value, "day", overflow, realm, origin))
        .transpose()?;
    Ok(CalendarFields::new()
        .with_era(fields.era)
        .with_era_year(
            fields
                .era_year
                .map(|era_year| temporal_plain_date_time_i32(era_year, realm, origin))
                .transpose()?,
        )
        .with_optional_year(year)
        .with_optional_month(month)
        .with_optional_month_code(fields.month_code)
        .with_optional_day(day))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the native method owns its fields and options across observable conversion"
)]
fn begin_temporal_plain_month_day_with(
    runtime: &mut Runtime,
    receiver: PlainMonthDay,
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
            "Temporal.PlainMonthDay.with requires a property bag",
        );
    }
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
            "Temporal.PlainMonthDay.with does not accept a Temporal object",
        );
    }
    advance_temporal_plain_month_day_with(
        runtime,
        TemporalPlainMonthDayWithContinuation {
            receiver,
            base: fields,
            fields: TemporalPlainMonthDayWithFields {
                day: None,
                era: None,
                era_year: None,
                month: None,
                month_code: None,
                year: None,
            },
            next: 0,
            stage: TemporalPlainMonthDayWithStage::ReadField,
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
pub(in crate::vm) fn advance_temporal_plain_month_day_with(
    runtime: &mut Runtime,
    mut state: TemporalPlainMonthDayWithContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalPlainMonthDayWithStage::ReadField => {
                if state.next == TEMPORAL_PLAIN_MONTH_DAY_WITH_FIELDS.len() {
                    if !temporal_calendar_supports_eras(state.receiver.calendar()) {
                        state.fields.era = None;
                        state.fields.era_year = None;
                    }
                    return begin_temporal_plain_date_from_options(
                        runtime,
                        TemporalPlainDateOverflowTarget::MonthDayWith {
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
                let name = JsString::from_utf8(TEMPORAL_PLAIN_MONTH_DAY_WITH_FIELDS[state.next])?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalPlainMonthDayWithStage::AwaitField;
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
                    temporal_plain_month_day_with_continuation,
                    "Temporal.PlainMonthDay.with field Get produced a structured result",
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
            TemporalPlainMonthDayWithStage::AwaitField => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainMonthDay.with field Get resumed without a value",
                })?;
                let field = TEMPORAL_PLAIN_MONTH_DAY_WITH_FIELDS[state.next];
                if matches!(field, "calendar" | "timeZone") {
                    if !matches!(value, StoredValue::Undefined) {
                        return temporal_type_error(
                            state.realm,
                            &state.origin,
                            "Temporal.PlainMonthDay.with cannot override calendar or timeZone",
                        );
                    }
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainMonthDayWithStage::ReadField;
                    continue;
                }
                if matches!(value, StoredValue::Undefined) {
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainMonthDayWithStage::ReadField;
                    continue;
                }
                state.stage = TemporalPlainMonthDayWithStage::AwaitConversion;
                let hint = match field {
                    "era" | "monthCode" => OperatorPrimitiveHint::String,
                    "day" | "eraYear" | "month" | "year" => OperatorPrimitiveHint::Number,
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "unknown Temporal.PlainMonthDay.with field",
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
                    OperatorPrimitiveTarget::TemporalPlainMonthDayWith(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalPlainMonthDayWithStage::AwaitConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainMonthDay.with field conversion resumed without a value",
                })?;
                match TEMPORAL_PLAIN_MONTH_DAY_WITH_FIELDS[state.next] {
                    "era" => {
                        let StoredValue::String(value) = value else {
                            return temporal_type_error(
                                state.realm,
                                &state.origin,
                                "Temporal.PlainMonthDay.with era must be a string",
                            );
                        };
                        state.fields.era =
                            Some(temporal_calendar_era(&value, state.realm, &state.origin)?);
                    }
                    "eraYear" => {
                        state.fields.era_year = Some(temporal_plain_date_integer(
                            operator_to_number(value, state.realm, &state.origin)?,
                            "eraYear",
                            state.realm,
                            &state.origin,
                        )?);
                    }
                    "year" => {
                        state.fields.year = Some(temporal_plain_date_integer(
                            operator_to_number(value, state.realm, &state.origin)?,
                            "year",
                            state.realm,
                            &state.origin,
                        )?);
                    }
                    "month" => {
                        let value = temporal_plain_date_integer(
                            operator_to_number(value, state.realm, &state.origin)?,
                            "month",
                            state.realm,
                            &state.origin,
                        )?;
                        temporal_plain_month_day_with_positive_field(
                            value,
                            "month",
                            state.realm,
                            &state.origin,
                        )?;
                        state.fields.month = Some(value);
                    }
                    "day" => {
                        let value = temporal_plain_date_integer(
                            operator_to_number(value, state.realm, &state.origin)?,
                            "day",
                            state.realm,
                            &state.origin,
                        )?;
                        temporal_plain_month_day_with_positive_field(
                            value,
                            "day",
                            state.realm,
                            &state.origin,
                        )?;
                        state.fields.day = Some(value);
                    }
                    "monthCode" => {
                        let StoredValue::String(value) = value else {
                            return temporal_type_error(
                                state.realm,
                                &state.origin,
                                "Temporal.PlainMonthDay.with monthCode must be a string",
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
                            message: "unknown Temporal.PlainMonthDay.with field",
                        }
                        .into());
                    }
                }
                state.next = state.next.saturating_add(1);
                state.stage = TemporalPlainMonthDayWithStage::ReadField;
            }
        }
    }
}

fn temporal_plain_month_day_with_continuation(
    state: TemporalPlainMonthDayWithContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainMonthDayWith(Box::new(state))
}

fn temporal_plain_month_day_with_positive_field(
    value: i64,
    field: &str,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<(), NativeFailure> {
    if value < 1 {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            &format!("Temporal.PlainMonthDay.with {field} is outside the supported range"),
        )?));
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the native method owns the observable year field conversion"
)]
fn begin_temporal_plain_month_day_to_plain_date(
    runtime: &mut Runtime,
    month_day: PlainMonthDay,
    fields: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if fields.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.PlainMonthDay.toPlainDate requires a property bag",
        );
    }
    advance_temporal_plain_month_day_to_plain_date(
        runtime,
        TemporalPlainMonthDayToPlainDateContinuation {
            month_day,
            fields,
            stage: TemporalPlainMonthDayToPlainDateStage::ReadYear,
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
    reason = "the explicit state machine preserves the year Get and ToNumber order"
)]
pub(in crate::vm) fn advance_temporal_plain_month_day_to_plain_date(
    runtime: &mut Runtime,
    mut state: TemporalPlainMonthDayToPlainDateContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalPlainMonthDayToPlainDateStage::ReadYear => {
                charge_heap_property_lookup(runtime, &state.fields, execution_budget)?;
                let name = JsString::from_utf8("year")?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalPlainMonthDayToPlainDateStage::AwaitYear;
                let dispatch = begin_value_get(
                    runtime,
                    &state.fields,
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
                    temporal_plain_month_day_to_plain_date_continuation,
                    "Temporal.PlainMonthDay.toPlainDate year Get produced a structured result",
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
            TemporalPlainMonthDayToPlainDateStage::AwaitYear => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainMonthDay.toPlainDate year Get resumed without a value",
                })?;
                if matches!(value, StoredValue::Undefined) {
                    return finish_temporal_plain_month_day_to_plain_date(
                        runtime,
                        &state.month_day,
                        None,
                        state.realm,
                        &state.origin,
                    );
                }
                state.stage = TemporalPlainMonthDayToPlainDateStage::AwaitYearConversion;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::TemporalPlainMonthDayToPlainDate(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalPlainMonthDayToPlainDateStage::AwaitYearConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message:
                        "Temporal.PlainMonthDay.toPlainDate year conversion resumed without a value",
                })?;
                let year = operator_to_number(value, state.realm, &state.origin)?;
                let year = temporal_plain_date_integer(year, "year", state.realm, &state.origin)?;
                let Ok(year) = i32::try_from(year) else {
                    return Err(NativeFailure::Abrupt(temporal_pending_exception(
                        state.realm,
                        &state.origin,
                        ExceptionKind::RangeError,
                        "Temporal.PlainMonthDay.toPlainDate year is outside the supported range",
                    )?));
                };
                return finish_temporal_plain_month_day_to_plain_date(
                    runtime,
                    &state.month_day,
                    Some(year),
                    state.realm,
                    &state.origin,
                );
            }
        }
    }
}

fn temporal_plain_month_day_to_plain_date_continuation(
    state: TemporalPlainMonthDayToPlainDateContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainMonthDayToPlainDate(Box::new(state))
}

fn finish_temporal_plain_month_day_to_plain_date(
    runtime: &mut Runtime,
    month_day: &PlainMonthDay,
    year: Option<i32>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let fields = year.map(|year| CalendarFields::new().with_year(year));
    let date = match month_day.to_plain_date(fields) {
        Ok(date) => date,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    allocate_temporal_plain_date_result(runtime, realm, date)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the ordered PlainMonthDay calendar formatting reader owns its resumable native call context"
)]
fn begin_temporal_plain_month_day_to_string(
    runtime: &mut Runtime,
    month_day: PlainMonthDay,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return complete_temporal_plain_month_day_to_string(&month_day, DisplayCalendar::Auto);
    }
    if options.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.PlainMonthDay.prototype.toString options must be an object",
        );
    }
    begin_temporal_plain_month_day_to_string_get(
        runtime,
        TemporalPlainMonthDayToStringContinuation {
            month_day,
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
    reason = "the observable PlainMonthDay calendarName Get retains native call state"
)]
fn begin_temporal_plain_month_day_to_string_get(
    runtime: &mut Runtime,
    state: TemporalPlainMonthDayToStringContinuation,
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
        temporal_plain_month_day_to_string_continuation,
        "Temporal.PlainMonthDay toString calendarName Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_plain_month_day_to_string(
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

fn temporal_plain_month_day_to_string_continuation(
    state: TemporalPlainMonthDayToStringContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainMonthDayToStringOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the PlainMonthDay calendarName option may complete through observable primitive conversion"
)]
pub(in crate::vm) fn advance_temporal_plain_month_day_to_string(
    runtime: &mut Runtime,
    state: TemporalPlainMonthDayToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(value, StoredValue::Undefined) {
        return complete_temporal_plain_month_day_to_string(
            &state.month_day,
            DisplayCalendar::Auto,
        );
    }
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::String,
        OperatorPrimitiveTarget::TemporalPlainMonthDayToStringCalendarName(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_month_day_to_string_calendar_name(
    state: &TemporalPlainMonthDayToStringContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let display_calendar = temporal_display_calendar(&source, state.realm, &state.origin)?;
    complete_temporal_plain_month_day_to_string(&state.month_day, display_calendar)
}

fn complete_temporal_plain_month_day_to_string(
    month_day: &PlainMonthDay,
    display_calendar: DisplayCalendar,
) -> Result<NativeDispatch, NativeFailure> {
    Ok(NativeDispatch::Immediate(StoredValue::String(
        JsString::from_utf8(&month_day.to_ixdtf_string(display_calendar))?,
    )))
}
