use super::super::conversions::operator_primitive_to_string;
#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use core::str::FromStr;
use temporal_rs::{
    Calendar, MonthCode, PlainYearMonth, TinyAsciiStr,
    fields::{CalendarFields, YearMonthCalendarFields},
    options::{
        DifferenceSettings, DisplayCalendar, Overflow, RoundingIncrement, RoundingMode, Unit,
    },
    partial::PartialYearMonth,
};

/// The `Temporal.PlainYearMonth` constructor converts ISO year/month, then
/// the calendar identifier, and finally the optional reference ISO day.
/// Each user-observable numeric conversion is retained across suspension.
pub(in crate::vm) struct TemporalPlainYearMonthConstructorContinuation {
    arguments: Vec<StoredValue>,
    converted: Vec<JsNumber>,
    calendar: Option<Calendar>,
    new_target: FunctionId,
}

const TEMPORAL_PLAIN_YEAR_MONTH_BAG_FIELDS: [&str; 6] =
    ["calendar", "era", "eraYear", "month", "monthCode", "year"];

const TEMPORAL_PLAIN_YEAR_MONTH_WITH_FIELDS: [&str; 7] = [
    "calendar",
    "timeZone",
    "era",
    "eraYear",
    "month",
    "monthCode",
    "year",
];

enum TemporalPlainYearMonthLikeTarget {
    From {
        options: StoredValue,
    },
    CompareFirst {
        second: StoredValue,
    },
    CompareSecond {
        first: PlainYearMonth,
    },
    Equals {
        receiver: PlainYearMonth,
    },
    Difference {
        receiver: PlainYearMonth,
        options: StoredValue,
        since: bool,
    },
}

