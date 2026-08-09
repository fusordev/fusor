//! ECMA-402 `DateTimeFormat` option resolution and ICU4X-backed rendering.

use core::fmt;

use icu::{
    datetime::{
        DateTimeFormatter, DateTimeFormatterPreferences,
        fieldsets::{
            builder::{DateFields, FieldSetBuilder, ZoneStyle},
            enums::CompositeFieldSet,
        },
        input::{Date, Time, TimeZone, ZonedDateTime},
        options::{Alignment, Length, SubsecondDigits, TimePrecision, YearStyle},
    },
    locale::Locale,
    time::zone::{UtcOffset, ZoneNameTimestamp, iana::IanaParserExtended},
};
use writeable::{Part, PartsWrite, Writeable};

use crate::{
    InvalidLocale, calendars_of_locale, canonicalize_locale, hour_cycles_of_locale,
    locale_components, number_format::numbering_system_digits, numbering_systems_of_locale,
    supported_values,
};

const DEFAULT_LOCALE: &str = "en-US";
const DEFAULT_TIME_ZONE: &str = "UTC";

macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $($variant:ident => $value:literal),+ $(,)? }
        default $default:ident
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum $name { $($variant),+ }

        impl Default for $name {
            fn default() -> Self { Self::$default }
        }

        impl $name {
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }
        }
    };
}

string_enum! {
    /// A component value from ECMA-402 Table 16.
    pub enum DateTimeComponentStyle {
        Numeric => "numeric",
        TwoDigit => "2-digit",
        Narrow => "narrow",
        Short => "short",
        Long => "long"
    }
    default Numeric
}

string_enum! {
    /// A resolved Unicode hour-cycle identifier.
    pub enum DateTimeHourCycle {
        H11 => "h11",
        H12 => "h12",
        H23 => "h23",
        H24 => "h24"
    }
    default H12
}

impl DateTimeHourCycle {
    #[must_use]
    pub const fn is_hour12(self) -> bool {
        matches!(self, Self::H11 | Self::H12)
    }
}

string_enum! {
    /// A `dateStyle` or `timeStyle` value.
    pub enum DateTimeStyle {
        Full => "full",
        Long => "long",
        Medium => "medium",
        Short => "short"
    }
    default Medium
}

string_enum! {
    /// A `timeZoneName` component value.
    pub enum DateTimeTimeZoneName {
        Short => "short",
        Long => "long",
        ShortOffset => "shortOffset",
        LongOffset => "longOffset",
        ShortGeneric => "shortGeneric",
        LongGeneric => "longGeneric"
    }
    default Short
}

string_enum! {
    /// The requested format-matching algorithm.
    pub enum DateTimeFormatMatcher {
        Basic => "basic",
        BestFit => "best fit"
    }
    default BestFit
}

/// Construction or formatting failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DateTimeFormatError {
    InvalidLocale,
    InvalidOption,
    InvalidTimeZone,
    InvalidDateTime,
    NoFields,
    Data,
}

impl From<InvalidLocale> for DateTimeFormatError {
    fn from(_: InvalidLocale) -> Self {
        Self::InvalidLocale
    }
}

/// Already-coerced JavaScript options in normative observation order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DateTimeFormatRequestOptions {
    pub calendar: Option<String>,
    pub numbering_system: Option<String>,
    pub hour12: Option<bool>,
    pub hour_cycle: Option<DateTimeHourCycle>,
    pub time_zone: Option<String>,
    pub weekday: Option<DateTimeComponentStyle>,
    pub era: Option<DateTimeComponentStyle>,
    pub year: Option<DateTimeComponentStyle>,
    pub month: Option<DateTimeComponentStyle>,
    pub day: Option<DateTimeComponentStyle>,
    pub day_period: Option<DateTimeComponentStyle>,
    pub hour: Option<DateTimeComponentStyle>,
    pub minute: Option<DateTimeComponentStyle>,
    pub second: Option<DateTimeComponentStyle>,
    pub fractional_second_digits: Option<u8>,
    pub time_zone_name: Option<DateTimeTimeZoneName>,
    pub format_matcher: Option<DateTimeFormatMatcher>,
    pub date_style: Option<DateTimeStyle>,
    pub time_style: Option<DateTimeStyle>,
}

impl DateTimeFormatRequestOptions {
    #[must_use]
    pub const fn has_explicit_components(&self) -> bool {
        self.weekday.is_some()
            || self.era.is_some()
            || self.year.is_some()
            || self.month.is_some()
            || self.day.is_some()
            || self.day_period.is_some()
            || self.hour.is_some()
            || self.minute.is_some()
            || self.second.is_some()
            || self.fractional_second_digits.is_some()
            || self.time_zone_name.is_some()
    }
}

/// Fully resolved immutable `DateTimeFormat` internal slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DateTimeFormatState {
    pub locale: String,
    pub calendar: String,
    pub numbering_system: String,
    pub time_zone: String,
    pub hour_cycle: DateTimeHourCycle,
    pub weekday: Option<DateTimeComponentStyle>,
    pub era: Option<DateTimeComponentStyle>,
    pub year: Option<DateTimeComponentStyle>,
    pub month: Option<DateTimeComponentStyle>,
    pub day: Option<DateTimeComponentStyle>,
    pub day_period: Option<DateTimeComponentStyle>,
    pub hour: Option<DateTimeComponentStyle>,
    pub minute: Option<DateTimeComponentStyle>,
    pub second: Option<DateTimeComponentStyle>,
    pub fractional_second_digits: Option<u8>,
    pub time_zone_name: Option<DateTimeTimeZoneName>,
    pub format_matcher: DateTimeFormatMatcher,
    pub date_style: Option<DateTimeStyle>,
    pub time_style: Option<DateTimeStyle>,
    /// Whether type-specific default fields must be selected for Temporal
    /// inputs. `resolvedOptions()` still exposes the legacy date defaults.
    pub default_components: bool,
}

