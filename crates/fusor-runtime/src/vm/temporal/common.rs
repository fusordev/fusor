#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use core::str::FromStr;
use temporal_rs::{
    Calendar, Duration, Instant, PlainDate, PlainDateTime, PlainMonthDay, PlainTime,
    PlainYearMonth, TimeZone, TinyAsciiStr, ZonedDateTime,
    error::ErrorKind as TemporalErrorKind,
    options::{DisplayCalendar, RoundingMode, Unit},
    parsers::Precision,
};

pub(in crate::vm) fn temporal_calendar_era(
    source: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<TinyAsciiStr<19>, NativeFailure> {
    let source = source.to_utf8_lossy()?;
    match TinyAsciiStr::<19>::try_from_utf8(source.as_bytes()) {
        Ok(era) => Ok(era),
        Err(_) => Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "invalid Temporal calendar era",
        )?)),
    }
}

pub(in crate::vm) fn temporal_calendar_supports_eras(calendar: &Calendar) -> bool {
    !matches!(calendar.identifier(), "iso8601" | "chinese" | "dangi")
}

pub(in crate::vm) fn temporal_calendar_from_string(
    source: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Calendar, NativeFailure> {
    let source = source.to_utf8_lossy()?;
    let lowercase = source.to_ascii_lowercase();
    if lowercase == "islamic"
        || lowercase.contains("[u-ca=islamic]")
        || lowercase.contains("[!u-ca=islamic]")
    {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "the islamic calendar identifier is not supported by Temporal",
        )?));
    }
    match Calendar::from_str(&source) {
        Ok(calendar) => Ok(calendar),
        Err(error) => Err(NativeFailure::Abrupt(temporal_exception_from_error(
            realm, origin, error,
        )?)),
    }
}

pub(in crate::vm) fn temporal_zoned_date_time_time_zone_from_value(
    runtime: &Runtime,
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<TimeZone, NativeFailure> {
    match value {
        StoredValue::String(value) => match TimeZone::try_from_str(&value.to_utf8_lossy()?) {
            Ok(time_zone) => Ok(time_zone),
            Err(error) => Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?)),
        },
        StoredValue::Object(object) => {
            if let Some(value) = runtime.temporal_zoned_date_time(object)? {
                return Ok(*value.time_zone());
            }
            Err(NativeFailure::Abrupt(temporal_pending_exception(
                realm,
                origin,
                ExceptionKind::TypeError,
                "Temporal.ZonedDateTime timeZone must be a string or ZonedDateTime",
            )?))
        }
        _ => Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Temporal.ZonedDateTime timeZone must be a string or ZonedDateTime",
        )?)),
    }
}

