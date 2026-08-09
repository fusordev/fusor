//! ECMA-402 `RelativeTimeFormat` resolution and ICU4X-backed formatting.

use fixed_decimal::Sign;
use icu::{
    experimental::relativetime::{
        RelativeTimeFormatter, RelativeTimeFormatterOptions, RelativeTimeFormatterPreferences,
        options::Numeric as IcuNumeric,
    },
    locale::Locale,
};

use crate::{
    IntlMathematicalValue, NumberFormatRequestOptions, NumberFormatState, format_number_to_parts,
    intl_mathematical_value_from_f64, locale_components, number_format::prepare_notation,
    number_format_supported_locales, resolve_number_format,
};

/// `RelativeTimeFormat` construction or formatting failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelativeTimeFormatError {
    InvalidLocale,
    InvalidOption,
    NonFinite,
    Data,
}

/// The resolved relative-time pattern width.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RelativeTimeFormatStyle {
    #[default]
    Long,
    Short,
    Narrow,
}

impl RelativeTimeFormatStyle {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Long => "long",
            Self::Short => "short",
            Self::Narrow => "narrow",
        }
    }
}

/// Whether named relative terms such as "yesterday" may be selected.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RelativeTimeFormatNumeric {
    #[default]
    Always,
    Auto,
}

impl RelativeTimeFormatNumeric {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Auto => "auto",
        }
    }

    const fn as_icu(self) -> IcuNumeric {
        match self {
            Self::Always => IcuNumeric::Always,
            Self::Auto => IcuNumeric::Auto,
        }
    }
}

/// One singular relative-time unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelativeTimeUnit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl RelativeTimeUnit {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Second => "second",
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Quarter => "quarter",
            Self::Year => "year",
        }
    }
}

/// Already-coerced JavaScript options passed into relative-time resolution.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RelativeTimeFormatRequestOptions {
    pub numbering_system: Option<String>,
    pub style: Option<RelativeTimeFormatStyle>,
    pub numeric: Option<RelativeTimeFormatNumeric>,
}

/// Fully resolved immutable `RelativeTimeFormat` internal slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelativeTimeFormatState {
    pub locale: String,
    pub numbering_system: String,
    pub style: RelativeTimeFormatStyle,
    pub numeric: RelativeTimeFormatNumeric,
    number_format: NumberFormatState,
}

/// One ECMA-402 relative-time part.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelativeTimeFormatPart {
    pub kind: &'static str,
    pub value: String,
    pub unit: Option<&'static str>,
}

/// Resolves the locale, numbering system, style, and numeric mode.
///
/// # Errors
///
/// Returns an error for invalid locale/options or unavailable ICU data.
pub fn resolve_relative_time_format(
    requested: &[String],
    options: RelativeTimeFormatRequestOptions,
) -> Result<RelativeTimeFormatState, RelativeTimeFormatError> {
    let number_format = resolve_number_format(
        requested,
        NumberFormatRequestOptions {
            numbering_system: options.numbering_system,
            ..NumberFormatRequestOptions::default()
        },
    )
    .map_err(|_| RelativeTimeFormatError::InvalidOption)?;
    let state = RelativeTimeFormatState {
        locale: number_format.locale.clone(),
        numbering_system: number_format.numbering_system.clone(),
        style: options.style.unwrap_or_default(),
        numeric: options.numeric.unwrap_or_default(),
        number_format,
    };
    relative_time_formatter(&state, RelativeTimeUnit::Second)?;
    Ok(state)
}

/// Returns the requested locales supported by the relative-time data profile.
#[must_use]
pub fn relative_time_format_supported_locales(requested: &[String]) -> Vec<String> {
    number_format_supported_locales(requested)
}

/// Formats one finite Number into relative-time parts.
///
/// # Errors
///
/// Returns an error for non-finite input or unavailable ICU data.
pub fn format_relative_time_to_parts(
    state: &RelativeTimeFormatState,
    value: f64,
    unit: RelativeTimeUnit,
) -> Result<Vec<RelativeTimeFormatPart>, RelativeTimeFormatError> {
    let IntlMathematicalValue::Finite(value) = intl_mathematical_value_from_f64(value) else {
        return Err(RelativeTimeFormatError::NonFinite);
    };
    let (rounded, _) =
        prepare_notation(&state.number_format, value).map_err(|_| RelativeTimeFormatError::Data)?;
    let formatter = relative_time_formatter(state, unit)?;
    let absolute = rounded.clone().with_sign(Sign::None);
    let number_parts = format_number_to_parts(
        &state.number_format,
        &IntlMathematicalValue::Finite(absolute),
    )
    .map_err(|_| RelativeTimeFormatError::Data)?;
    let number = number_parts
        .iter()
        .map(|part| part.value.as_str())
        .collect::<String>();
    let rendered = formatter.format(rounded).to_string();
    let Some(number_start) = rendered.find(&number) else {
        return Ok(vec![RelativeTimeFormatPart {
            kind: "literal",
            value: rendered,
            unit: None,
        }]);
    };
    let number_end = number_start + number.len();
    let mut parts = Vec::with_capacity(number_parts.len() + 2);
    push_part(
        &mut parts,
        RelativeTimeFormatPart {
            kind: "literal",
            value: rendered[..number_start].to_owned(),
            unit: None,
        },
    );
    for part in number_parts {
        push_part(
            &mut parts,
            RelativeTimeFormatPart {
                kind: part.kind,
                value: part.value,
                unit: Some(unit.as_str()),
            },
        );
    }
    push_part(
        &mut parts,
        RelativeTimeFormatPart {
            kind: "literal",
            value: rendered[number_end..].to_owned(),
            unit: None,
        },
    );
    Ok(parts)
}