impl DateTimeFormatState {
    #[must_use]
    pub const fn has_hour(&self) -> bool {
        self.hour.is_some() || self.time_style.is_some()
    }

    #[must_use]
    pub const fn hour12(&self) -> bool {
        self.hour_cycle.is_hour12()
    }
}

/// The date/time data model used by the format operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DateTimeFormatInputKind {
    /// A legacy Date or Temporal.Instant projected through the formatter zone.
    Epoch,
    Instant,
    PlainDateTime,
    PlainDate,
    PlainYearMonth,
    PlainMonthDay,
    PlainTime,
}

/// An ISO local date/time plus zone metadata already resolved by the runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DateTimeFormatInput {
    pub kind: DateTimeFormatInputKind,
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub nanosecond: u32,
    pub offset_seconds: i32,
    pub epoch_seconds: i64,
}

/// One ECMA-402 date-time part.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DateTimeFormatPart {
    pub kind: &'static str,
    pub value: String,
}

/// Resolves requested locales and options into `DateTimeFormat` internal slots.
///
/// JavaScript property access and coercion happen before this pure operation.
///
/// # Errors
///
/// Returns an error for an invalid time zone or a conflicting style/component
/// request.
#[allow(
    clippy::too_many_lines,
    reason = "DateTimeFormat slot resolution keeps the interacting locale extensions and option precedence in one auditable operation"
)]
pub fn resolve_date_time_format(
    requested: &[String],
    mut options: DateTimeFormatRequestOptions,
) -> Result<DateTimeFormatState, DateTimeFormatError> {
    if (options.date_style.is_some() || options.time_style.is_some())
        && options.has_explicit_components()
    {
        return Err(DateTimeFormatError::InvalidOption);
    }
    if options
        .fractional_second_digits
        .is_some_and(|digits| !(1..=3).contains(&digits))
    {
        return Err(DateTimeFormatError::InvalidOption);
    }

    let requested_locale = requested
        .iter()
        .find(|locale| locale_is_supported(locale))
        .map_or(DEFAULT_LOCALE, String::as_str);
    let components = locale_components(requested_locale)?;
    let base = components.base_name;

    let extension_calendar = components
        .calendar
        .filter(|value| calendar_is_supported(value));
    let option_calendar = options
        .calendar
        .take()
        .filter(|value| calendar_is_supported(value));
    let calendar = option_calendar
        .as_ref()
        .or(extension_calendar.as_ref())
        .cloned()
        .unwrap_or(
            calendars_of_locale(&base)?
                .into_iter()
                .next()
                .unwrap_or_else(|| "gregory".to_owned()),
        );

    let extension_numbering = components
        .numbering_system
        .filter(|value| numbering_system_is_supported(value));
    let option_numbering = options
        .numbering_system
        .take()
        .filter(|value| numbering_system_is_supported(value));
    let numbering_system = option_numbering
        .as_ref()
        .or(extension_numbering.as_ref())
        .cloned()
        .unwrap_or(
            numbering_systems_of_locale(&base)?
                .into_iter()
                .next()
                .unwrap_or_else(|| "latn".to_owned()),
        );

    let hour_cycle_preferences = hour_cycles_of_locale(&base)?;
    let default_hour_cycle = hour_cycle_preferences
        .iter()
        .find_map(|value| parse_hour_cycle(value))
        .unwrap_or(DateTimeHourCycle::H23);
    let extension_hour_cycle = components.hour_cycle.as_deref().and_then(parse_hour_cycle);
    let hour_cycle = match options.hour12 {
        Some(true) => hour_cycle_preferences
            .iter()
            .filter_map(|value| parse_hour_cycle(value))
            .find(|cycle| cycle.is_hour12())
            .unwrap_or(DateTimeHourCycle::H12),
        Some(false) => hour_cycle_preferences
            .iter()
            .filter_map(|value| parse_hour_cycle(value))
            .find(|cycle| !cycle.is_hour12())
            .unwrap_or(DateTimeHourCycle::H23),
        None => options
            .hour_cycle
            .or(extension_hour_cycle)
            .unwrap_or(default_hour_cycle),
    };

    let retain_calendar = extension_calendar.as_ref().is_some_and(|extension| {
        extension == &calendar
            && option_calendar
                .as_ref()
                .is_none_or(|option| option == extension)
    });
    let retain_hour_cycle = options.hour12.is_none()
        && extension_hour_cycle.is_some_and(|extension| {
            extension == hour_cycle && options.hour_cycle.is_none_or(|option| option == extension)
        });
    let retain_numbering = extension_numbering.as_ref().is_some_and(|extension| {
        extension == &numbering_system
            && option_numbering
                .as_ref()
                .is_none_or(|option| option == extension)
    });
    let mut retained = Vec::new();
    if retain_calendar {
        retained.push(format!("ca-{calendar}"));
    }
    if retain_hour_cycle {
        retained.push(format!("hc-{}", hour_cycle.as_str()));
    }
    if retain_numbering {
        retained.push(format!("nu-{numbering_system}"));
    }
    let locale = if retained.is_empty() {
        base.clone()
    } else {
        canonicalize_locale(&format!("{base}-u-{}", retained.join("-")))?
    };

    let time_zone =
        canonicalize_time_zone(options.time_zone.as_deref().unwrap_or(DEFAULT_TIME_ZONE))?;

    // `era` and `timeZoneName` do not by themselves suppress the service's
    // type-specific defaults. This is the `ToDateTimeOptions` defaulting
    // boundary exercised by current Temporal-aware ECMA-402 tests.
    let default_components = options.date_style.is_none()
        && options.time_style.is_none()
        && options.weekday.is_none()
        && options.year.is_none()
        && options.month.is_none()
        && options.day.is_none()
        && options.day_period.is_none()
        && options.hour.is_none()
        && options.minute.is_none()
        && options.second.is_none()
        && options.fractional_second_digits.is_none();
    if default_components {
        options.year = Some(DateTimeComponentStyle::Numeric);
        options.month = Some(DateTimeComponentStyle::Numeric);
        options.day = Some(DateTimeComponentStyle::Numeric);
    }

    Ok(DateTimeFormatState {
        locale,
        calendar,
        numbering_system,
        time_zone,
        hour_cycle,
        weekday: options.weekday,
        era: options.era,
        year: options.year,
        month: options.month,
        day: options.day,
        day_period: options.day_period,
        hour: options.hour,
        minute: options.minute,
        second: options.second,
        fractional_second_digits: options.fractional_second_digits,
        time_zone_name: options.time_zone_name,
        format_matcher: options.format_matcher.unwrap_or_default(),
        date_style: options.date_style,
        time_style: options.time_style,
        default_components,
    })
}

