//! ECMA-402 `DurationFormat` option resolution, validation, and rendering.

use fixed_decimal::{Decimal, Sign};

use crate::{
    DateTimeComponentStyle, DateTimeFormatInput, DateTimeFormatInputKind,
    DateTimeFormatRequestOptions, IntlMathematicalValue, ListFormatRequestOptions, ListFormatStyle,
    ListFormatType, NumberFormatRequestOptions, NumberFormatRoundingMode, NumberFormatSignDisplay,
    NumberFormatStyle, NumberFormatUnitDisplay, NumberFormatUseGrouping, format_datetime_to_parts,
    format_list_to_parts, format_number_to_parts, number_format_supported_locales,
    resolve_date_time_format, resolve_list_format, resolve_number_format,
};

const NANOSECONDS_PER_SECOND: i128 = 1_000_000_000;
const NANOSECONDS_PER_MILLISECOND: i128 = 1_000_000;
const NANOSECONDS_PER_MICROSECOND: i128 = 1_000;
const SECONDS_PER_MINUTE: i128 = 60;
const SECONDS_PER_HOUR: i128 = 3_600;
const SECONDS_PER_DAY: i128 = 86_400;
const TWO_TO_32: i128 = 1_i128 << 32;
const TWO_TO_53: i128 = 1_i128 << 53;

/// Duration-format construction or formatting failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurationFormatError {
    InvalidLocale,
    InvalidOption,
    InvalidDuration,
    Data,
}

/// The ten duration units in ECMA-402 table order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DurationUnit {
    Years,
    Months,
    Weeks,
    Days,
    Hours,
    Minutes,
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
}

impl DurationUnit {
    pub const ALL: [Self; 10] = [
        Self::Years,
        Self::Months,
        Self::Weeks,
        Self::Days,
        Self::Hours,
        Self::Minutes,
        Self::Seconds,
        Self::Milliseconds,
        Self::Microseconds,
        Self::Nanoseconds,
    ];

    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    #[must_use]
    pub const fn plural_name(self) -> &'static str {
        match self {
            Self::Years => "years",
            Self::Months => "months",
            Self::Weeks => "weeks",
            Self::Days => "days",
            Self::Hours => "hours",
            Self::Minutes => "minutes",
            Self::Seconds => "seconds",
            Self::Milliseconds => "milliseconds",
            Self::Microseconds => "microseconds",
            Self::Nanoseconds => "nanoseconds",
        }
    }

    #[must_use]
    pub const fn singular_name(self) -> &'static str {
        match self {
            Self::Years => "year",
            Self::Months => "month",
            Self::Weeks => "week",
            Self::Days => "day",
            Self::Hours => "hour",
            Self::Minutes => "minute",
            Self::Seconds => "second",
            Self::Milliseconds => "millisecond",
            Self::Microseconds => "microsecond",
            Self::Nanoseconds => "nanosecond",
        }
    }

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Years => "yearsDisplay",
            Self::Months => "monthsDisplay",
            Self::Weeks => "weeksDisplay",
            Self::Days => "daysDisplay",
            Self::Hours => "hoursDisplay",
            Self::Minutes => "minutesDisplay",
            Self::Seconds => "secondsDisplay",
            Self::Milliseconds => "millisecondsDisplay",
            Self::Microseconds => "microsecondsDisplay",
            Self::Nanoseconds => "nanosecondsDisplay",
        }
    }

    const fn is_subsecond(self) -> bool {
        matches!(
            self,
            Self::Milliseconds | Self::Microseconds | Self::Nanoseconds
        )
    }
}

/// The top-level duration list style.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DurationFormatStyle {
    Long,
    #[default]
    Short,
    Narrow,
    Digital,
}