pub(in crate::vm) fn allocate_temporal_zoned_date_time_result(
    runtime: &mut Runtime,
    realm: RealmId,
    date_time: ZonedDateTime,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = HeapReference::Object(runtime.realm_temporal_zoned_date_time_prototype(realm)?);
    let object = runtime.allocate_temporal_zoned_date_time(prototype, date_time)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(in crate::vm) fn allocate_temporal_plain_date_result(
    runtime: &mut Runtime,
    realm: RealmId,
    date: PlainDate,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = HeapReference::Object(runtime.realm_temporal_plain_date_prototype(realm)?);
    let object = runtime.allocate_temporal_plain_date(prototype, date)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(in crate::vm) fn allocate_temporal_plain_date_time_result(
    runtime: &mut Runtime,
    realm: RealmId,
    date_time: PlainDateTime,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = HeapReference::Object(runtime.realm_temporal_plain_date_time_prototype(realm)?);
    let object = runtime.allocate_temporal_plain_date_time(prototype, date_time)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(in crate::vm) fn allocate_temporal_plain_time_result(
    runtime: &mut Runtime,
    realm: RealmId,
    time: PlainTime,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = HeapReference::Object(runtime.realm_temporal_plain_time_prototype(realm)?);
    let object = runtime.allocate_temporal_plain_time(prototype, time)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(in crate::vm) fn require_temporal_plain_date(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<PlainDate, NativeFailure> {
    if let StoredValue::Object(object) = receiver
        && let Some(date) = runtime.temporal_plain_date(*object)?
    {
        return Ok(date);
    }
    Err(NativeFailure::Abrupt(temporal_pending_exception(
        realm,
        origin,
        ExceptionKind::TypeError,
        "not a Temporal.PlainDate object",
    )?))
}

pub(in crate::vm) fn temporal_calendar_from_object(
    runtime: &Runtime,
    value: &StoredValue,
) -> Result<Option<Calendar>, NativeFailure> {
    let StoredValue::Object(object) = value else {
        return Ok(None);
    };
    if let Some(date) = runtime.temporal_plain_date(*object)? {
        return Ok(Some(date.calendar().clone()));
    }
    if let Some(date_time) = runtime.temporal_plain_date_time(*object)? {
        return Ok(Some(date_time.calendar().clone()));
    }
    if let Some(month_day) = runtime.temporal_plain_month_day(*object)? {
        return Ok(Some(month_day.calendar().clone()));
    }
    if let Some(year_month) = runtime.temporal_plain_year_month(*object)? {
        return Ok(Some(year_month.calendar().clone()));
    }
    if let Some(date_time) = runtime.temporal_zoned_date_time(*object)? {
        return Ok(Some(date_time.calendar().clone()));
    }
    Ok(None)
}

pub(in crate::vm) fn allocate_temporal_plain_month_day_result(
    runtime: &mut Runtime,
    realm: RealmId,
    month_day: PlainMonthDay,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = runtime.realm_temporal_plain_month_day_prototype(realm)?;
    let object =
        runtime.allocate_temporal_plain_month_day(HeapReference::Object(prototype), month_day)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(in crate::vm) fn allocate_temporal_plain_year_month_result(
    runtime: &mut Runtime,
    realm: RealmId,
    year_month: PlainYearMonth,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = runtime.realm_temporal_plain_year_month_prototype(realm)?;
    let object =
        runtime.allocate_temporal_plain_year_month(HeapReference::Object(prototype), year_month)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(in crate::vm) fn require_temporal_plain_month_day(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<PlainMonthDay, NativeFailure> {
    if let StoredValue::Object(object) = receiver
        && let Some(month_day) = runtime.temporal_plain_month_day(*object)?
    {
        return Ok(month_day);
    }
    Err(NativeFailure::Abrupt(temporal_pending_exception(
        realm,
        origin,
        ExceptionKind::TypeError,
        "not a Temporal.PlainMonthDay object",
    )?))
}

pub(in crate::vm) fn require_temporal_plain_year_month(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<PlainYearMonth, NativeFailure> {
    if let StoredValue::Object(object) = receiver
        && let Some(year_month) = runtime.temporal_plain_year_month(*object)?
    {
        return Ok(year_month);
    }
    Err(NativeFailure::Abrupt(temporal_pending_exception(
        realm,
        origin,
        ExceptionKind::TypeError,
        "not a Temporal.PlainYearMonth object",
    )?))
}

pub(in crate::vm) fn require_temporal_plain_time(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<PlainTime, NativeFailure> {
    if let StoredValue::Object(object) = receiver
        && let Some(time) = runtime.temporal_plain_time(*object)?
    {
        return Ok(time);
    }
    Err(NativeFailure::Abrupt(temporal_pending_exception(
        realm,
        origin,
        ExceptionKind::TypeError,
        "not a Temporal.PlainTime object",
    )?))
}

pub(in crate::vm) fn require_temporal_plain_date_time(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<PlainDateTime, NativeFailure> {
    if let StoredValue::Object(object) = receiver
        && let Some(date_time) = runtime.temporal_plain_date_time(*object)?
    {
        return Ok(date_time);
    }
    Err(NativeFailure::Abrupt(temporal_pending_exception(
        realm,
        origin,
        ExceptionKind::TypeError,
        "not a Temporal.PlainDateTime object",
    )?))
}

pub(in crate::vm) fn require_temporal_zoned_date_time(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<ZonedDateTime, NativeFailure> {
    if let StoredValue::Object(object) = receiver
        && let Some(date_time) = runtime.temporal_zoned_date_time(*object)?
    {
        return Ok(date_time);
    }
    Err(NativeFailure::Abrupt(temporal_pending_exception(
        realm,
        origin,
        ExceptionKind::TypeError,
        "not a Temporal.ZonedDateTime object",
    )?))
}

pub(in crate::vm) fn temporal_round_unit(
    source: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Unit, NativeFailure> {
    let source = source.to_utf8_lossy()?;
    match source.parse::<Unit>() {
        Ok(unit) => Ok(unit),
        Err(_) => Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "invalid Temporal unit",
        )?)),
    }
}

pub(in crate::vm) fn temporal_rounding_mode(
    source: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<RoundingMode, NativeFailure> {
    let source = source.to_utf8_lossy()?;
    let mode = match source.as_str() {
        "ceil" => RoundingMode::Ceil,
        "floor" => RoundingMode::Floor,
        "expand" => RoundingMode::Expand,
        "trunc" => RoundingMode::Trunc,
        "halfCeil" => RoundingMode::HalfCeil,
        "halfFloor" => RoundingMode::HalfFloor,
        "halfExpand" => RoundingMode::HalfExpand,
        "halfTrunc" => RoundingMode::HalfTrunc,
        "halfEven" => RoundingMode::HalfEven,
        _ => {
            return Err(NativeFailure::Abrupt(temporal_pending_exception(
                realm,
                origin,
                ExceptionKind::RangeError,
                "invalid Temporal roundingMode",
            )?));
        }
    };
    Ok(mode)
}

pub(in crate::vm) fn temporal_duration_unit(
    source: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Unit, NativeFailure> {
    let source = source.to_utf8_lossy()?;
    let Ok(unit) = source.parse::<Unit>() else {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "invalid Temporal unit",
        )?));
    };
    if unit == Unit::Auto {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "auto is not a valid Temporal unit here",
        )?));
    }
    Ok(unit)
}

pub(in crate::vm) fn allocate_temporal_duration_result(
    runtime: &mut Runtime,
    realm: RealmId,
    duration: Duration,
) -> Result<NativeDispatch, NativeFailure> {
    let duration = normalize_temporal_duration_fields(duration)?;
    let prototype = HeapReference::Object(runtime.realm_temporal_duration_prototype(realm)?);
    let object = runtime.allocate_temporal_duration(prototype, duration)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(in crate::vm) fn require_temporal_duration(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Duration, NativeFailure> {
    if let StoredValue::Object(object) = receiver
        && let Some(duration) = runtime.temporal_duration(*object)?
    {
        return Ok(duration);
    }
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a Temporal.Duration object")?,
        },
        origin: origin.clone(),
    }))
}

pub(in crate::vm) fn allocate_temporal_instant_result(
    runtime: &mut Runtime,
    realm: RealmId,
    instant: Instant,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = HeapReference::Object(runtime.realm_temporal_instant_prototype(realm)?);
    let object = runtime.allocate_temporal_instant(prototype, instant)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(in crate::vm) fn temporal_display_calendar(
    source: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<DisplayCalendar, NativeFailure> {
    match source.to_utf8_lossy()?.parse::<DisplayCalendar>() {
        Ok(display_calendar) => Ok(display_calendar),
        Err(_) => Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "invalid Temporal calendarName",
        )?)),
    }
}

pub(in crate::vm) fn temporal_fractional_second_digits(
    value: JsNumber,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Precision, NativeFailure> {
    let value = value.as_f64();
    if !value.is_finite() {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "fractionalSecondDigits must be finite",
        )?));
    }
    let digits = value.floor();
    if !(0.0..=9.0).contains(&digits) {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "fractionalSecondDigits must be between zero and nine",
        )?));
    }
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "the validated integer digit count is in the inclusive u8 range zero through nine"
    )]
    let digits = digits as u8;
    Ok(Precision::Digit(digits))
}