/// Returns requested locales supported by the ICU4X `DateTimeFormat` profile.
#[must_use]
pub fn date_time_format_supported_locales(requested: &[String]) -> Vec<String> {
    requested
        .iter()
        .filter(|locale| locale_is_supported(locale))
        .cloned()
        .collect()
}

/// Validates and case-normalizes an ECMA-402 time-zone identifier.
///
/// Offset identifiers are normalized to `+HH:MM`/`-HH:MM`; named identifiers
/// retain the case-normalized IANA identifier rather than being replaced by a
/// link target, matching current `AvailableNamedTimeZoneIdentifiers` semantics.
///
/// # Errors
///
/// Returns [`DateTimeFormatError::InvalidTimeZone`] for an unknown named zone
/// or malformed offset identifier.
pub fn canonicalize_time_zone(input: &str) -> Result<String, DateTimeFormatError> {
    if let Some((offset, normalized)) = parse_offset_time_zone(input) {
        let _ = offset;
        return Ok(normalized);
    }
    if input.starts_with(['+', '-']) {
        return Err(DateTimeFormatError::InvalidTimeZone);
    }
    if input.eq_ignore_ascii_case("UTC") {
        return Ok("UTC".to_owned());
    }
    let result = IanaParserExtended::new().parse(input);
    if result.time_zone == TimeZone::UNKNOWN {
        return Err(DateTimeFormatError::InvalidTimeZone);
    }
    Ok(result.normalized.to_owned())
}

/// Formats one already-projected input.
///
/// # Errors
///
/// Returns an error if the input has no fields overlapping this formatter or
/// if ICU data cannot format the resolved request.
pub fn format_datetime(
    state: &DateTimeFormatState,
    input: &DateTimeFormatInput,
) -> Result<String, DateTimeFormatError> {
    Ok(format_datetime_to_parts(state, input)?
        .into_iter()
        .map(|part| part.value)
        .collect())
}

/// Formats one already-projected input to ECMA-402 parts.
///
/// # Errors
///
/// Returns an error if the input has no overlapping fields or contains an
/// invalid ISO date/time.
pub fn format_datetime_to_parts(
    state: &DateTimeFormatState,
    input: &DateTimeFormatInput,
) -> Result<Vec<DateTimeFormatPart>, DateTimeFormatError> {
    if state.date_style.is_some() && state.time_style.is_some() {
        return format_combined_style_to_parts(state, input);
    }
    let pattern = match effective_pattern(state, input.kind) {
        Ok(pattern) => pattern,
        Err(DateTimeFormatError::Data) if has_mixed_explicit_components(state) => {
            return format_mixed_components_to_parts(state, input);
        }
        Err(error) => return Err(error),
    };
    if let Some(parts) = format_special_time_parts(state, input) {
        return Ok(parts);
    }
    let prefs = formatter_preferences(state)?;
    let formatter = match DateTimeFormatter::<CompositeFieldSet>::try_new(prefs, pattern.field_set)
    {
        Ok(formatter) => formatter,
        Err(_) if has_mixed_explicit_components(state) => {
            return format_mixed_components_to_parts(state, input);
        }
        Err(_) => return Err(DateTimeFormatError::Data),
    };
    let (date, shifted_year) =
        if let Ok(date) = Date::try_new_iso(input.year, input.month, input.day) {
            (date, false)
        } else {
            // ICU's ISO date storage is intentionally narrower than the
            // Temporal/ECMAScript proleptic-Gregorian range. Shift by a whole
            // number of 400-year cycles so month, leap-day, and weekday stay
            // identical, then restore the observable year and era parts.
            let proxy_year = 2_000 + input.year.rem_euclid(400);
            let date = Date::try_new_iso(proxy_year, input.month, input.day)
                .map_err(|_| DateTimeFormatError::InvalidDateTime)?;
            (date, true)
        };
    let time = Time::try_new(input.hour, input.minute, input.second, input.nanosecond)
        .map_err(|_| DateTimeFormatError::InvalidDateTime)?;
    let zone_id = time_zone_id(&state.time_zone);
    // ECMA-402 admits offset identifiers through ±23:59, wider than ICU's
    // validation range. The already validated value is safe to carry here.
    let offset = UtcOffset::from_seconds_unchecked(input.offset_seconds);
    let zone = zone_id
        .with_offset(Some(offset))
        .with_zone_name_timestamp(ZoneNameTimestamp::from_epoch_seconds(input.epoch_seconds));
    let zoned = ZonedDateTime { date, time, zone };
    let output = formatter.format(&zoned);
    let mut parts = capture_parts(&output);
    if shifted_year {
        restore_proleptic_year_parts(state, input.year, &mut parts);
    }
    apply_flexible_day_period(state, input, &mut parts);
    normalize_date_time_parts(state, &mut parts);
    if state.hour_cycle == DateTimeHourCycle::H24 {
        for part in &mut parts {
            if part.kind == "hour" && matches!(part.value.as_str(), "0" | "00") {
                "24".clone_into(&mut part.value);
            }
        }
    }
    normalize_component_widths(state, &mut parts);
    transliterate_parts(&mut parts, &state.numbering_system);
    Ok(parts)
}