/// Formats one finite Number into a relative-time string.
///
/// # Errors
///
/// Returns an error for non-finite input or unavailable ICU data.
pub fn format_relative_time(
    state: &RelativeTimeFormatState,
    value: f64,
    unit: RelativeTimeUnit,
) -> Result<String, RelativeTimeFormatError> {
    Ok(format_relative_time_to_parts(state, value, unit)?
        .into_iter()
        .map(|part| part.value)
        .collect())
}

fn relative_time_formatter(
    state: &RelativeTimeFormatState,
    unit: RelativeTimeUnit,
) -> Result<RelativeTimeFormatter, RelativeTimeFormatError> {
    let locale = formatter_locale(state)?;
    let preferences: RelativeTimeFormatterPreferences = locale.into();
    let mut options = RelativeTimeFormatterOptions::default();
    options.numeric = state.numeric.as_icu();
    let formatter = match (state.style, unit) {
        (RelativeTimeFormatStyle::Long, RelativeTimeUnit::Second) => {
            RelativeTimeFormatter::try_new_long_second(preferences, options)
        }
        (RelativeTimeFormatStyle::Long, RelativeTimeUnit::Minute) => {
            RelativeTimeFormatter::try_new_long_minute(preferences, options)
        }
        (RelativeTimeFormatStyle::Long, RelativeTimeUnit::Hour) => {
            RelativeTimeFormatter::try_new_long_hour(preferences, options)
        }
        (RelativeTimeFormatStyle::Long, RelativeTimeUnit::Day) => {
            RelativeTimeFormatter::try_new_long_day(preferences, options)
        }
        (RelativeTimeFormatStyle::Long, RelativeTimeUnit::Week) => {
            RelativeTimeFormatter::try_new_long_week(preferences, options)
        }
        (RelativeTimeFormatStyle::Long, RelativeTimeUnit::Month) => {
            RelativeTimeFormatter::try_new_long_month(preferences, options)
        }
        (RelativeTimeFormatStyle::Long, RelativeTimeUnit::Quarter) => {
            RelativeTimeFormatter::try_new_long_quarter(preferences, options)
        }
        (RelativeTimeFormatStyle::Long, RelativeTimeUnit::Year) => {
            RelativeTimeFormatter::try_new_long_year(preferences, options)
        }
        (RelativeTimeFormatStyle::Short, RelativeTimeUnit::Second) => {
            RelativeTimeFormatter::try_new_short_second(preferences, options)
        }
        (RelativeTimeFormatStyle::Short, RelativeTimeUnit::Minute) => {
            RelativeTimeFormatter::try_new_short_minute(preferences, options)
        }
        (RelativeTimeFormatStyle::Short, RelativeTimeUnit::Hour) => {
            RelativeTimeFormatter::try_new_short_hour(preferences, options)
        }
        (RelativeTimeFormatStyle::Short, RelativeTimeUnit::Day) => {
            RelativeTimeFormatter::try_new_short_day(preferences, options)
        }
        (RelativeTimeFormatStyle::Short, RelativeTimeUnit::Week) => {
            RelativeTimeFormatter::try_new_short_week(preferences, options)
        }
        (RelativeTimeFormatStyle::Short, RelativeTimeUnit::Month) => {
            RelativeTimeFormatter::try_new_short_month(preferences, options)
        }
        (RelativeTimeFormatStyle::Short, RelativeTimeUnit::Quarter) => {
            RelativeTimeFormatter::try_new_short_quarter(preferences, options)
        }
        (RelativeTimeFormatStyle::Short, RelativeTimeUnit::Year) => {
            RelativeTimeFormatter::try_new_short_year(preferences, options)
        }
        (RelativeTimeFormatStyle::Narrow, RelativeTimeUnit::Second) => {
            RelativeTimeFormatter::try_new_narrow_second(preferences, options)
        }
        (RelativeTimeFormatStyle::Narrow, RelativeTimeUnit::Minute) => {
            RelativeTimeFormatter::try_new_narrow_minute(preferences, options)
        }
        (RelativeTimeFormatStyle::Narrow, RelativeTimeUnit::Hour) => {
            RelativeTimeFormatter::try_new_narrow_hour(preferences, options)
        }
        (RelativeTimeFormatStyle::Narrow, RelativeTimeUnit::Day) => {
            RelativeTimeFormatter::try_new_narrow_day(preferences, options)
        }
        (RelativeTimeFormatStyle::Narrow, RelativeTimeUnit::Week) => {
            RelativeTimeFormatter::try_new_narrow_week(preferences, options)
        }
        (RelativeTimeFormatStyle::Narrow, RelativeTimeUnit::Month) => {
            RelativeTimeFormatter::try_new_narrow_month(preferences, options)
        }
        (RelativeTimeFormatStyle::Narrow, RelativeTimeUnit::Quarter) => {
            RelativeTimeFormatter::try_new_narrow_quarter(preferences, options)
        }
        (RelativeTimeFormatStyle::Narrow, RelativeTimeUnit::Year) => {
            RelativeTimeFormatter::try_new_narrow_year(preferences, options)
        }
    };
    formatter.map_err(|_| RelativeTimeFormatError::Data)
}