impl DurationFormatStyle {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Long => "long",
            Self::Short => "short",
            Self::Narrow => "narrow",
            Self::Digital => "digital",
        }
    }

    const fn list_style(self) -> ListFormatStyle {
        match self {
            Self::Long => ListFormatStyle::Long,
            Self::Short | Self::Digital => ListFormatStyle::Short,
            Self::Narrow => ListFormatStyle::Narrow,
        }
    }

    const fn non_digital_unit_style(self) -> DurationUnitStyle {
        match self {
            Self::Long => DurationUnitStyle::Long,
            Self::Short | Self::Digital => DurationUnitStyle::Short,
            Self::Narrow => DurationUnitStyle::Narrow,
        }
    }
}

/// A public or internal duration-unit style.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurationUnitStyle {
    Long,
    Short,
    Narrow,
    Numeric,
    TwoDigit,
    /// Internal style used when a numeric value absorbs smaller units.
    Fractional,
}

impl DurationUnitStyle {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Long => "long",
            Self::Short => "short",
            Self::Narrow => "narrow",
            Self::Numeric | Self::Fractional => "numeric",
            Self::TwoDigit => "2-digit",
        }
    }

    const fn is_numeric_like(self) -> bool {
        matches!(self, Self::Numeric | Self::TwoDigit | Self::Fractional)
    }
}

/// Whether a zero-valued duration unit is displayed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DurationDisplay {
    #[default]
    Auto,
    Always,
}

impl DurationDisplay {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
        }
    }
}

/// Already-coerced JavaScript options in normative observation order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DurationFormatRequestOptions {
    pub numbering_system: Option<String>,
    pub style: Option<DurationFormatStyle>,
    pub unit_styles: [Option<DurationUnitStyle>; 10],
    pub unit_displays: [Option<DurationDisplay>; 10],
    pub fractional_digits: Option<u8>,
}

/// Resolved style and display slots for one unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurationUnitOptions {
    pub style: DurationUnitStyle,
    pub display: DurationDisplay,
}

/// Fully resolved immutable `DurationFormat` internal slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurationFormatState {
    pub locale: String,
    pub numbering_system: String,
    pub style: DurationFormatStyle,
    pub units: [DurationUnitOptions; 10],
    pub fractional_digits: Option<u8>,
    pub time_separator: String,
}

impl DurationFormatState {
    #[must_use]
    pub const fn unit(&self, unit: DurationUnit) -> DurationUnitOptions {
        self.units[unit.index()]
    }
}

/// An exact integral duration record after JavaScript coercion.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DurationRecord {
    pub values: [i128; 10],
}

impl DurationRecord {
    #[must_use]
    pub const fn value(self, unit: DurationUnit) -> i128 {
        self.values[unit.index()]
    }

    #[must_use]
    pub fn is_valid(self) -> bool {
        is_valid_duration(self)
    }

    fn is_negative(self) -> bool {
        self.values.iter().any(|value| *value < 0)
    }
}

/// One ECMA-402 `DurationFormat` part.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurationFormatPart {
    pub kind: &'static str,
    pub value: String,
    pub unit: Option<&'static str>,
}