fn has_mixed_explicit_components(state: &DateTimeFormatState) -> bool {
    let has_date = state.weekday.is_some()
        || state.era.is_some()
        || state.year.is_some()
        || state.month.is_some()
        || state.day.is_some();
    let has_time = state.day_period.is_some()
        || state.hour.is_some()
        || state.minute.is_some()
        || state.second.is_some()
        || state.fractional_second_digits.is_some()
        || state.time_zone_name.is_some();
    has_date && has_time
}

fn format_mixed_components_to_parts(
    state: &DateTimeFormatState,
    input: &DateTimeFormatInput,
) -> Result<Vec<DateTimeFormatPart>, DateTimeFormatError> {
    let mut date_state = state.clone();
    date_state.day_period = None;
    date_state.hour = None;
    date_state.minute = None;
    date_state.second = None;
    date_state.fractional_second_digits = None;
    date_state.time_zone_name = None;
    date_state.time_style = None;
    date_state.default_components = false;
    let date = format_datetime_to_parts(&date_state, input);

    let mut time_state = state.clone();
    time_state.weekday = None;
    time_state.era = None;
    time_state.year = None;
    time_state.month = None;
    time_state.day = None;
    time_state.date_style = None;
    time_state.default_components = false;
    let time = format_datetime_to_parts(&time_state, input);

    match (date, time) {
        (Ok(mut date), Ok(time)) => {
            date.push(DateTimeFormatPart {
                kind: "literal",
                value: ", ".to_owned(),
            });
            date.extend(time);
            Ok(date)
        }
        (Ok(date), Err(DateTimeFormatError::NoFields)) => Ok(date),
        (Err(DateTimeFormatError::NoFields), Ok(time)) => Ok(time),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

fn format_combined_style_to_parts(
    state: &DateTimeFormatState,
    input: &DateTimeFormatInput,
) -> Result<Vec<DateTimeFormatPart>, DateTimeFormatError> {
    let mut date_state = state.clone();
    date_state.time_style = None;
    let date = format_datetime_to_parts(&date_state, input);

    let mut time_state = state.clone();
    time_state.date_style = None;
    let time = format_datetime_to_parts(&time_state, input);

    match (date, time) {
        (Ok(mut date), Ok(time)) => {
            date.push(DateTimeFormatPart {
                kind: "literal",
                value: ", ".to_owned(),
            });
            date.extend(time);
            Ok(date)
        }
        (Ok(date), Err(DateTimeFormatError::NoFields)) => Ok(date),
        (Err(DateTimeFormatError::NoFields), Ok(time)) => Ok(time),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

fn normalize_date_time_parts(state: &DateTimeFormatState, parts: &mut [DateTimeFormatPart]) {
    if matches!(state.calendar.as_str(), "chinese" | "dangi") {
        for part in &mut *parts {
            if part.kind == "month"
                && let Some(month) = part.value.strip_prefix("Mo")
            {
                part.value = month.to_owned();
            }
        }
    }
    if matches!(state.numbering_system.as_str(), "arab" | "arabext") {
        for index in 1..parts.len().saturating_sub(1) {
            if parts[index].kind == "literal"
                && parts[index - 1].kind == "second"
                && parts[index + 1].kind == "fractionalSecond"
            {
                "٫".clone_into(&mut parts[index].value);
            }
        }
    }
    if state.locale == "en" || state.locale.starts_with("en-") {
        for index in 1..parts.len() {
            if parts[index].kind == "dayPeriod" && parts[index - 1].kind == "literal" {
                " ".clone_into(&mut parts[index - 1].value);
            }
        }
    }
}

fn restore_proleptic_year_parts(
    state: &DateTimeFormatState,
    iso_year: i32,
    parts: &mut [DateTimeFormatPart],
) {
    let display_year = if iso_year <= 0 {
        i64::from(1) - i64::from(iso_year)
    } else {
        i64::from(iso_year)
    };
    for part in parts {
        match part.kind {
            "year" | "relatedYear" => part.value = display_year.to_string(),
            "era" if iso_year <= 0 => {
                part.value = if state.era == Some(DateTimeComponentStyle::Narrow) {
                    "B".to_owned()
                } else {
                    "BC".to_owned()
                };
            }
            "era" => {
                part.value = if state.era == Some(DateTimeComponentStyle::Narrow) {
                    "A".to_owned()
                } else {
                    "AD".to_owned()
                };
            }
            _ => {}
        }
    }
}

fn format_special_time_parts(
    state: &DateTimeFormatState,
    input: &DateTimeFormatInput,
) -> Option<Vec<DateTimeFormatPart>> {
    let english = state.locale == "en" || state.locale.starts_with("en-");
    if english
        && state.day_period.is_some()
        && state.hour.is_none()
        && state.minute.is_none()
        && state.second.is_none()
        && state.fractional_second_digits.is_none()
        && !state.default_components
        && state.date_style.is_none()
        && state.time_style.is_none()
    {
        return Some(vec![DateTimeFormatPart {
            kind: "dayPeriod",
            value: flexible_english_day_period(input.hour, state.day_period.unwrap_or_default())
                .to_owned(),
        }]);
    }

    if english
        && state.weekday.is_none()
        && state.era.is_none()
        && state.year.is_none()
        && state.month.is_none()
        && state.day.is_none()
        && state.day_period.is_none()
        && state.hour.is_none()
        && state.minute.is_some()
        && state.second.is_some()
        && state.time_zone_name.is_none()
        && state.date_style.is_none()
        && state.time_style.is_none()
    {
        let mut parts = vec![
            DateTimeFormatPart {
                kind: "minute",
                value: format!("{:02}", input.minute),
            },
            DateTimeFormatPart {
                kind: "literal",
                value: ":".to_owned(),
            },
            DateTimeFormatPart {
                kind: "second",
                value: format!("{:02}", input.second),
            },
        ];
        if let Some(digits) = state.fractional_second_digits {
            let divisor = 10_u32.pow(9_u32.saturating_sub(u32::from(digits)));
            parts.push(DateTimeFormatPart {
                kind: "literal",
                value: if matches!(state.numbering_system.as_str(), "arab" | "arabext") {
                    "٫".to_owned()
                } else {
                    ".".to_owned()
                },
            });
            parts.push(DateTimeFormatPart {
                kind: "fractionalSecond",
                value: format!(
                    "{:0width$}",
                    input.nanosecond / divisor,
                    width = usize::from(digits)
                ),
            });
        }
        transliterate_parts(&mut parts, &state.numbering_system);
        return Some(parts);
    }
    None
}

fn apply_flexible_day_period(
    state: &DateTimeFormatState,
    input: &DateTimeFormatInput,
    parts: &mut [DateTimeFormatPart],
) {
    let Some(style) = state.day_period else {
        return;
    };
    if !(state.locale == "en" || state.locale.starts_with("en-")) {
        return;
    }
    let value = flexible_english_day_period(input.hour, style);
    for index in 0..parts.len() {
        if parts[index].kind == "dayPeriod" {
            value.clone_into(&mut parts[index].value);
            if index > 0 && parts[index - 1].kind == "literal" {
                " ".clone_into(&mut parts[index - 1].value);
            }
        }
    }
}

fn flexible_english_day_period(hour: u8, style: DateTimeComponentStyle) -> &'static str {
    match hour {
        6..=11 => "in the morning",
        12 if style == DateTimeComponentStyle::Narrow => "n",
        12 => "noon",
        13..=17 => "in the afternoon",
        18..=20 => "in the evening",
        _ => "at night",
    }
}

#[derive(Clone, Copy)]
struct EffectivePattern {
    field_set: CompositeFieldSet,
}

#[allow(
    clippy::too_many_lines,
    reason = "the Temporal input-kind field mask and ECMA-402 pattern selection remain visible as one closed decision table"
)]
fn effective_pattern(
    state: &DateTimeFormatState,
    kind: DateTimeFormatInputKind,
) -> Result<EffectivePattern, DateTimeFormatError> {
    let allow_year = matches!(
        kind,
        DateTimeFormatInputKind::Epoch
            | DateTimeFormatInputKind::Instant
            | DateTimeFormatInputKind::PlainDateTime
            | DateTimeFormatInputKind::PlainDate
            | DateTimeFormatInputKind::PlainYearMonth
    );
    let allow_month = !matches!(kind, DateTimeFormatInputKind::PlainTime);
    let allow_day = matches!(
        kind,
        DateTimeFormatInputKind::Epoch
            | DateTimeFormatInputKind::Instant
            | DateTimeFormatInputKind::PlainDateTime
            | DateTimeFormatInputKind::PlainDate
            | DateTimeFormatInputKind::PlainMonthDay
    );
    let allow_weekday = matches!(
        kind,
        DateTimeFormatInputKind::Epoch
            | DateTimeFormatInputKind::Instant
            | DateTimeFormatInputKind::PlainDateTime
            | DateTimeFormatInputKind::PlainDate
    );
    let allow_time = matches!(
        kind,
        DateTimeFormatInputKind::Epoch
            | DateTimeFormatInputKind::Instant
            | DateTimeFormatInputKind::PlainDateTime
            | DateTimeFormatInputKind::PlainTime
    );
    let allow_zone = matches!(
        kind,
        DateTimeFormatInputKind::Epoch | DateTimeFormatInputKind::Instant
    );

    let mut year = state.year.is_some() && allow_year;
    let mut month = state.month.is_some() && allow_month;
    let mut day = state.day.is_some() && allow_day;
    let mut weekday = state.weekday.is_some() && allow_weekday;
    let mut hour = state.hour.is_some() && allow_time;
    let mut minute = state.minute.is_some() && allow_time;
    let mut second =
        (state.second.is_some() || state.fractional_second_digits.is_some()) && allow_time;
    let mut zone = state.time_zone_name;
    let mut length = component_length(state);

    if state.default_components {
        match kind {
            DateTimeFormatInputKind::Epoch | DateTimeFormatInputKind::PlainDate => {}
            DateTimeFormatInputKind::Instant | DateTimeFormatInputKind::PlainDateTime => {
                hour = true;
                minute = true;
                second = true;
            }
            DateTimeFormatInputKind::PlainYearMonth => {
                day = false;
            }
            DateTimeFormatInputKind::PlainMonthDay => {
                year = false;
            }
            DateTimeFormatInputKind::PlainTime => {
                year = false;
                month = false;
                day = false;
                weekday = false;
                hour = true;
                minute = true;
                second = true;
            }
        }
    }

    if let Some(style) = state.date_style
        && (allow_month || allow_year || allow_day)
    {
        year = allow_year;
        month = allow_month;
        day = allow_day;
        weekday = style == DateTimeStyle::Full && allow_weekday;
        length = style_length(style);
    }
    if let Some(style) = state.time_style
        && allow_time
    {
        hour = true;
        minute = true;
        second = style != DateTimeStyle::Short;
        if allow_zone {
            zone = match style {
                DateTimeStyle::Full => Some(DateTimeTimeZoneName::Long),
                DateTimeStyle::Long => Some(DateTimeTimeZoneName::Short),
                DateTimeStyle::Medium | DateTimeStyle::Short => None,
            };
        } else {
            zone = None;
        }
        if state.date_style.is_none() {
            length = style_length(style);
        }
    }
    if !allow_zone {
        zone = None;
    }

    let date_fields = select_date_fields(year, month, day, weekday);
    let has_time = hour || minute || second || (state.day_period.is_some() && allow_time);
    if date_fields.is_none() && !has_time && zone.is_none() {
        return Err(DateTimeFormatError::NoFields);
    }

    let mut builder = FieldSetBuilder::new();
    builder.length = Some(length);
    builder.date_fields = date_fields;
    if has_time {
        builder.time_precision = Some(if let Some(digits) = state.fractional_second_digits {
            TimePrecision::Subsecond(subsecond_digits(digits))
        } else if second {
            TimePrecision::Second
        } else if minute {
            TimePrecision::Minute
        } else {
            TimePrecision::Hour
        });
    }
    builder.zone_style = zone.map(zone_style);
    if year {
        builder.year_style = Some(if state.era.is_some() {
            YearStyle::WithEra
        } else if state.year == Some(DateTimeComponentStyle::TwoDigit)
            || state.date_style == Some(DateTimeStyle::Short)
        {
            YearStyle::Auto
        } else {
            YearStyle::Full
        });
    }
    if has_two_digit_component(state) {
        builder.alignment = Some(Alignment::Column);
    }
    let field_set = builder
        .build_composite()
        .map_err(|_| DateTimeFormatError::Data)?;
    Ok(EffectivePattern { field_set })
}

#[allow(
    clippy::fn_params_excessive_bools,
    reason = "the four booleans are the closed year-month-day-weekday skeleton axes"
)]
fn select_date_fields(year: bool, month: bool, day: bool, weekday: bool) -> Option<DateFields> {
    match (year, month, day, weekday) {
        (true, _, _, true) => Some(DateFields::YMDE),
        (false, true, _, true) => Some(DateFields::MDE),
        (false, false, true, true) => Some(DateFields::DE),
        (false, false, false, true) => Some(DateFields::E),
        (true, _, true, false) => Some(DateFields::YMD),
        (false, true, true, false) => Some(DateFields::MD),
        (true, true, false, false) => Some(DateFields::YM),
        (true, false, false, false) => Some(DateFields::Y),
        (false, true, false, false) => Some(DateFields::M),
        (false, false, true, false) => Some(DateFields::D),
        (false, false, false, false) => None,
    }
}