impl TemporalPlainYearMonthLikeTarget {
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

pub(in crate::vm) struct TemporalPlainYearMonthFields {
    calendar: Option<Calendar>,
    era: Option<TinyAsciiStr<19>>,
    era_year: Option<JsNumber>,
    month: Option<JsNumber>,
    month_code: Option<MonthCode>,
    year: Option<JsNumber>,
}

#[derive(Clone, Copy)]
enum TemporalPlainYearMonthBagStage {
    ReadField,
    AwaitField,
    AwaitConversion,
}

/// Resumable `ToTemporalYearMonth` conversion for ordinary property bags.
///
/// The field order is the Temporal `PrepareTemporalFields` order for a
/// year-month record. Both the property reads and their primitive conversions
/// remain explicitly resumable so getters cannot lose state across GC.
pub(in crate::vm) struct TemporalPlainYearMonthBagContinuation {
    base: StoredValue,
    calendar: Option<Calendar>,
    era: Option<TinyAsciiStr<19>>,
    era_year: Option<JsNumber>,
    month: Option<JsNumber>,
    month_code: Option<MonthCode>,
    year: Option<JsNumber>,
    next: usize,
    stage: TemporalPlainYearMonthBagStage,
    target: TemporalPlainYearMonthLikeTarget,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainYearMonthBagContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        2
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.base, mark);
        self.target.trace_roots(mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalPlainYearMonthWithStage {
    ReadField,
    AwaitField,
    AwaitConversion,
}

pub(in crate::vm) struct TemporalPlainYearMonthWithFields {
    era: Option<TinyAsciiStr<19>>,
    era_year: Option<i64>,
    month: Option<i64>,
    month_code: Option<MonthCode>,
    year: Option<i64>,
}

/// Resumable `Temporal.PlainYearMonth.prototype.with` field handling.
///
/// All user-visible fields are prepared before the overflow option is read, so
/// invalid calendar-specific combinations do not pre-empt observable options.
pub(in crate::vm) struct TemporalPlainYearMonthWithContinuation {
    receiver: PlainYearMonth,
    base: StoredValue,
    fields: TemporalPlainYearMonthWithFields,
    next: usize,
    stage: TemporalPlainYearMonthWithStage,
    options: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainYearMonthWithContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        2
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.base, mark);
        trace_stored_value_root(&self.options, mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalPlainYearMonthDifferenceStage {
    LargestUnit,
    RoundingIncrement,
    RoundingMode,
    SmallestUnit,
}

/// Resumable options state for `Temporal.PlainYearMonth.prototype.until` and
/// `since`. The converted operand is retained before the first option Get.
pub(in crate::vm) struct TemporalPlainYearMonthDifferenceContinuation {
    receiver: PlainYearMonth,
    other: PlainYearMonth,
    options: StoredValue,
    largest_unit: Option<Unit>,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    since: bool,
    stage: TemporalPlainYearMonthDifferenceStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainYearMonthDifferenceContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

/// Resumable `calendarName` option state for
/// `Temporal.PlainYearMonth.prototype.toString`.
pub(in crate::vm) struct TemporalPlainYearMonthToStringContinuation {
    year_month: PlainYearMonth,
    options: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

#[derive(Clone, Copy)]
enum TemporalPlainYearMonthToPlainDateStage {
    ReadDay,
    AwaitDay,
    AwaitDayConversion,
}

/// Resumable `Temporal.PlainYearMonth.prototype.toPlainDate` field access.
///
/// The specification reads and converts only `day`; the property bag remains
/// rooted while an accessor or user-defined primitive conversion is running.
pub(in crate::vm) struct TemporalPlainYearMonthToPlainDateContinuation {
    year_month: PlainYearMonth,
    fields: StoredValue,
    stage: TemporalPlainYearMonthToPlainDateStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalPlainYearMonthToStringContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

impl TemporalPlainYearMonthToPlainDateContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.fields, mark);
    }
}

impl TemporalPlainYearMonthConstructorContinuation {
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
pub(in crate::vm) fn begin_temporal_plain_year_month_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = inputs.new_target else {
        return temporal_type_error(realm, &origin, "Temporal.PlainYearMonth is not callable");
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
    advance_temporal_plain_year_month_constructor(
        runtime,
        TemporalPlainYearMonthConstructorContinuation {
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
pub(in crate::vm) fn advance_temporal_plain_year_month_constructor(
    runtime: &mut Runtime,
    mut state: TemporalPlainYearMonthConstructorContinuation,
    completion: Option<JsNumber>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        let field = match state.converted.len() {
            0 => "ISO year",
            1 => "ISO month",
            2 => "reference ISO day",
            _ => {
                return Err(EngineFault::RuntimeInvariant {
                    message:
                        "Temporal.PlainYearMonth constructor resumed after all components converted",
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
            OperatorPrimitiveTarget::TemporalPlainYearMonthConstructor(Box::new(state)),
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
                    "Temporal.PlainYearMonth calendar must be a string",
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
            state.converted.push(JsNumber::from_i32(1));
        } else {
            return begin_operator_primitive_conversion(
                runtime,
                argument,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::TemporalPlainYearMonthConstructor(Box::new(state)),
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            );
        }
    }
    complete_temporal_plain_year_month_constructor(
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
fn complete_temporal_plain_year_month_constructor(
    runtime: &mut Runtime,
    state: &TemporalPlainYearMonthConstructorContinuation,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let [year, month, reference_day] = state.converted.as_slice() else {
        return Err(EngineFault::RuntimeInvariant {
            message: "Temporal.PlainYearMonth constructor completed before all components converted",
        }
        .into());
    };
    let year = temporal_plain_date_integer(*year, "ISO year", realm, origin)?;
    let month = temporal_plain_date_integer(*month, "ISO month", realm, origin)?;
    let reference_day =
        temporal_plain_date_integer(*reference_day, "reference ISO day", realm, origin)?;
    let (Ok(year), Ok(month), Ok(reference_day)) = (
        i32::try_from(year),
        u8::try_from(month),
        u8::try_from(reference_day),
    ) else {
        return temporal_range_error(
            realm,
            origin,
            "Temporal.PlainYearMonth fields are outside the supported range",
        );
    };
    let year_month = match PlainYearMonth::new_with_overflow(
        year,
        month,
        Some(reference_day),
        state
            .calendar
            .clone()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Temporal.PlainYearMonth constructor completed without a calendar",
            })?,
        Overflow::Reject,
    ) {
        Ok(year_month) => year_month,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    begin_temporal_plain_year_month_wrapper(
        runtime,
        realm,
        state.new_target,
        year_month,
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
fn begin_temporal_plain_year_month_wrapper(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: FunctionId,
    year_month: PlainYearMonth,
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
        IntrinsicGetContinuation::TemporalPlainYearMonthConstructor {
            new_target,
            year_month,
        },
        return_to,
        Some(origin),
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_year_month_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    year_month: PlainYearMonth,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        _ => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_temporal_plain_year_month_prototype(realm)?)
        }
    };
    let object = runtime.allocate_temporal_plain_year_month(prototype, year_month)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "the static dispatch keeps the Temporal native call shape uniform"
)]
pub(in crate::vm) fn begin_temporal_plain_year_month_static(
    runtime: &mut Runtime,
    method: TemporalPlainYearMonthStaticMethod,
    realm: RealmId,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        TemporalPlainYearMonthStaticMethod::From => begin_temporal_plain_year_month_like(
            runtime,
            arguments.take_first_or_undefined(),
            TemporalPlainYearMonthLikeTarget::From {
                options: arguments.take_first_or_undefined(),
            },
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        TemporalPlainYearMonthStaticMethod::Compare => {
            let first = arguments.take_first_or_undefined();
            let second = arguments.take_first_or_undefined();
            begin_temporal_plain_year_month_like(
                runtime,
                first,
                TemporalPlainYearMonthLikeTarget::CompareFirst { second },
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
    clippy::too_many_lines,
    reason = "one dispatcher preserves receiver validation before all PlainYearMonth operations"
)]
pub(in crate::vm) fn dispatch_temporal_plain_year_month_prototype(
    runtime: &mut Runtime,
    method: TemporalPlainYearMonthPrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let year_month = require_temporal_plain_year_month(runtime, receiver, realm, origin)?;
    if let Some(accessor) = dispatch_temporal_plain_year_month_accessor(method, &year_month)? {
        return Ok(accessor);
    }
    match method {
        TemporalPlainYearMonthPrototypeMethod::ToString => {
            begin_temporal_plain_year_month_to_string(
                runtime,
                year_month,
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalPlainYearMonthPrototypeMethod::ToJson => {
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&year_month.to_ixdtf_string(DisplayCalendar::Auto))?,
            )))
        }
        TemporalPlainYearMonthPrototypeMethod::ToLocaleString => {
            begin_intl_temporal_to_locale_string(
                runtime,
                IntlDateTimeFormatLocaleValue::PlainYearMonth(year_month),
                arguments,
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalPlainYearMonthPrototypeMethod::ValueOf => temporal_type_error(
            realm,
            origin,
            "Temporal.PlainYearMonth cannot be converted to a primitive value",
        ),
        TemporalPlainYearMonthPrototypeMethod::ToPlainDate => {
            begin_temporal_plain_year_month_to_plain_date(
                runtime,
                year_month,
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalPlainYearMonthPrototypeMethod::Equals => begin_temporal_plain_year_month_like(
            runtime,
            arguments.take_first_or_undefined(),
            TemporalPlainYearMonthLikeTarget::Equals {
                receiver: year_month,
            },
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainYearMonthPrototypeMethod::With => begin_temporal_plain_year_month_with(
            runtime,
            year_month,
            arguments.take_first_or_undefined(),
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainYearMonthPrototypeMethod::Add
        | TemporalPlainYearMonthPrototypeMethod::Subtract => begin_temporal_duration_like(
            runtime,
            arguments.take_first_or_undefined(),
            TemporalDurationLikeTarget::PlainYearMonthArithmetic {
                receiver: year_month,
                subtract: matches!(method, TemporalPlainYearMonthPrototypeMethod::Subtract),
                options: arguments.take_first_or_undefined(),
            },
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainYearMonthPrototypeMethod::Until
        | TemporalPlainYearMonthPrototypeMethod::Since => begin_temporal_plain_year_month_like(
            runtime,
            arguments.take_first_or_undefined(),
            TemporalPlainYearMonthLikeTarget::Difference {
                receiver: year_month,
                options: arguments.take_first_or_undefined(),
                since: matches!(method, TemporalPlainYearMonthPrototypeMethod::Since),
            },
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalPlainYearMonthPrototypeMethod::CalendarId
        | TemporalPlainYearMonthPrototypeMethod::Era
        | TemporalPlainYearMonthPrototypeMethod::EraYear
        | TemporalPlainYearMonthPrototypeMethod::Year
        | TemporalPlainYearMonthPrototypeMethod::Month
        | TemporalPlainYearMonthPrototypeMethod::MonthCode
        | TemporalPlainYearMonthPrototypeMethod::DaysInYear
        | TemporalPlainYearMonthPrototypeMethod::DaysInMonth
        | TemporalPlainYearMonthPrototypeMethod::MonthsInYear
        | TemporalPlainYearMonthPrototypeMethod::InLeapYear => {
            unreachable!("Temporal.PlainYearMonth accessor dispatch returned no value")
        }
    }
}

fn dispatch_temporal_plain_year_month_accessor(
    method: TemporalPlainYearMonthPrototypeMethod,
    year_month: &PlainYearMonth,
) -> Result<Option<NativeDispatch>, NativeFailure> {
    let number = |value| NativeDispatch::Immediate(StoredValue::Number(JsNumber::from_i64(value)));
    let dispatch = match method {
        TemporalPlainYearMonthPrototypeMethod::CalendarId => NativeDispatch::Immediate(
            StoredValue::String(JsString::from_utf8(year_month.calendar_id())?),
        ),
        TemporalPlainYearMonthPrototypeMethod::Era => {
            NativeDispatch::Immediate(match year_month.era() {
                Some(era) => StoredValue::String(JsString::from_utf8(era.as_str())?),
                None => StoredValue::Undefined,
            })
        }
        TemporalPlainYearMonthPrototypeMethod::EraYear => NativeDispatch::Immediate(
            year_month
                .era_year()
                .map_or(StoredValue::Undefined, |value| {
                    StoredValue::Number(JsNumber::from_i64(i64::from(value)))
                }),
        ),
        TemporalPlainYearMonthPrototypeMethod::Year => number(i64::from(year_month.year())),
        TemporalPlainYearMonthPrototypeMethod::Month => number(i64::from(year_month.month())),
        TemporalPlainYearMonthPrototypeMethod::MonthCode => NativeDispatch::Immediate(
            StoredValue::String(JsString::from_utf8(year_month.month_code().as_str())?),
        ),
        TemporalPlainYearMonthPrototypeMethod::DaysInYear => {
            number(i64::from(year_month.days_in_year()))
        }
        TemporalPlainYearMonthPrototypeMethod::DaysInMonth => {
            number(i64::from(year_month.days_in_month()))
        }
        TemporalPlainYearMonthPrototypeMethod::MonthsInYear => {
            number(i64::from(year_month.months_in_year()))
        }
        TemporalPlainYearMonthPrototypeMethod::InLeapYear => {
            NativeDispatch::Immediate(StoredValue::Boolean(year_month.in_leap_year()))
        }
        _ => return Ok(None),
    };
    Ok(Some(dispatch))
}

#[allow(
    clippy::too_many_arguments,
    reason = "all accepted Temporal.PlainYearMonth inputs share a resumable conversion boundary"
)]
fn begin_temporal_plain_year_month_like(
    runtime: &mut Runtime,
    value: StoredValue,
    target: TemporalPlainYearMonthLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(object) = value
        && let Some(year_month) = runtime.temporal_plain_year_month(object)?
    {
        return continue_temporal_plain_year_month_like(
            runtime,
            year_month,
            target,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if let StoredValue::String(value) = value {
        let year_month = match PlainYearMonth::from_utf8(value.to_utf8_lossy()?.as_bytes()) {
            Ok(year_month) => year_month,
            Err(error) => {
                return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                    realm, &origin, error,
                )?));
            }
        };
        return continue_temporal_plain_year_month_like(
            runtime,
            year_month,
            target,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if value.heap_reference().is_some() {
        return advance_temporal_plain_year_month_property_bag(
            runtime,
            TemporalPlainYearMonthBagContinuation {
                base: value,
                calendar: None,
                era: None,
                era_year: None,
                month: None,
                month_code: None,
                year: None,
                next: 0,
                stage: TemporalPlainYearMonthBagStage::ReadField,
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
        "Temporal.PlainYearMonth requires a PlainYearMonth, ISO string, or property bag",
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the conversion target carries its own native call context"
)]
fn continue_temporal_plain_year_month_like(
    runtime: &mut Runtime,
    year_month: PlainYearMonth,
    target: TemporalPlainYearMonthLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match target {
        TemporalPlainYearMonthLikeTarget::From { options } => {
            begin_temporal_plain_date_from_options(
                runtime,
                TemporalPlainDateOverflowTarget::FromYearMonth(year_month),
                options,
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainYearMonthLikeTarget::CompareFirst { second } => {
            begin_temporal_plain_year_month_like(
                runtime,
                second,
                TemporalPlainYearMonthLikeTarget::CompareSecond { first: year_month },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainYearMonthLikeTarget::CompareSecond { first } => {
            let result = match first.compare_iso(&year_month) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            Ok(NativeDispatch::Immediate(StoredValue::Number(
                JsNumber::from_i32(result),
            )))
        }
        TemporalPlainYearMonthLikeTarget::Equals { receiver } => Ok(NativeDispatch::Immediate(
            StoredValue::Boolean(receiver == year_month),
        )),
        TemporalPlainYearMonthLikeTarget::Difference {
            receiver,
            options,
            since,
        } => begin_temporal_plain_year_month_difference(
            runtime,
            receiver,
            year_month,
            options,
            since,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the state machine retains the property bag across observable Gets and conversions"
)]
pub(in crate::vm) fn advance_temporal_plain_year_month_property_bag(
    runtime: &mut Runtime,
    mut state: TemporalPlainYearMonthBagContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalPlainYearMonthBagStage::ReadField => {
                if state.next == TEMPORAL_PLAIN_YEAR_MONTH_BAG_FIELDS.len() {
                    let TemporalPlainYearMonthBagContinuation {
                        calendar,
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
                    let fields = TemporalPlainYearMonthFields {
                        calendar,
                        era,
                        era_year,
                        month,
                        month_code,
                        year,
                    };
                    return match target {
                        TemporalPlainYearMonthLikeTarget::From { options } => {
                            begin_temporal_plain_date_from_options(
                                runtime,
                                TemporalPlainDateOverflowTarget::FromYearMonthFields(fields),
                                options,
                                realm,
                                return_to,
                                origin,
                                execution_budget,
                            )
                        }
                        target => {
                            let year_month = temporal_plain_year_month_from_fields(
                                &fields,
                                Overflow::Constrain,
                                realm,
                                &origin,
                            )?;
                            continue_temporal_plain_year_month_like(
                                runtime,
                                year_month,
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
                let name = JsString::from_utf8(TEMPORAL_PLAIN_YEAR_MONTH_BAG_FIELDS[state.next])?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalPlainYearMonthBagStage::AwaitField;
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
                    temporal_plain_year_month_bag_continuation,
                    "Temporal.PlainYearMonth property bag Get produced a structured result",
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
            TemporalPlainYearMonthBagStage::AwaitField => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainYearMonth property bag Get resumed without a value",
                })?;
                let field = TEMPORAL_PLAIN_YEAR_MONTH_BAG_FIELDS[state.next];
                if matches!(value, StoredValue::Undefined) {
                    if field == "calendar" {
                        state.calendar = Some(Calendar::default());
                    }
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainYearMonthBagStage::ReadField;
                    continue;
                }
                if field == "calendar" {
                    if let Some(calendar) = temporal_calendar_from_object(runtime, &value)? {
                        state.calendar = Some(calendar);
                        state.next = state.next.saturating_add(1);
                        state.stage = TemporalPlainYearMonthBagStage::ReadField;
                        continue;
                    }
                    let StoredValue::String(value) = value else {
                        return temporal_type_error(
                            state.realm,
                            &state.origin,
                            "Temporal.PlainYearMonth calendar must be a string",
                        );
                    };
                    state.calendar = Some(temporal_calendar_from_string(
                        &value,
                        state.realm,
                        &state.origin,
                    )?);
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainYearMonthBagStage::ReadField;
                    continue;
                }
                state.stage = TemporalPlainYearMonthBagStage::AwaitConversion;
                let hint = match field {
                    "era" | "monthCode" => OperatorPrimitiveHint::String,
                    "eraYear" | "month" | "year" => OperatorPrimitiveHint::Number,
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "unknown Temporal.PlainYearMonth property bag field",
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
                    OperatorPrimitiveTarget::TemporalPlainYearMonthBag(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalPlainYearMonthBagStage::AwaitConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message:
                        "Temporal.PlainYearMonth property bag conversion resumed without a value",
                })?;
                match TEMPORAL_PLAIN_YEAR_MONTH_BAG_FIELDS[state.next] {
                    "era" => {
                        let StoredValue::String(value) = value else {
                            return temporal_type_error(
                                state.realm,
                                &state.origin,
                                "Temporal.PlainYearMonth era must be a string",
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
                                "Temporal.PlainYearMonth monthCode must be a string",
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
                    "month" => {
                        state.month = Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    "year" => {
                        state.year = Some(operator_to_number(value, state.realm, &state.origin)?);
                    }
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "unknown Temporal.PlainYearMonth property bag field",
                        }
                        .into());
                    }
                }
                state.next = state.next.saturating_add(1);
                state.stage = TemporalPlainYearMonthBagStage::ReadField;
            }
        }
    }
}

fn temporal_plain_year_month_bag_continuation(
    state: TemporalPlainYearMonthBagContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainYearMonthBag(Box::new(state))
}

pub(in crate::vm) fn temporal_plain_year_month_from_fields(
    fields: &TemporalPlainYearMonthFields,
    overflow: Overflow,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<PlainYearMonth, NativeFailure> {
    let calendar = fields.calendar.clone().unwrap_or_default();
    let year = temporal_plain_date_time_optional_i32(fields.year, realm, origin)?;
    let era_year = temporal_plain_date_time_optional_i32(fields.era_year, realm, origin)?;
    let month = fields
        .month
        .map(|month| temporal_plain_date_integer(month, "month", realm, origin))
        .transpose()?
        .map(|month| temporal_plain_year_month_month(month, &calendar, overflow, realm, origin))
        .transpose()?;
    let (era, era_year) = if temporal_calendar_supports_eras(&calendar) {
        (fields.era, era_year)
    } else {
        (None, None)
    };
    let calendar_fields = YearMonthCalendarFields::new()
        .with_era(era)
        .with_era_year(era_year)
        .with_optional_year(year)
        .with_optional_month(month)
        .with_optional_month_code(fields.month_code);
    let partial = PartialYearMonth {
        calendar_fields,
        calendar,
    };
    match PlainYearMonth::from_partial(partial, Some(overflow)) {
        Ok(year_month) => Ok(year_month),
        Err(error) => Err(NativeFailure::Abrupt(temporal_exception_from_error(
            realm, origin, error,
        )?)),
    }
}

fn temporal_plain_year_month_month(
    month: i64,
    calendar: &Calendar,
    overflow: Overflow,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<u8, NativeFailure> {
    if month < 1 {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "Temporal.PlainYearMonth month is outside the supported range",
        )?));
    }
    let maximum = if calendar.identifier() == "iso8601" {
        12
    } else {
        u8::MAX
    };
    let month = match overflow {
        Overflow::Constrain => month.min(i64::from(maximum)),
        Overflow::Reject => month,
    };
    let Ok(month) = u8::try_from(month) else {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "Temporal.PlainYearMonth month is outside the supported range",
        )?));
    };
    Ok(month)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the native method owns its fields and options across observable conversion"
)]
fn begin_temporal_plain_year_month_with(
    runtime: &mut Runtime,
    receiver: PlainYearMonth,
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
            "Temporal.PlainYearMonth.with requires a property bag",
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
            "Temporal.PlainYearMonth.with does not accept a Temporal object",
        );
    }
    advance_temporal_plain_year_month_with(
        runtime,
        TemporalPlainYearMonthWithContinuation {
            receiver,
            base: fields,
            fields: TemporalPlainYearMonthWithFields {
                era: None,
                era_year: None,
                month: None,
                month_code: None,
                year: None,
            },
            next: 0,
            stage: TemporalPlainYearMonthWithStage::ReadField,
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
pub(in crate::vm) fn advance_temporal_plain_year_month_with(
    runtime: &mut Runtime,
    mut state: TemporalPlainYearMonthWithContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalPlainYearMonthWithStage::ReadField => {
                if state.next == TEMPORAL_PLAIN_YEAR_MONTH_WITH_FIELDS.len() {
                    if !temporal_calendar_supports_eras(state.receiver.calendar()) {
                        state.fields.era = None;
                        state.fields.era_year = None;
                    }
                    return begin_temporal_plain_date_from_options(
                        runtime,
                        TemporalPlainDateOverflowTarget::YearMonthWith {
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
                let name = JsString::from_utf8(TEMPORAL_PLAIN_YEAR_MONTH_WITH_FIELDS[state.next])?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalPlainYearMonthWithStage::AwaitField;
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
                    temporal_plain_year_month_with_continuation,
                    "Temporal.PlainYearMonth.with field Get produced a structured result",
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
            TemporalPlainYearMonthWithStage::AwaitField => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainYearMonth.with field Get resumed without a value",
                })?;
                let field = TEMPORAL_PLAIN_YEAR_MONTH_WITH_FIELDS[state.next];
                if matches!(field, "calendar" | "timeZone") {
                    if !matches!(value, StoredValue::Undefined) {
                        return temporal_type_error(
                            state.realm,
                            &state.origin,
                            "Temporal.PlainYearMonth.with cannot override calendar or timeZone",
                        );
                    }
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainYearMonthWithStage::ReadField;
                    continue;
                }
                if matches!(value, StoredValue::Undefined) {
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalPlainYearMonthWithStage::ReadField;
                    continue;
                }
                state.stage = TemporalPlainYearMonthWithStage::AwaitConversion;
                let hint = match field {
                    "era" | "monthCode" => OperatorPrimitiveHint::String,
                    "eraYear" | "month" | "year" => OperatorPrimitiveHint::Number,
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "unknown Temporal.PlainYearMonth.with field",
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
                    OperatorPrimitiveTarget::TemporalPlainYearMonthWith(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalPlainYearMonthWithStage::AwaitConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainYearMonth.with field conversion resumed without a value",
                })?;
                match TEMPORAL_PLAIN_YEAR_MONTH_WITH_FIELDS[state.next] {
                    "era" => {
                        let StoredValue::String(value) = value else {
                            return temporal_type_error(
                                state.realm,
                                &state.origin,
                                "Temporal.PlainYearMonth.with era must be a string",
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
                    "month" => {
                        let value = temporal_plain_date_integer(
                            operator_to_number(value, state.realm, &state.origin)?,
                            "month",
                            state.realm,
                            &state.origin,
                        )?;
                        if value < 1 {
                            return Err(NativeFailure::Abrupt(temporal_pending_exception(
                                state.realm,
                                &state.origin,
                                ExceptionKind::RangeError,
                                "Temporal.PlainYearMonth.with month is outside the supported range",
                            )?));
                        }
                        state.fields.month = Some(value);
                    }
                    "year" => {
                        state.fields.year = Some(temporal_plain_date_integer(
                            operator_to_number(value, state.realm, &state.origin)?,
                            "year",
                            state.realm,
                            &state.origin,
                        )?);
                    }
                    "monthCode" => {
                        let StoredValue::String(value) = value else {
                            return temporal_type_error(
                                state.realm,
                                &state.origin,
                                "Temporal.PlainYearMonth.with monthCode must be a string",
                            );
                        };
                        state.fields.month_code =
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
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "unknown Temporal.PlainYearMonth.with field",
                        }
                        .into());
                    }
                }
                state.next = state.next.saturating_add(1);
                state.stage = TemporalPlainYearMonthWithStage::ReadField;
            }
        }
    }
}

fn temporal_plain_year_month_with_continuation(
    state: TemporalPlainYearMonthWithContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainYearMonthWith(Box::new(state))
}

pub(in crate::vm) fn temporal_plain_year_month_with_fields(
    fields: &TemporalPlainYearMonthWithFields,
    calendar: &Calendar,
    overflow: Overflow,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<YearMonthCalendarFields, NativeFailure> {
    let year = match fields.year {
        Some(year) => match i32::try_from(year) {
            Ok(year) => Some(year),
            Err(_) => {
                return Err(NativeFailure::Abrupt(temporal_pending_exception(
                    realm,
                    origin,
                    ExceptionKind::RangeError,
                    "Temporal.PlainYearMonth.with year is outside the supported range",
                )?));
            }
        },
        None => None,
    };
    let month = fields
        .month
        .map(|month| temporal_plain_year_month_month(month, calendar, overflow, realm, origin))
        .transpose()?;
    let era_year = fields
        .era_year
        .map(|era_year| temporal_plain_date_time_i32(era_year, realm, origin))
        .transpose()?;
    Ok(YearMonthCalendarFields::new()
        .with_era(fields.era)
        .with_era_year(era_year)
        .with_optional_year(year)
        .with_optional_month(month)
        .with_optional_month_code(fields.month_code))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the year-month operand is converted before the observable options object"
)]
fn begin_temporal_plain_year_month_difference(
    runtime: &mut Runtime,
    receiver: PlainYearMonth,
    other: PlainYearMonth,
    options: StoredValue,
    since: bool,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return complete_temporal_plain_year_month_difference(
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
            "Temporal.PlainYearMonth.prototype.until options must be an object",
        );
    }
    begin_temporal_plain_year_month_difference_get(
        runtime,
        TemporalPlainYearMonthDifferenceContinuation {
            receiver,
            other,
            options,
            largest_unit: None,
            rounding_increment: RoundingIncrement::ONE,
            rounding_mode: RoundingMode::Trunc,
            since,
            stage: TemporalPlainYearMonthDifferenceStage::LargestUnit,
            realm,
            origin,
        },
        "largestUnit",
        TemporalPlainYearMonthDifferenceStage::LargestUnit,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "each year-month difference option Get owns the native call state across suspension"
)]
fn begin_temporal_plain_year_month_difference_get(
    runtime: &mut Runtime,
    mut state: TemporalPlainYearMonthDifferenceContinuation,
    name: &str,
    next_stage: TemporalPlainYearMonthDifferenceStage,
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
        temporal_plain_year_month_difference_continuation,
        "Temporal.PlainYearMonth difference option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_plain_year_month_difference_options(
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

fn temporal_plain_year_month_difference_continuation(
    state: TemporalPlainYearMonthDifferenceContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainYearMonthDifferenceOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one ordered state table preserves year-month difference option observation across suspension"
)]
pub(in crate::vm) fn advance_temporal_plain_year_month_difference_options(
    runtime: &mut Runtime,
    state: TemporalPlainYearMonthDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalPlainYearMonthDifferenceStage::LargestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_year_month_difference_get(
                    runtime,
                    state,
                    "roundingIncrement",
                    TemporalPlainYearMonthDifferenceStage::RoundingIncrement,
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
                OperatorPrimitiveTarget::TemporalPlainYearMonthDifferenceLargestUnit(Box::new(
                    state,
                )),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainYearMonthDifferenceStage::RoundingIncrement => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_year_month_difference_get(
                    runtime,
                    state,
                    "roundingMode",
                    TemporalPlainYearMonthDifferenceStage::RoundingMode,
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
                OperatorPrimitiveTarget::TemporalPlainYearMonthDifferenceRoundingIncrement(
                    Box::new(state),
                ),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainYearMonthDifferenceStage::RoundingMode => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_plain_year_month_difference_get(
                    runtime,
                    state,
                    "smallestUnit",
                    TemporalPlainYearMonthDifferenceStage::SmallestUnit,
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
                OperatorPrimitiveTarget::TemporalPlainYearMonthDifferenceRoundingMode(Box::new(
                    state,
                )),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalPlainYearMonthDifferenceStage::SmallestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return complete_temporal_plain_year_month_difference(
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
                OperatorPrimitiveTarget::TemporalPlainYearMonthDifferenceSmallestUnit(Box::new(
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

pub(in crate::vm) fn finish_temporal_plain_year_month_difference_largest_unit(
    runtime: &mut Runtime,
    mut state: TemporalPlainYearMonthDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.largest_unit = Some(temporal_round_unit(&source, state.realm, &state.origin)?);
    begin_temporal_plain_year_month_difference_get(
        runtime,
        state,
        "roundingIncrement",
        TemporalPlainYearMonthDifferenceStage::RoundingIncrement,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_year_month_difference_rounding_increment(
    runtime: &mut Runtime,
    mut state: TemporalPlainYearMonthDifferenceContinuation,
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
    begin_temporal_plain_year_month_difference_get(
        runtime,
        state,
        "roundingMode",
        TemporalPlainYearMonthDifferenceStage::RoundingMode,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_year_month_difference_rounding_mode(
    runtime: &mut Runtime,
    mut state: TemporalPlainYearMonthDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.rounding_mode = temporal_rounding_mode(&source, state.realm, &state.origin)?;
    begin_temporal_plain_year_month_difference_get(
        runtime,
        state,
        "smallestUnit",
        TemporalPlainYearMonthDifferenceStage::SmallestUnit,
        return_to,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_year_month_difference_smallest_unit(
    runtime: &mut Runtime,
    state: &TemporalPlainYearMonthDifferenceContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let smallest_unit = temporal_round_unit(&source, state.realm, &state.origin)?;
    complete_temporal_plain_year_month_difference(
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
fn complete_temporal_plain_year_month_difference(
    runtime: &mut Runtime,
    receiver: &PlainYearMonth,
    other: &PlainYearMonth,
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
    reason = "the ordered PlainYearMonth calendar formatting reader owns its resumable native call context"
)]
fn begin_temporal_plain_year_month_to_string(
    runtime: &mut Runtime,
    year_month: PlainYearMonth,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return complete_temporal_plain_year_month_to_string(&year_month, DisplayCalendar::Auto);
    }
    if options.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.PlainYearMonth.prototype.toString options must be an object",
        );
    }
    begin_temporal_plain_year_month_to_string_get(
        runtime,
        TemporalPlainYearMonthToStringContinuation {
            year_month,
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
    reason = "the observable PlainYearMonth calendarName Get retains native call state"
)]
fn begin_temporal_plain_year_month_to_string_get(
    runtime: &mut Runtime,
    state: TemporalPlainYearMonthToStringContinuation,
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
        temporal_plain_year_month_to_string_continuation,
        "Temporal.PlainYearMonth toString calendarName Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_plain_year_month_to_string(
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

fn temporal_plain_year_month_to_string_continuation(
    state: TemporalPlainYearMonthToStringContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainYearMonthToStringOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the PlainYearMonth calendarName option may complete through observable primitive conversion"
)]
pub(in crate::vm) fn advance_temporal_plain_year_month_to_string(
    runtime: &mut Runtime,
    state: TemporalPlainYearMonthToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(value, StoredValue::Undefined) {
        return complete_temporal_plain_year_month_to_string(
            &state.year_month,
            DisplayCalendar::Auto,
        );
    }
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::String,
        OperatorPrimitiveTarget::TemporalPlainYearMonthToStringCalendarName(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(in crate::vm) fn finish_temporal_plain_year_month_to_string_calendar_name(
    state: &TemporalPlainYearMonthToStringContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let display_calendar = temporal_display_calendar(&source, state.realm, &state.origin)?;
    complete_temporal_plain_year_month_to_string(&state.year_month, display_calendar)
}

fn complete_temporal_plain_year_month_to_string(
    year_month: &PlainYearMonth,
    display_calendar: DisplayCalendar,
) -> Result<NativeDispatch, NativeFailure> {
    Ok(NativeDispatch::Immediate(StoredValue::String(
        JsString::from_utf8(&year_month.to_ixdtf_string(display_calendar))?,
    )))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the native method owns the observable day field conversion"
)]
fn begin_temporal_plain_year_month_to_plain_date(
    runtime: &mut Runtime,
    year_month: PlainYearMonth,
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
            "Temporal.PlainYearMonth.toPlainDate requires a property bag",
        );
    }
    advance_temporal_plain_year_month_to_plain_date(
        runtime,
        TemporalPlainYearMonthToPlainDateContinuation {
            year_month,
            fields,
            stage: TemporalPlainYearMonthToPlainDateStage::ReadDay,
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
    reason = "the explicit state machine preserves the day Get and ToNumber order"
)]
pub(in crate::vm) fn advance_temporal_plain_year_month_to_plain_date(
    runtime: &mut Runtime,
    mut state: TemporalPlainYearMonthToPlainDateContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalPlainYearMonthToPlainDateStage::ReadDay => {
                charge_heap_property_lookup(runtime, &state.fields, execution_budget)?;
                let name = JsString::from_utf8("day")?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalPlainYearMonthToPlainDateStage::AwaitDay;
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
                    temporal_plain_year_month_to_plain_date_continuation,
                    "Temporal.PlainYearMonth.toPlainDate day Get produced a structured result",
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
            TemporalPlainYearMonthToPlainDateStage::AwaitDay => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.PlainYearMonth.toPlainDate day Get resumed without a value",
                })?;
                if matches!(value, StoredValue::Undefined) {
                    return temporal_type_error(
                        state.realm,
                        &state.origin,
                        "Temporal.PlainYearMonth.toPlainDate requires a day",
                    );
                }
                state.stage = TemporalPlainYearMonthToPlainDateStage::AwaitDayConversion;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::TemporalPlainYearMonthToPlainDate(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalPlainYearMonthToPlainDateStage::AwaitDayConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message:
                        "Temporal.PlainYearMonth.toPlainDate day conversion resumed without a value",
                })?;
                let day = operator_to_number(value, state.realm, &state.origin)?;
                let day = temporal_plain_date_integer(day, "day", state.realm, &state.origin)?;
                let Ok(day) = u8::try_from(day) else {
                    return Err(NativeFailure::Abrupt(temporal_pending_exception(
                        state.realm,
                        &state.origin,
                        ExceptionKind::RangeError,
                        "Temporal.PlainYearMonth.toPlainDate day is outside the supported range",
                    )?));
                };
                let date = match state
                    .year_month
                    .to_plain_date(Some(CalendarFields::new().with_day(day)))
                {
                    Ok(date) => date,
                    Err(error) => {
                        return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                            state.realm,
                            &state.origin,
                            error,
                        )?));
                    }
                };
                return allocate_temporal_plain_date_result(runtime, state.realm, date);
            }
        }
    }
}

fn temporal_plain_year_month_to_plain_date_continuation(
    state: TemporalPlainYearMonthToPlainDateContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalPlainYearMonthToPlainDate(Box::new(state))
}