/// Resolves requested locales and duration options.
///
/// # Errors
///
/// Returns an error for conflicting numeric/unit options or unavailable locale
/// data.
pub fn resolve_duration_format(
    requested: &[String],
    options: &DurationFormatRequestOptions,
) -> Result<DurationFormatState, DurationFormatError> {
    if options.fractional_digits.is_some_and(|digits| digits > 9) {
        return Err(DurationFormatError::InvalidOption);
    }

    let number_state = resolve_number_format(
        requested,
        NumberFormatRequestOptions {
            numbering_system: options.numbering_system.clone(),
            ..NumberFormatRequestOptions::default()
        },
    )
    .map_err(map_number_error)?;
    let style = options.style.unwrap_or_default();
    let mut units = [DurationUnitOptions {
        style: DurationUnitStyle::Short,
        display: DurationDisplay::Auto,
    }; 10];
    let mut previous_style = None;

    for unit in DurationUnit::ALL {
        let index = unit.index();
        let requested_style = options.unit_styles[index];
        let mut unit_style = requested_style.unwrap_or_else(|| {
            if style == DurationFormatStyle::Digital {
                digital_default(unit)
            } else if previous_style.is_some_and(DurationUnitStyle::is_numeric_like) {
                DurationUnitStyle::Numeric
            } else {
                style.non_digital_unit_style()
            }
        });

        if previous_style.is_some_and(DurationUnitStyle::is_numeric_like)
            && !unit_style.is_numeric_like()
        {
            return Err(DurationFormatError::InvalidOption);
        }
        if previous_style.is_some_and(|previous| {
            matches!(
                previous,
                DurationUnitStyle::Numeric | DurationUnitStyle::TwoDigit
            )
        }) && matches!(unit, DurationUnit::Minutes | DurationUnit::Seconds)
        {
            unit_style = DurationUnitStyle::TwoDigit;
        }
        if unit.is_subsecond() && unit_style == DurationUnitStyle::Numeric {
            unit_style = DurationUnitStyle::Fractional;
        }

        let default_display = if unit_style == DurationUnitStyle::Fractional {
            DurationDisplay::Auto
        } else if requested_style.is_some()
            || (style == DurationFormatStyle::Digital
                && matches!(
                    unit,
                    DurationUnit::Hours | DurationUnit::Minutes | DurationUnit::Seconds
                ))
        {
            DurationDisplay::Always
        } else {
            DurationDisplay::Auto
        };
        let display = options.unit_displays[index].unwrap_or(default_display);
        if unit_style == DurationUnitStyle::Fractional && display == DurationDisplay::Always {
            return Err(DurationFormatError::InvalidOption);
        }

        units[index] = DurationUnitOptions {
            style: unit_style,
            display,
        };
        previous_style = Some(unit_style);
    }

    let time_separator =
        resolve_time_separator(&number_state.locale, &number_state.numbering_system)?;
    Ok(DurationFormatState {
        locale: number_state.locale,
        numbering_system: number_state.numbering_system,
        style,
        units,
        fractional_digits: options.fractional_digits,
        time_separator,
    })
}

/// Returns the requested locales supported by the duration-format data profile.
#[must_use]
pub fn duration_format_supported_locales(requested: &[String]) -> Vec<String> {
    number_format_supported_locales(requested)
}

/// Formats an exact duration record.
///
/// # Errors
///
/// Returns an error when the record is invalid or locale data cannot render it.
pub fn format_duration(
    state: &DurationFormatState,
    duration: DurationRecord,
) -> Result<String, DurationFormatError> {
    Ok(format_duration_to_parts(state, duration)?
        .into_iter()
        .map(|part| part.value)
        .collect())
}