fn formatter_locale(state: &RelativeTimeFormatState) -> Result<Locale, RelativeTimeFormatError> {
    let base = locale_components(&state.locale)
        .map_err(|_| RelativeTimeFormatError::InvalidLocale)?
        .base_name;
    format!("{base}-u-nu-{}", state.numbering_system)
        .parse()
        .map_err(|_| RelativeTimeFormatError::InvalidLocale)
}

fn push_part(parts: &mut Vec<RelativeTimeFormatPart>, part: RelativeTimeFormatPart) {
    if part.value.is_empty() {
        return;
    }
    if let Some(last) = parts
        .last_mut()
        .filter(|last| last.kind == part.kind && last.unit == part.unit)
    {
        last.value.push_str(&part.value);
    } else {
        parts.push(part);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_numbering_system_style_and_numeric_mode() {
        let state = resolve_relative_time_format(
            &["en-u-nu-arab".to_owned()],
            RelativeTimeFormatRequestOptions {
                style: Some(RelativeTimeFormatStyle::Short),
                numeric: Some(RelativeTimeFormatNumeric::Auto),
                ..RelativeTimeFormatRequestOptions::default()
            },
        )
        .unwrap();
        assert_eq!(state.locale, "en-u-nu-arab");
        assert_eq!(state.numbering_system, "arab");
        assert_eq!(state.style, RelativeTimeFormatStyle::Short);
        assert_eq!(state.numeric, RelativeTimeFormatNumeric::Auto);
    }

    #[test]
    fn formats_numeric_named_and_partitioned_relative_times() {
        let always = resolve_relative_time_format(
            &["en-US".to_owned()],
            RelativeTimeFormatRequestOptions::default(),
        )
        .unwrap();
        assert_eq!(
            format_relative_time(&always, -0.0, RelativeTimeUnit::Day),
            Ok("0 days ago".to_owned())
        );
        assert_eq!(
            format_relative_time_to_parts(&always, 123_456.78, RelativeTimeUnit::Second),
            Ok(vec![
                RelativeTimeFormatPart {
                    kind: "literal",
                    value: "in ".to_owned(),
                    unit: None,
                },
                RelativeTimeFormatPart {
                    kind: "integer",
                    value: "123".to_owned(),
                    unit: Some("second"),
                },
                RelativeTimeFormatPart {
                    kind: "group",
                    value: ",".to_owned(),
                    unit: Some("second"),
                },
                RelativeTimeFormatPart {
                    kind: "integer",
                    value: "456".to_owned(),
                    unit: Some("second"),
                },
                RelativeTimeFormatPart {
                    kind: "decimal",
                    value: ".".to_owned(),
                    unit: Some("second"),
                },
                RelativeTimeFormatPart {
                    kind: "fraction",
                    value: "78".to_owned(),
                    unit: Some("second"),
                },
                RelativeTimeFormatPart {
                    kind: "literal",
                    value: " seconds".to_owned(),
                    unit: None,
                },
            ])
        );

        let auto = resolve_relative_time_format(
            &["en-US".to_owned()],
            RelativeTimeFormatRequestOptions {
                numeric: Some(RelativeTimeFormatNumeric::Auto),
                ..RelativeTimeFormatRequestOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            format_relative_time_to_parts(&auto, -1.0, RelativeTimeUnit::Day),
            Ok(vec![RelativeTimeFormatPart {
                kind: "literal",
                value: "yesterday".to_owned(),
                unit: None,
            }])
        );
    }

    #[test]
    fn rejects_non_finite_values() {
        let state = resolve_relative_time_format(
            &["en".to_owned()],
            RelativeTimeFormatRequestOptions::default(),
        )
        .unwrap();
        assert_eq!(
            format_relative_time(&state, f64::INFINITY, RelativeTimeUnit::Year),
            Err(RelativeTimeFormatError::NonFinite)
        );
    }
}