fn style_length(style: DateTimeStyle) -> Length {
    match style {
        DateTimeStyle::Full | DateTimeStyle::Long => Length::Long,
        DateTimeStyle::Medium => Length::Medium,
        DateTimeStyle::Short => Length::Short,
    }
}

fn component_length(state: &DateTimeFormatState) -> Length {
    if [
        state.weekday,
        state.era,
        state.year,
        state.month,
        state.day_period,
    ]
    .into_iter()
    .flatten()
    .any(|value| value == DateTimeComponentStyle::Long)
    {
        Length::Long
    } else if [state.weekday, state.era, state.month, state.day_period]
        .into_iter()
        .flatten()
        .any(|value| value == DateTimeComponentStyle::Short)
    {
        Length::Medium
    } else {
        Length::Short
    }
}

fn zone_style(style: DateTimeTimeZoneName) -> ZoneStyle {
    match style {
        DateTimeTimeZoneName::Short => ZoneStyle::SpecificShort,
        DateTimeTimeZoneName::Long => ZoneStyle::SpecificLong,
        DateTimeTimeZoneName::ShortOffset => ZoneStyle::LocalizedOffsetShort,
        DateTimeTimeZoneName::LongOffset => ZoneStyle::LocalizedOffsetLong,
        DateTimeTimeZoneName::ShortGeneric => ZoneStyle::GenericShort,
        DateTimeTimeZoneName::LongGeneric => ZoneStyle::GenericLong,
    }
}