/// Formats an exact duration record into ECMA-402 parts.
///
/// # Errors
///
/// Returns an error when the record is invalid or locale data cannot render it.
#[allow(
    clippy::too_many_lines,
    reason = "the duration unit loop mirrors ECMA-402 table order and sign propagation"
)]
pub fn format_duration_to_parts(
    state: &DurationFormatState,
    duration: DurationRecord,
) -> Result<Vec<DurationFormatPart>, DurationFormatError> {
    if !duration.is_valid() {
        return Err(DurationFormatError::InvalidDuration);
    }

    let mut groups: Vec<Vec<DurationFormatPart>> = Vec::new();
    let mut numeric_group = false;
    let mut sign_displayed = false;
    let negative = duration.is_negative();

    for unit in DurationUnit::ALL {
        let unit_options = state.unit(unit);
        let value = duration.value(unit);
        let mut number = IntlMathematicalValue::Finite(Decimal::from(value));
        let mut folded_fraction = false;

        if matches!(
            unit,
            DurationUnit::Seconds | DurationUnit::Milliseconds | DurationUnit::Microseconds
        ) && next_unit_style(state, unit) == Some(DurationUnitStyle::Fractional)
        {
            number = IntlMathematicalValue::Finite(fractional_duration_value(duration, unit)?);
            folded_fraction = true;
        }

        let display_required = unit == DurationUnit::Minutes
            && numeric_group
            && (state.unit(DurationUnit::Seconds).display == DurationDisplay::Always
                || DurationUnit::ALL[DurationUnit::Seconds.index()..]
                    .iter()
                    .any(|unit| duration.value(*unit) != 0));
        let formatted_value_is_zero = if folded_fraction {
            DurationUnit::ALL[unit.index()..]
                .iter()
                .all(|unit| duration.value(*unit) == 0)
        } else {
            value == 0
        };
        if formatted_value_is_zero
            && unit_options.display == DurationDisplay::Auto
            && !display_required
        {
            if folded_fraction {
                break;
            }
            continue;
        }

        if !sign_displayed {
            sign_displayed = true;
            if value == 0 && negative {
                let IntlMathematicalValue::Finite(decimal) = &mut number else {
                    unreachable!("duration values are finite")
                };
                decimal.set_sign(Sign::Negative);
            }
        }

        let number_state = duration_number_state(
            state,
            unit,
            unit_options.style,
            sign_displayed && groups.iter().any(|group| !group.is_empty()),
            folded_fraction,
        )?;
        let number_parts = format_number_to_parts(&number_state, &number)
            .map_err(map_number_error)?
            .into_iter()
            .map(|part| DurationFormatPart {
                kind: part.kind,
                value: part.value,
                unit: Some(unit.singular_name()),
            })
            .collect::<Vec<_>>();

        if numeric_group {
            let group = groups.last_mut().ok_or(DurationFormatError::Data)?;
            group.push(DurationFormatPart {
                kind: "literal",
                value: state.time_separator.clone(),
                unit: None,
            });
            group.extend(number_parts);
        } else {
            numeric_group = matches!(
                unit_options.style,
                DurationUnitStyle::Numeric | DurationUnitStyle::TwoDigit
            );
            groups.push(number_parts);
        }

        if folded_fraction {
            break;
        }
    }

    flatten_duration_groups(state, groups)
}

fn digital_default(unit: DurationUnit) -> DurationUnitStyle {
    match unit {
        DurationUnit::Years | DurationUnit::Months | DurationUnit::Weeks | DurationUnit::Days => {
            DurationUnitStyle::Short
        }
        DurationUnit::Hours => DurationUnitStyle::Numeric,
        DurationUnit::Minutes | DurationUnit::Seconds => DurationUnitStyle::TwoDigit,
        DurationUnit::Milliseconds | DurationUnit::Microseconds | DurationUnit::Nanoseconds => {
            DurationUnitStyle::Fractional
        }
    }
}

fn next_unit_style(state: &DurationFormatState, unit: DurationUnit) -> Option<DurationUnitStyle> {
    DurationUnit::ALL
        .get(unit.index() + 1)
        .map(|next| state.unit(*next).style)
}

fn duration_number_state(
    duration_state: &DurationFormatState,
    unit: DurationUnit,
    style: DurationUnitStyle,
    suppress_sign: bool,
    folded_fraction: bool,
) -> Result<crate::NumberFormatState, DurationFormatError> {
    let mut options = NumberFormatRequestOptions {
        numbering_system: Some(duration_state.numbering_system.clone()),
        sign_display: suppress_sign.then_some(NumberFormatSignDisplay::Never),
        ..NumberFormatRequestOptions::default()
    };
    match style {
        DurationUnitStyle::Long | DurationUnitStyle::Short | DurationUnitStyle::Narrow => {
            options.style = Some(NumberFormatStyle::Unit);
            options.unit = Some(unit.singular_name().to_owned());
            options.unit_display = Some(match style {
                DurationUnitStyle::Long => NumberFormatUnitDisplay::Long,
                DurationUnitStyle::Short => NumberFormatUnitDisplay::Short,
                DurationUnitStyle::Narrow => NumberFormatUnitDisplay::Narrow,
                _ => unreachable!("matched standalone duration styles"),
            });
        }
        DurationUnitStyle::Numeric | DurationUnitStyle::Fractional => {
            options.use_grouping = Some(NumberFormatUseGrouping::Never);
        }
        DurationUnitStyle::TwoDigit => {
            options.minimum_integer_digits = Some(2);
            options.use_grouping = Some(NumberFormatUseGrouping::Never);
        }
    }
    if folded_fraction {
        options.minimum_fraction_digits = Some(duration_state.fractional_digits.unwrap_or(0));
        options.maximum_fraction_digits = Some(duration_state.fractional_digits.unwrap_or(9));
        options.rounding_mode = Some(NumberFormatRoundingMode::Trunc);
    }
    resolve_number_format(std::slice::from_ref(&duration_state.locale), options)
        .map_err(map_number_error)
}