pub(in crate::vm) fn temporal_range_exception_from_error(
    realm: RealmId,
    origin: &JsStackFrame,
    error: temporal_rs::TemporalError,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::RangeError,
            message: JsString::from_utf8(error.into_message())?,
        },
        origin: origin.clone(),
    })
}

pub(in crate::vm) fn require_temporal_instant(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Instant, NativeFailure> {
    if let StoredValue::Object(object) = receiver
        && let Some(instant) = runtime.temporal_instant(*object)?
    {
        return Ok(instant);
    }
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a Temporal.Instant object")?,
        },
        origin: origin.clone(),
    }))
}

pub(in crate::vm) fn temporal_exception_from_error(
    realm: RealmId,
    origin: &JsStackFrame,
    error: temporal_rs::TemporalError,
) -> Result<PendingException, NativeFailure> {
    let kind = match error.kind() {
        TemporalErrorKind::Type => ExceptionKind::TypeError,
        TemporalErrorKind::Syntax => ExceptionKind::SyntaxError,
        TemporalErrorKind::Generic | TemporalErrorKind::Range | TemporalErrorKind::Assert => {
            ExceptionKind::RangeError
        }
    };
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(error.into_message())?,
        },
        origin: origin.clone(),
    })
}

pub(in crate::vm) fn temporal_type_error(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    temporal_exception(realm, origin, ExceptionKind::TypeError, message)
}

pub(in crate::vm) fn temporal_range_error(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    temporal_exception(realm, origin, ExceptionKind::RangeError, message)
}

fn temporal_exception(
    realm: RealmId,
    origin: &JsStackFrame,
    kind: ExceptionKind,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(temporal_pending_exception(
        realm, origin, kind, message,
    )?))
}

pub(in crate::vm) fn temporal_pending_exception(
    realm: RealmId,
    origin: &JsStackFrame,
    kind: ExceptionKind,
    message: &str,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    })
}