fn subsecond_digits(digits: u8) -> SubsecondDigits {
    match digits {
        1 => SubsecondDigits::S1,
        2 => SubsecondDigits::S2,
        _ => SubsecondDigits::S3,
    }
}

fn formatter_preferences(
    state: &DateTimeFormatState,
) -> Result<DateTimeFormatterPreferences, DateTimeFormatError> {
    let base = locale_components(&state.locale)?.base_name;
    let hour_cycle = match state.hour_cycle {
        DateTimeHourCycle::H11 => "h11",
        DateTimeHourCycle::H12 => "h12",
        DateTimeHourCycle::H23 | DateTimeHourCycle::H24 => "h23",
    };
    let locale = format!("{base}-u-ca-{}-hc-{hour_cycle}-nu-latn", state.calendar)
        .parse::<Locale>()
        .map_err(|_| DateTimeFormatError::InvalidLocale)?;
    Ok(locale.into())
}

fn time_zone_id(time_zone: &str) -> TimeZone {
    if parse_offset_time_zone(time_zone).is_some() {
        TimeZone::UNKNOWN
    } else {
        IanaParserExtended::new().parse(time_zone).time_zone
    }
}

fn parse_offset_time_zone(input: &str) -> Option<(i32, String)> {
    if !input.is_ascii() {
        return None;
    }
    let bytes = input.as_bytes();
    let sign = match bytes.first() {
        Some(b'+') => 1,
        Some(b'-') => -1,
        _ => return None,
    };
    let digits = match bytes {
        [_, h1, h2] => [*h1, *h2, b'0', b'0'],
        [_, h1, h2, m1, m2] | [_, h1, h2, b':', m1, m2] => [*h1, *h2, *m1, *m2],
        _ => return None,
    };
    if !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let hour = i32::from(digits[0] - b'0') * 10 + i32::from(digits[1] - b'0');
    let minute = i32::from(digits[2] - b'0') * 10 + i32::from(digits[3] - b'0');
    if hour > 23 || minute > 59 {
        return None;
    }
    let total = sign * (hour * 60 + minute) * 60;
    let normalized_sign = if total < 0 { '-' } else { '+' };
    Some((total, format!("{normalized_sign}{hour:02}:{minute:02}")))
}