fn fractional_duration_value(
    duration: DurationRecord,
    unit: DurationUnit,
) -> Result<Decimal, DurationFormatError> {
    let (numerator, scale) = match unit {
        DurationUnit::Seconds => (
            duration.value(DurationUnit::Seconds) * NANOSECONDS_PER_SECOND
                + duration.value(DurationUnit::Milliseconds) * NANOSECONDS_PER_MILLISECOND
                + duration.value(DurationUnit::Microseconds) * NANOSECONDS_PER_MICROSECOND
                + duration.value(DurationUnit::Nanoseconds),
            9,
        ),
        DurationUnit::Milliseconds => (
            duration.value(DurationUnit::Milliseconds) * NANOSECONDS_PER_MILLISECOND
                + duration.value(DurationUnit::Microseconds) * NANOSECONDS_PER_MICROSECOND
                + duration.value(DurationUnit::Nanoseconds),
            6,
        ),
        DurationUnit::Microseconds => (
            duration.value(DurationUnit::Microseconds) * NANOSECONDS_PER_MICROSECOND
                + duration.value(DurationUnit::Nanoseconds),
            3,
        ),
        _ => return Err(DurationFormatError::InvalidDuration),
    };
    decimal_from_scaled_integer(numerator, scale)
}

fn decimal_from_scaled_integer(
    value: i128,
    fractional_digits: usize,
) -> Result<Decimal, DurationFormatError> {
    let negative = value < 0;
    let mut digits = value.unsigned_abs().to_string();
    if digits.len() <= fractional_digits {
        digits.insert_str(0, &"0".repeat(fractional_digits + 1 - digits.len()));
    }
    let split = digits.len() - fractional_digits;
    digits.insert(split, '.');
    if negative {
        digits.insert(0, '-');
    }
    digits.parse().map_err(|_| DurationFormatError::Data)
}

fn flatten_duration_groups(
    state: &DurationFormatState,
    mut groups: Vec<Vec<DurationFormatPart>>,
) -> Result<Vec<DurationFormatPart>, DurationFormatError> {
    if groups.is_empty() {
        return Ok(Vec::new());
    }
    let strings = groups
        .iter()
        .map(|group| group.iter().map(|part| part.value.as_str()).collect())
        .collect::<Vec<String>>();
    let list_state = resolve_list_format(
        std::slice::from_ref(&state.locale),
        ListFormatRequestOptions {
            list_type: Some(ListFormatType::Unit),
            style: Some(state.style.list_style()),
        },
    )
    .map_err(|_| DurationFormatError::Data)?;
    let list_parts =
        format_list_to_parts(&list_state, &strings).map_err(|_| DurationFormatError::Data)?;
    let mut flattened = Vec::new();
    let mut groups = groups.drain(..);
    for part in list_parts {
        if part.kind == "element" {
            flattened.extend(groups.next().ok_or(DurationFormatError::Data)?);
        } else {
            flattened.push(DurationFormatPart {
                kind: part.kind,
                value: part.value,
                unit: None,
            });
        }
    }
    if groups.next().is_some() {
        return Err(DurationFormatError::Data);
    }
    Ok(flattened)
}

fn resolve_time_separator(
    locale: &str,
    numbering_system: &str,
) -> Result<String, DurationFormatError> {
    let state = resolve_date_time_format(
        &[locale.to_owned()],
        DateTimeFormatRequestOptions {
            numbering_system: Some(numbering_system.to_owned()),
            hour: Some(DateTimeComponentStyle::Numeric),
            minute: Some(DateTimeComponentStyle::Numeric),
            ..DateTimeFormatRequestOptions::default()
        },
    )
    .map_err(|_| DurationFormatError::Data)?;
    let parts = format_datetime_to_parts(
        &state,
        &DateTimeFormatInput {
            kind: DateTimeFormatInputKind::PlainTime,
            year: 1970,
            month: 1,
            day: 1,
            hour: 1,
            minute: 2,
            second: 0,
            nanosecond: 0,
            offset_seconds: 0,
            epoch_seconds: 0,
        },
    )
    .map_err(|_| DurationFormatError::Data)?;
    let hour = parts
        .iter()
        .position(|part| part.kind == "hour")
        .ok_or(DurationFormatError::Data)?;
    let minute = parts
        .iter()
        .enumerate()
        .skip(hour + 1)
        .find_map(|(index, part)| (part.kind == "minute").then_some(index))
        .ok_or(DurationFormatError::Data)?;
    let separator = parts[hour + 1..minute]
        .iter()
        .map(|part| part.value.as_str())
        .collect::<String>();
    if separator.is_empty() {
        Err(DurationFormatError::Data)
    } else {
        Ok(separator)
    }
}

fn is_valid_duration(duration: DurationRecord) -> bool {
    let mut sign = 0_i8;
    for value in duration.values {
        let value_sign = value.signum() as i8;
        if value_sign != 0 {
            if sign != 0 && sign != value_sign {
                return false;
            }
            sign = value_sign;
        }
    }
    if DurationUnit::ALL[..=DurationUnit::Weeks.index()]
        .iter()
        .any(|unit| duration.value(*unit).unsigned_abs() >= TWO_TO_32 as u128)
    {
        return false;
    }

    let seconds_numerator = duration.value(DurationUnit::Days) * SECONDS_PER_DAY
        + duration.value(DurationUnit::Hours) * SECONDS_PER_HOUR
        + duration.value(DurationUnit::Minutes) * SECONDS_PER_MINUTE
        + duration.value(DurationUnit::Seconds);
    let nanoseconds = seconds_numerator * NANOSECONDS_PER_SECOND
        + duration.value(DurationUnit::Milliseconds) * NANOSECONDS_PER_MILLISECOND
        + duration.value(DurationUnit::Microseconds) * NANOSECONDS_PER_MICROSECOND
        + duration.value(DurationUnit::Nanoseconds);
    nanoseconds.unsigned_abs() < (TWO_TO_53 * NANOSECONDS_PER_SECOND) as u128
}