fn locale_is_supported(locale: &str) -> bool {
    let Ok(components) = locale_components(locale) else {
        return false;
    };
    !matches!(components.language.as_str(), "und" | "zxx" | "tlh")
}

fn calendar_is_supported(value: &str) -> bool {
    supported_values("calendar").is_some_and(|values| values.iter().any(|item| item == value))
}

fn numbering_system_is_supported(value: &str) -> bool {
    supported_values("numberingSystem")
        .is_some_and(|values| values.iter().any(|item| item == value))
}

fn parse_hour_cycle(value: &str) -> Option<DateTimeHourCycle> {
    Some(match value {
        "h11" => DateTimeHourCycle::H11,
        "h12" => DateTimeHourCycle::H12,
        "h23" => DateTimeHourCycle::H23,
        "h24" => DateTimeHourCycle::H24,
        _ => return None,
    })
}

fn has_two_digit_component(state: &DateTimeFormatState) -> bool {
    [
        state.year,
        state.month,
        state.day,
        state.hour,
        state.minute,
        state.second,
    ]
    .into_iter()
    .flatten()
    .any(|value| value == DateTimeComponentStyle::TwoDigit)
}

#[derive(Default)]
struct PartCollector {
    text: String,
    annotations: Vec<(usize, usize, Part)>,
}

impl fmt::Write for PartCollector {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.text.push_str(value);
        Ok(())
    }
}

impl PartsWrite for PartCollector {
    type SubPartsWrite = Self;

    fn with_part(
        &mut self,
        part: Part,
        mut f: impl FnMut(&mut Self::SubPartsWrite) -> fmt::Result,
    ) -> fmt::Result {
        let start = self.text.len();
        f(self)?;
        let end = self.text.len();
        if start != end {
            self.annotations.push((start, end, part));
        }
        Ok(())
    }
}

fn capture_parts(value: &impl Writeable) -> Vec<DateTimeFormatPart> {
    let mut collector = PartCollector::default();
    if value.write_to_parts(&mut collector).is_err() {
        return Vec::new();
    }
    let mut boundaries = vec![0, collector.text.len()];
    for &(start, end, _) in &collector.annotations {
        boundaries.push(start);
        boundaries.push(end);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut result = Vec::new();
    for range in boundaries.windows(2) {
        let [start, end] = range else { continue };
        if start == end {
            continue;
        }
        let datetime = smallest_annotation(&collector.annotations, *start, *end, "datetime");
        let decimal = smallest_annotation(&collector.annotations, *start, *end, "decimal");
        let kind = match (
            datetime.map(|part| part.value),
            decimal.map(|part| part.value),
        ) {
            (Some("second"), Some("fraction")) => "fractionalSecond",
            (Some("second"), Some("decimal")) => "literal",
            (Some(value), _) => datetime_part_kind(value),
            _ => "literal",
        };
        push_part(&mut result, kind, collector.text[*start..*end].to_owned());
    }
    result
}

fn smallest_annotation(
    annotations: &[(usize, usize, Part)],
    start: usize,
    end: usize,
    category: &str,
) -> Option<Part> {
    annotations
        .iter()
        .filter(|(part_start, part_end, part)| {
            part.category == category && *part_start <= start && end <= *part_end
        })
        .min_by_key(|(part_start, part_end, _)| part_end - part_start)
        .map(|(_, _, part)| *part)
}

fn datetime_part_kind(value: &str) -> &'static str {
    match value {
        "era" => "era",
        "year" | "extendedYear" => "year",
        "relatedYear" => "relatedYear",
        "yearName" => "yearName",
        "month" => "month",
        "day" | "julianDay" => "day",
        "weekday" => "weekday",
        "dayPeriod" => "dayPeriod",
        "hour" => "hour",
        "minute" => "minute",
        "second" => "second",
        "timeZoneName" => "timeZoneName",
        _ => "literal",
    }
}

fn push_part(parts: &mut Vec<DateTimeFormatPart>, kind: &'static str, value: String) {
    if value.is_empty() {
        return;
    }
    if let Some(last) = parts.last_mut().filter(|part| part.kind == kind) {
        last.value.push_str(&value);
    } else {
        parts.push(DateTimeFormatPart { kind, value });
    }
}

fn normalize_component_widths(state: &DateTimeFormatState, parts: &mut [DateTimeFormatPart]) {
    for part in parts {
        if part.kind == "year"
            && state.date_style == Some(DateTimeStyle::Short)
            && part.value.bytes().all(|byte| byte.is_ascii_digit())
        {
            let start = part.value.len().saturating_sub(2);
            part.value = format!("{:0>2}", &part.value[start..]);
            continue;
        }
        let style = match part.kind {
            "year" => state.year,
            "month" => state.month,
            "day" => state.day,
            "hour" => state.hour,
            "minute" => state.minute,
            "second" => state.second,
            _ => None,
        };
        let Some(style) = style else { continue };
        if !part.value.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        match style {
            DateTimeComponentStyle::TwoDigit if part.kind == "year" => {
                let start = part.value.len().saturating_sub(2);
                part.value = format!("{:0>2}", &part.value[start..]);
            }
            DateTimeComponentStyle::TwoDigit => {
                part.value = format!("{:0>2}", part.value);
            }
            DateTimeComponentStyle::Numeric
            | DateTimeComponentStyle::Narrow
            | DateTimeComponentStyle::Short
            | DateTimeComponentStyle::Long => {}
        }
    }
}

fn transliterate_parts(parts: &mut [DateTimeFormatPart], numbering_system: &str) {
    let Some(digits) = numbering_system_digits(numbering_system) else {
        return;
    };
    if digits == "0123456789" {
        return;
    }
    let digits = digits.chars().collect::<Vec<_>>();
    for part in parts {
        if matches!(
            part.kind,
            "year"
                | "relatedYear"
                | "month"
                | "day"
                | "hour"
                | "minute"
                | "second"
                | "fractionalSecond"
        ) {
            part.value = part
                .value
                .chars()
                .map(|character| {
                    character
                        .to_digit(10)
                        .and_then(|digit| digits.get(digit as usize).copied())
                        .unwrap_or(character)
                })
                .collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> DateTimeFormatInput {
        DateTimeFormatInput {
            kind: DateTimeFormatInputKind::Epoch,
            year: 1886,
            month: 5,
            day: 1,
            hour: 14,
            minute: 12,
            second: 47,
            nanosecond: 0,
            offset_seconds: 0,
            epoch_seconds: -2_640_003_133,
        }
    }

    #[test]
    fn normalizes_named_and_offset_time_zones() {
        assert_eq!(canonicalize_time_zone("utc"), Ok("UTC".to_owned()));
        assert_eq!(
            canonicalize_time_zone("america/los_angeles"),
            Ok("America/Los_Angeles".to_owned())
        );
        assert_eq!(canonicalize_time_zone("-00"), Ok("+00:00".to_owned()));
        assert_eq!(canonicalize_time_zone("+2300"), Ok("+23:00".to_owned()));
        assert_eq!(
            canonicalize_time_zone("+24"),
            Err(DateTimeFormatError::InvalidTimeZone)
        );
    }

    #[test]
    fn resolves_default_date_fields_and_extension_keys() {
        let state = resolve_date_time_format(
            &["en-US-u-ca-buddhist-hc-h23-nu-arab".to_owned()],
            DateTimeFormatRequestOptions::default(),
        )
        .expect("valid state");
        assert_eq!(state.calendar, "buddhist");
        assert_eq!(state.hour_cycle, DateTimeHourCycle::H23);
        assert_eq!(state.numbering_system, "arab");
        assert_eq!(state.year, Some(DateTimeComponentStyle::Numeric));
        assert_eq!(state.month, Some(DateTimeComponentStyle::Numeric));
        assert_eq!(state.day, Some(DateTimeComponentStyle::Numeric));
    }

    #[test]
    fn formats_en_us_style_and_parts() {
        let state = resolve_date_time_format(
            &["en-US".to_owned()],
            DateTimeFormatRequestOptions {
                time_zone: Some("UTC".to_owned()),
                date_style: Some(DateTimeStyle::Full),
                ..DateTimeFormatRequestOptions::default()
            },
        )
        .expect("valid state");
        assert_eq!(
            format_datetime(&state, &input()).expect("format"),
            "Saturday, May 1, 1886"
        );
        let parts = format_datetime_to_parts(&state, &input()).expect("parts");
        assert!(parts.iter().any(|part| part.kind == "weekday"));
        assert!(parts.iter().any(|part| part.kind == "year"));
    }

    #[test]
    fn rejects_non_overlapping_temporal_fields() {
        let state = resolve_date_time_format(
            &["en-US".to_owned()],
            DateTimeFormatRequestOptions {
                year: Some(DateTimeComponentStyle::Numeric),
                ..DateTimeFormatRequestOptions::default()
            },
        )
        .expect("valid state");
        let mut plain_time = input();
        plain_time.kind = DateTimeFormatInputKind::PlainTime;
        assert_eq!(
            format_datetime(&state, &plain_time),
            Err(DateTimeFormatError::NoFields)
        );
    }
}