fn map_number_error(error: crate::NumberFormatError) -> DurationFormatError {
    match error {
        crate::NumberFormatError::InvalidLocale => DurationFormatError::InvalidLocale,
        crate::NumberFormatError::InvalidOption
        | crate::NumberFormatError::InvalidCurrency
        | crate::NumberFormatError::InvalidUnit => DurationFormatError::InvalidOption,
        crate::NumberFormatError::InvalidNumber => DurationFormatError::InvalidDuration,
        crate::NumberFormatError::Data => DurationFormatError::Data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(values: [i128; 10]) -> DurationRecord {
        DurationRecord { values }
    }

    #[test]
    fn resolves_default_and_digital_unit_options() {
        let short =
            resolve_duration_format(&["en".to_owned()], &DurationFormatRequestOptions::default())
                .expect("short DurationFormat");
        assert_eq!(short.style, DurationFormatStyle::Short);
        assert!(short.units.iter().all(|unit| {
            unit.style == DurationUnitStyle::Short && unit.display == DurationDisplay::Auto
        }));

        let digital = resolve_duration_format(
            &["en".to_owned()],
            &DurationFormatRequestOptions {
                style: Some(DurationFormatStyle::Digital),
                ..Default::default()
            },
        )
        .expect("digital DurationFormat");
        assert_eq!(
            digital.unit(DurationUnit::Hours).style,
            DurationUnitStyle::Numeric
        );
        assert_eq!(
            digital.unit(DurationUnit::Minutes).style,
            DurationUnitStyle::TwoDigit
        );
        assert_eq!(
            digital.unit(DurationUnit::Seconds).style,
            DurationUnitStyle::TwoDigit
        );
        assert_eq!(
            digital.unit(DurationUnit::Milliseconds).style,
            DurationUnitStyle::Fractional
        );
        assert_eq!(digital.time_separator, ":");
    }

    #[test]
    fn resolves_numeric_cascade_and_rejects_conflicts() {
        let mut options = DurationFormatRequestOptions::default();
        options.unit_styles[DurationUnit::Hours.index()] = Some(DurationUnitStyle::Numeric);
        let state =
            resolve_duration_format(&["en".to_owned()], &options).expect("numeric DurationFormat");
        assert_eq!(
            state.unit(DurationUnit::Minutes).style,
            DurationUnitStyle::TwoDigit
        );
        assert_eq!(
            state.unit(DurationUnit::Seconds).style,
            DurationUnitStyle::TwoDigit
        );
        assert_eq!(
            state.unit(DurationUnit::Milliseconds).style,
            DurationUnitStyle::Fractional
        );

        let mut invalid = DurationFormatRequestOptions::default();
        invalid.unit_styles[DurationUnit::Seconds.index()] = Some(DurationUnitStyle::Numeric);
        invalid.unit_styles[DurationUnit::Milliseconds.index()] = Some(DurationUnitStyle::Long);
        assert_eq!(
            resolve_duration_format(&["en".to_owned()], &invalid),
            Err(DurationFormatError::InvalidOption)
        );
    }

    #[test]
    fn validates_sign_and_exact_bounds() {
        assert!(record([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]).is_valid());
        assert!(!record([1, -2, 0, 0, 0, 0, 0, 0, 0, 0]).is_valid());
        assert!(!record([TWO_TO_32, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_valid());
        assert!(record([0, 0, 0, 0, 0, 0, TWO_TO_53 - 1, 0, 0, 0]).is_valid());
        assert!(!record([0, 0, 0, 0, 0, 0, TWO_TO_53, 0, 0, 0]).is_valid());
    }

    #[test]
    fn formats_digital_values_and_exact_fractional_digits() {
        let state = resolve_duration_format(
            &["en".to_owned()],
            &DurationFormatRequestOptions {
                style: Some(DurationFormatStyle::Digital),
                fractional_digits: Some(9),
                ..Default::default()
            },
        )
        .expect("digital DurationFormat");
        assert_eq!(
            format_duration(&state, record([0, 0, 0, 0, 1, 22, 33, 111, 222, 333])),
            Ok("1:22:33.111222333".to_owned())
        );

        let mut seconds = DurationFormatRequestOptions::default();
        seconds.unit_styles[DurationUnit::Seconds.index()] = Some(DurationUnitStyle::Numeric);
        let state = resolve_duration_format(&["en".to_owned()], &seconds)
            .expect("numeric seconds DurationFormat");
        assert_eq!(
            format_duration(&state, record([0, 0, 0, 0, 0, 0, 10_000_000, 0, 0, 1])),
            Ok("10000000.000000001".to_owned())
        );
    }

    #[test]
    fn formats_non_numeric_units_with_list_patterns() {
        let state =
            resolve_duration_format(&["en".to_owned()], &DurationFormatRequestOptions::default())
                .expect("short DurationFormat");
        assert_eq!(
            format_duration(&state, record([1, 2, 0, 0, 0, 0, 0, 0, 0, 0])),
            Ok("1 yr, 2 mths".to_owned())
        );
    }

    #[test]
    fn derives_non_colon_time_separators_from_locale_data() {
        let state = resolve_duration_format(
            &["fi".to_owned()],
            &DurationFormatRequestOptions {
                style: Some(DurationFormatStyle::Digital),
                ..Default::default()
            },
        )
        .expect("Finnish digital DurationFormat");
        assert_eq!(state.time_separator, ".");
    }
}
