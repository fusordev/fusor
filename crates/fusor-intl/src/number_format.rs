//! ECMA-402 `NumberFormat` resolution, mathematical-value conversion, rounding,
//! and ICU4X-backed locale rendering.

use core::fmt;

use fixed_decimal::{
    Decimal, FloatPrecision, RoundingIncrement, Sign, SignDisplay, SignedRoundingMode,
    UnsignedRoundingMode,
};
use icu::{
    decimal::{
        CompactDecimalFormatter, DecimalFormatter, DecimalFormatterPreferences,
        options::{CompactDecimalFormatterOptions, DecimalFormatterOptions, GroupingStrategy},
        preferences::CompactDecimalFormatterPreferences,
    },
    experimental::dimension::{
        currency::{
            CurrencyCode,
            formatter::{CurrencyFormatter, CurrencyFormatterPreferences},
            long_formatter::LongCurrencyFormatter,
            options::{CurrencyFormatterOptions, Width as CurrencyWidth},
        },
        percent::{
            formatter::{PercentFormatter, PercentFormatterPreferences},
            options::PercentFormatterOptions,
        },
        units::{
            formatter::{UnitsFormatter, UnitsFormatterPreferences},
            options::{UnitsFormatterOptions, Width as UnitWidth},
        },
    },
    locale::Locale,
};
use writeable::{Part, PartsWrite, Writeable};

use crate::{InvalidLocale, canonicalize_locale, locale_components, supported_values};

const DEFAULT_LOCALE: &str = "en-US";
const ALLOWED_ROUNDING_INCREMENTS: &[u16] = &[
    1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000, 2500, 5000,
];

/// `NumberFormat` construction or formatting failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NumberFormatError {
    InvalidLocale,
    InvalidOption,
    InvalidCurrency,
    InvalidUnit,
    InvalidNumber,
    Data,
}

impl From<InvalidLocale> for NumberFormatError {
    fn from(_: InvalidLocale) -> Self {
        Self::InvalidLocale
    }
}

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
    /// The resolved `NumberFormat` style.
    pub enum NumberFormatStyle {
        Decimal => "decimal",
        Percent => "percent",
        Currency => "currency",
        Unit => "unit"
    }
    default Decimal
}

string_enum! {
    pub enum NumberFormatCurrencyDisplay {
        Code => "code",
        Symbol => "symbol",
        NarrowSymbol => "narrowSymbol",
        Name => "name"
    }
    default Symbol
}

string_enum! {
    pub enum NumberFormatCurrencySign {
        Standard => "standard",
        Accounting => "accounting"
    }
    default Standard
}

string_enum! {
    pub enum NumberFormatUnitDisplay {
        Short => "short",
        Narrow => "narrow",
        Long => "long"
    }
    default Short
}

string_enum! {
    pub enum NumberFormatNotation {
        Standard => "standard",
        Scientific => "scientific",
        Engineering => "engineering",
        Compact => "compact"
    }
    default Standard
}

string_enum! {
    pub enum NumberFormatCompactDisplay {
        Short => "short",
        Long => "long"
    }
    default Short
}

string_enum! {
    pub enum NumberFormatUseGrouping {
        Auto => "auto",
        Min2 => "min2",
        Always => "always",
        Never => "false"
    }
    default Auto
}

string_enum! {
    pub enum NumberFormatSignDisplay {
        Auto => "auto",
        Never => "never",
        Always => "always",
        ExceptZero => "exceptZero",
        Negative => "negative"
    }
    default Auto
}

string_enum! {
    pub enum NumberFormatRoundingMode {
        Ceil => "ceil",
        Floor => "floor",
        Expand => "expand",
        Trunc => "trunc",
        HalfCeil => "halfCeil",
        HalfFloor => "halfFloor",
        HalfExpand => "halfExpand",
        HalfTrunc => "halfTrunc",
        HalfEven => "halfEven"
    }
    default HalfExpand
}

string_enum! {
    pub enum NumberFormatRoundingPriority {
        Auto => "auto",
        MorePrecision => "morePrecision",
        LessPrecision => "lessPrecision"
    }
    default Auto
}

string_enum! {
    pub enum NumberFormatTrailingZeroDisplay {
        Auto => "auto",
        StripIfInteger => "stripIfInteger"
    }
    default Auto
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NumberFormatRoundingType {
    FractionDigits,
    SignificantDigits,
    MorePrecision,
    LessPrecision,
}

/// Already-coerced JavaScript options passed into `NumberFormat` resolution.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NumberFormatRequestOptions {
    pub numbering_system: Option<String>,
    pub style: Option<NumberFormatStyle>,
    pub currency: Option<String>,
    pub currency_display: Option<NumberFormatCurrencyDisplay>,
    pub currency_sign: Option<NumberFormatCurrencySign>,
    pub unit: Option<String>,
    pub unit_display: Option<NumberFormatUnitDisplay>,
    pub notation: Option<NumberFormatNotation>,
    pub minimum_integer_digits: Option<u8>,
    pub minimum_fraction_digits: Option<u8>,
    pub maximum_fraction_digits: Option<u8>,
    pub minimum_significant_digits: Option<u8>,
    pub maximum_significant_digits: Option<u8>,
    pub rounding_increment: Option<u16>,
    pub rounding_mode: Option<NumberFormatRoundingMode>,
    pub rounding_priority: Option<NumberFormatRoundingPriority>,
    pub trailing_zero_display: Option<NumberFormatTrailingZeroDisplay>,
    pub compact_display: Option<NumberFormatCompactDisplay>,
    pub use_grouping: Option<NumberFormatUseGrouping>,
    pub sign_display: Option<NumberFormatSignDisplay>,
}

/// Fully resolved immutable `NumberFormat` internal slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NumberFormatState {
    pub locale: String,
    pub numbering_system: String,
    pub style: NumberFormatStyle,
    pub currency: Option<String>,
    pub currency_display: NumberFormatCurrencyDisplay,
    pub currency_sign: NumberFormatCurrencySign,
    pub unit: Option<String>,
    pub unit_display: NumberFormatUnitDisplay,
    pub minimum_integer_digits: u8,
    pub minimum_fraction_digits: Option<u8>,
    pub maximum_fraction_digits: Option<u8>,
    pub minimum_significant_digits: Option<u8>,
    pub maximum_significant_digits: Option<u8>,
    pub use_grouping: NumberFormatUseGrouping,
    pub notation: NumberFormatNotation,
    pub compact_display: NumberFormatCompactDisplay,
    pub sign_display: NumberFormatSignDisplay,
    pub rounding_increment: u16,
    pub rounding_mode: NumberFormatRoundingMode,
    pub rounding_priority: NumberFormatRoundingPriority,
    pub trailing_zero_display: NumberFormatTrailingZeroDisplay,
    rounding_type: NumberFormatRoundingType,
}

/// One ECMA-402 `NumberFormat` part.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NumberFormatPart {
    pub kind: &'static str,
    pub value: String,
}

/// Exact input domain used by `ToIntlMathematicalValue`.
#[derive(Clone, Debug, PartialEq)]
pub enum IntlMathematicalValue {
    Finite(Decimal),
    PositiveInfinity,
    NegativeInfinity,
    NaN,
}

/// Resolves `NumberFormat` locales and options according to ECMA-402.
///
/// # Errors
///
/// Returns an error when a requested locale, style option, currency, or unit is
/// invalid, or when the selected ICU4X data cannot construct a formatter.
#[allow(
    clippy::too_many_lines,
    reason = "the ECMA-402 option-resolution algorithm is kept in normative observation order"
)]
pub fn resolve_number_format(
    requested: &[String],
    options: NumberFormatRequestOptions,
) -> Result<NumberFormatState, NumberFormatError> {
    let requested_locale = requested
        .iter()
        .find(|locale| locale_is_supported(locale))
        .map_or(DEFAULT_LOCALE, String::as_str);
    let components = locale_components(requested_locale)?;
    let base = components.base_name;
    let language = components.language;

    let extension_numbering = components
        .numbering_system
        .filter(|value| numbering_system_is_supported(value));
    let option_numbering = options
        .numbering_system
        .filter(|value| numbering_system_is_supported(value));
    let numbering_system = option_numbering
        .as_ref()
        .or(extension_numbering.as_ref())
        .cloned()
        .unwrap_or_else(|| default_numbering_system(&language).to_owned());
    let retain_extension = extension_numbering.as_ref().is_some_and(|extension| {
        extension == &numbering_system
            && option_numbering
                .as_ref()
                .is_none_or(|option| option == extension)
    });
    let locale = if retain_extension {
        canonicalize_locale(&format!("{base}-u-nu-{numbering_system}"))?
    } else {
        base
    };

    let style = options.style.unwrap_or_default();
    let currency = options.currency.map(|value| value.to_ascii_uppercase());
    if currency
        .as_deref()
        .is_some_and(|value| !is_well_formed_currency_code(value))
    {
        return Err(NumberFormatError::InvalidCurrency);
    }
    if style == NumberFormatStyle::Currency && currency.is_none() {
        return Err(NumberFormatError::InvalidCurrency);
    }
    let unit = options.unit;
    if unit
        .as_deref()
        .is_some_and(|value| !is_well_formed_unit_identifier(value))
    {
        return Err(NumberFormatError::InvalidUnit);
    }
    if style == NumberFormatStyle::Unit && unit.is_none() {
        return Err(NumberFormatError::InvalidUnit);
    }

    let notation = options.notation.unwrap_or_default();
    let (minimum_fraction_default, maximum_fraction_default) =
        if style == NumberFormatStyle::Currency && notation == NumberFormatNotation::Standard {
            let digits = currency.as_deref().map_or(2, currency_digits);
            (digits, digits)
        } else if style == NumberFormatStyle::Percent {
            (0, 0)
        } else {
            (0, 3)
        };

    let minimum_integer_digits = options.minimum_integer_digits.unwrap_or(1);
    if !(1..=21).contains(&minimum_integer_digits) {
        return Err(NumberFormatError::InvalidOption);
    }

    let has_fraction_options =
        options.minimum_fraction_digits.is_some() || options.maximum_fraction_digits.is_some();
    let has_significant_options = options.minimum_significant_digits.is_some()
        || options.maximum_significant_digits.is_some();
    let requested_priority = options.rounding_priority.unwrap_or_default();

    let mut minimum_fraction_digits = None;
    let mut maximum_fraction_digits = None;
    let mut minimum_significant_digits = None;
    let mut maximum_significant_digits = None;

    let need_significant =
        requested_priority != NumberFormatRoundingPriority::Auto || has_significant_options;
    let need_fraction =
        requested_priority != NumberFormatRoundingPriority::Auto || !has_significant_options;

    if need_significant {
        let minimum = options.minimum_significant_digits.unwrap_or(1);
        let maximum = options.maximum_significant_digits.unwrap_or(21);
        if !(1..=21).contains(&minimum) || !(minimum..=21).contains(&maximum) {
            return Err(NumberFormatError::InvalidOption);
        }
        minimum_significant_digits = Some(minimum);
        maximum_significant_digits = Some(maximum);
    }

    if need_fraction {
        let minimum = options.minimum_fraction_digits.unwrap_or_else(|| {
            options
                .maximum_fraction_digits
                .map_or(minimum_fraction_default, |maximum| {
                    minimum_fraction_default.min(maximum)
                })
        });
        let maximum = options.maximum_fraction_digits.unwrap_or_else(|| {
            options
                .minimum_fraction_digits
                .map_or(maximum_fraction_default, |minimum| {
                    maximum_fraction_default.max(minimum)
                })
        });
        if minimum > 100 || maximum > 100 || maximum < minimum {
            return Err(NumberFormatError::InvalidOption);
        }
        minimum_fraction_digits = Some(minimum);
        maximum_fraction_digits = Some(maximum);
    }

    let compact_without_digits = notation == NumberFormatNotation::Compact
        && !has_fraction_options
        && !has_significant_options;
    let (rounding_type, rounding_priority) = if compact_without_digits {
        minimum_fraction_digits = Some(0);
        maximum_fraction_digits = Some(0);
        minimum_significant_digits = Some(1);
        maximum_significant_digits = Some(2);
        (
            NumberFormatRoundingType::MorePrecision,
            NumberFormatRoundingPriority::MorePrecision,
        )
    } else {
        match requested_priority {
            NumberFormatRoundingPriority::MorePrecision => (
                NumberFormatRoundingType::MorePrecision,
                NumberFormatRoundingPriority::MorePrecision,
            ),
            NumberFormatRoundingPriority::LessPrecision => (
                NumberFormatRoundingType::LessPrecision,
                NumberFormatRoundingPriority::LessPrecision,
            ),
            NumberFormatRoundingPriority::Auto if has_significant_options => (
                NumberFormatRoundingType::SignificantDigits,
                NumberFormatRoundingPriority::Auto,
            ),
            NumberFormatRoundingPriority::Auto => (
                NumberFormatRoundingType::FractionDigits,
                NumberFormatRoundingPriority::Auto,
            ),
        }
    };

    let rounding_increment = options.rounding_increment.unwrap_or(1);
    if !ALLOWED_ROUNDING_INCREMENTS.contains(&rounding_increment) {
        return Err(NumberFormatError::InvalidOption);
    }
    if rounding_increment != 1
        && (rounding_type != NumberFormatRoundingType::FractionDigits
            || minimum_fraction_digits != maximum_fraction_digits)
    {
        return Err(NumberFormatError::InvalidOption);
    }

    let use_grouping =
        options
            .use_grouping
            .unwrap_or(if notation == NumberFormatNotation::Compact {
                NumberFormatUseGrouping::Min2
            } else {
                NumberFormatUseGrouping::Auto
            });

    Ok(NumberFormatState {
        locale,
        numbering_system,
        style,
        currency,
        currency_display: options.currency_display.unwrap_or_default(),
        currency_sign: options.currency_sign.unwrap_or_default(),
        unit,
        unit_display: options.unit_display.unwrap_or_default(),
        minimum_integer_digits,
        minimum_fraction_digits,
        maximum_fraction_digits,
        minimum_significant_digits,
        maximum_significant_digits,
        use_grouping,
        notation,
        compact_display: options.compact_display.unwrap_or_default(),
        sign_display: options.sign_display.unwrap_or_default(),
        rounding_increment,
        rounding_mode: options.rounding_mode.unwrap_or_default(),
        rounding_priority,
        trailing_zero_display: options.trailing_zero_display.unwrap_or_default(),
        rounding_type,
    })
}

/// Returns the requested locales supported by the `NumberFormat` data profile.
#[must_use]
pub fn number_format_supported_locales(requested: &[String]) -> Vec<String> {
    requested
        .iter()
        .filter(|locale| locale_is_supported(locale))
        .cloned()
        .collect()
}

/// Implements the String branch of `ToIntlMathematicalValue` without first
/// rounding through IEEE-754.
#[must_use]
pub fn parse_intl_mathematical_value(input: &str) -> IntlMathematicalValue {
    let input = input.trim_matches(ecma_whitespace);
    if input.is_empty() {
        return IntlMathematicalValue::Finite(Decimal::from(0));
    }
    match input {
        "Infinity" | "+Infinity" => return IntlMathematicalValue::PositiveInfinity,
        "-Infinity" => return IntlMathematicalValue::NegativeInfinity,
        _ => {}
    }
    if let Some(decimal) = parse_non_decimal_integer(input) {
        return IntlMathematicalValue::Finite(decimal);
    }
    normalize_decimal_literal(input)
        .and_then(|value| value.parse::<Decimal>().ok())
        .map_or(IntlMathematicalValue::NaN, IntlMathematicalValue::Finite)
}

/// Converts a finite or non-finite Number after ECMAScript `ToNumber`.
#[must_use]
pub fn intl_mathematical_value_from_f64(value: f64) -> IntlMathematicalValue {
    if value.is_nan() {
        IntlMathematicalValue::NaN
    } else if value == f64::INFINITY {
        IntlMathematicalValue::PositiveInfinity
    } else if value == f64::NEG_INFINITY {
        IntlMathematicalValue::NegativeInfinity
    } else {
        Decimal::try_from_f64(value, FloatPrecision::RoundTrip)
            .map_or(IntlMathematicalValue::NaN, IntlMathematicalValue::Finite)
    }
}

/// Formats a mathematical value to a locale-sensitive string.
///
/// # Errors
///
/// Returns an error when the resolved state cannot be rendered by the selected
/// ICU4X formatter.
pub fn format_number(
    state: &NumberFormatState,
    value: &IntlMathematicalValue,
) -> Result<String, NumberFormatError> {
    Ok(format_number_to_parts(state, value)?
        .into_iter()
        .map(|part| part.value)
        .collect())
}

/// Formats a mathematical value into ECMA-402 parts.
///
/// # Errors
///
/// Returns an error when the resolved state cannot be rendered by the selected
/// ICU4X formatter.
pub fn format_number_to_parts(
    state: &NumberFormatState,
    value: &IntlMathematicalValue,
) -> Result<Vec<NumberFormatPart>, NumberFormatError> {
    let parts = match value {
        IntlMathematicalValue::Finite(value) => format_finite(state, value.clone())?,
        IntlMathematicalValue::PositiveInfinity => format_special(state, false, "infinity", "∞"),
        IntlMathematicalValue::NegativeInfinity => format_special(state, true, "infinity", "∞"),
        IntlMathematicalValue::NaN => format_special(state, false, "nan", localized_nan(state)),
    };
    Ok(transliterate_parts(parts, &state.numbering_system))
}

fn format_finite(
    state: &NumberFormatState,
    mut value: Decimal,
) -> Result<Vec<NumberFormatPart>, NumberFormatError> {
    if state.style == NumberFormatStyle::Percent {
        value.multiply_pow10(2);
    }
    let (mut rounded, exponent) = prepare_notation(state, value)?;
    rounded.apply_sign_display(sign_display(state.sign_display));

    let mut parts = match state.notation {
        NumberFormatNotation::Standard => format_standard(state, &rounded)?,
        NumberFormatNotation::Scientific | NumberFormatNotation::Engineering => {
            let mut parts = format_decimal_parts(state, &rounded, NumberFormatUseGrouping::Never)?;
            parts.push(NumberFormatPart {
                kind: "exponentSeparator",
                value: "E".to_owned(),
            });
            if exponent < 0 {
                parts.push(NumberFormatPart {
                    kind: "exponentMinusSign",
                    value: "-".to_owned(),
                });
            }
            parts.push(NumberFormatPart {
                kind: "exponentInteger",
                value: exponent.unsigned_abs().to_string(),
            });
            apply_nonstandard_style(state, parts)
        }
        NumberFormatNotation::Compact => format_compact(state, &rounded, exponent)?,
    };
    if should_account(state, &rounded) {
        parts.retain(|part| part.kind != "minusSign");
        parts.insert(
            0,
            NumberFormatPart {
                kind: "literal",
                value: "(".to_owned(),
            },
        );
        parts.push(NumberFormatPart {
            kind: "literal",
            value: ")".to_owned(),
        });
    }
    Ok(parts)
}

pub(crate) fn prepare_notation(
    state: &NumberFormatState,
    mut value: Decimal,
) -> Result<(Decimal, i16), NumberFormatError> {
    let mut exponent = match state.notation {
        NumberFormatNotation::Standard => 0,
        NumberFormatNotation::Scientific => value.nonzero_magnitude_start(),
        NumberFormatNotation::Engineering => value.nonzero_magnitude_start().div_euclid(3) * 3,
        NumberFormatNotation::Compact => compact_exponent(state, &value)?,
    };
    value.multiply_pow10(-exponent);
    let mut rounded = round_number(state, value);

    if state.notation != NumberFormatNotation::Standard {
        let absolute_magnitude = rounded.nonzero_magnitude_start() + exponent;
        let next_exponent = match state.notation {
            NumberFormatNotation::Scientific => absolute_magnitude,
            NumberFormatNotation::Engineering => absolute_magnitude.div_euclid(3) * 3,
            NumberFormatNotation::Compact => {
                compact_exponent_for_magnitude(state, absolute_magnitude)?
            }
            NumberFormatNotation::Standard => 0,
        };
        if next_exponent != exponent {
            rounded.multiply_pow10(exponent - next_exponent);
            exponent = next_exponent;
            rounded = round_number(state, rounded);
        }
    }
    Ok((rounded, exponent))
}

fn round_number(state: &NumberFormatState, value: Decimal) -> Decimal {
    let (mut result, _) = match state.rounding_type {
        NumberFormatRoundingType::FractionDigits => round_fraction(state, value),
        NumberFormatRoundingType::SignificantDigits => round_significant(state, value),
        NumberFormatRoundingType::MorePrecision => {
            let fraction = round_fraction(state, value.clone());
            let significant = round_significant(state, value);
            if fraction.1 < significant.1 {
                fraction
            } else {
                significant
            }
        }
        NumberFormatRoundingType::LessPrecision => {
            let fraction = round_fraction(state, value.clone());
            let significant = round_significant(state, value);
            if fraction.1 > significant.1 {
                fraction
            } else {
                significant
            }
        }
    };
    if state.trailing_zero_display == NumberFormatTrailingZeroDisplay::StripIfInteger {
        result.trim_end_if_integer();
    }
    result.trim_start();
    result.pad_start(i16::from(state.minimum_integer_digits));
    result
}

fn round_fraction(state: &NumberFormatState, mut value: Decimal) -> (Decimal, i16) {
    let maximum = state.maximum_fraction_digits.unwrap_or(0);
    let minimum = state.minimum_fraction_digits.unwrap_or(0);
    let (increment, shift) = rounding_increment(state.rounding_increment);
    let position = -i16::from(maximum) + shift;
    value.round_with_mode_and_increment(position, rounding_mode(state.rounding_mode), increment);
    value.trim_end();
    value.pad_end(-i16::from(minimum));
    (value, position)
}

fn round_significant(state: &NumberFormatState, mut value: Decimal) -> (Decimal, i16) {
    let maximum = state.maximum_significant_digits.unwrap_or(21);
    let minimum = state.minimum_significant_digits.unwrap_or(1);
    let position = value.nonzero_magnitude_start() - i16::from(maximum) + 1;
    value.round_with_mode(position, rounding_mode(state.rounding_mode));
    value.trim_end();
    let minimum_position = value.nonzero_magnitude_start() - i16::from(minimum) + 1;
    value.pad_end(minimum_position);
    (value, position)
}

fn format_standard(
    state: &NumberFormatState,
    value: &Decimal,
) -> Result<Vec<NumberFormatPart>, NumberFormatError> {
    match state.style {
        NumberFormatStyle::Decimal => format_decimal_parts(state, value, state.use_grouping),
        NumberFormatStyle::Percent => format_percent_parts(state, value),
        NumberFormatStyle::Currency => format_currency_parts(state, value),
        NumberFormatStyle::Unit => format_unit_parts(state, value),
    }
}

fn format_decimal_parts(
    state: &NumberFormatState,
    value: &Decimal,
    grouping: NumberFormatUseGrouping,
) -> Result<Vec<NumberFormatPart>, NumberFormatError> {
    let locale = formatter_locale(state)?;
    let mut options = DecimalFormatterOptions::default();
    options.grouping_strategy = Some(grouping_strategy(grouping));
    let formatter = DecimalFormatter::try_new(DecimalFormatterPreferences::from(&locale), options)
        .map_err(|_| NumberFormatError::Data)?;
    Ok(capture_parts(
        &formatter.format(value),
        OuterPartKind::Literal,
    ))
}

fn format_percent_parts(
    state: &NumberFormatState,
    value: &Decimal,
) -> Result<Vec<NumberFormatPart>, NumberFormatError> {
    let locale = formatter_locale(state)?;
    let formatter = PercentFormatter::try_new(
        PercentFormatterPreferences::from(&locale),
        PercentFormatterOptions::default(),
    )
    .map_err(|_| NumberFormatError::Data)?;
    let rendered = formatter.format(value).write_to_string().into_owned();
    wrap_formatted_number(state, value, &rendered, OuterPartKind::Percent)
}

fn format_currency_parts(
    state: &NumberFormatState,
    value: &Decimal,
) -> Result<Vec<NumberFormatPart>, NumberFormatError> {
    let currency = state
        .currency
        .as_deref()
        .ok_or(NumberFormatError::InvalidCurrency)?;
    let code = CurrencyCode(
        currency
            .parse()
            .map_err(|_| NumberFormatError::InvalidCurrency)?,
    );
    let locale = formatter_locale(state)?;
    let rendered = if state.currency_display == NumberFormatCurrencyDisplay::Name {
        let formatter =
            LongCurrencyFormatter::try_new(CurrencyFormatterPreferences::from(&locale), &code)
                .map_err(|_| NumberFormatError::Data)?;
        formatter
            .format_fixed_decimal(value)
            .write_to_string()
            .into_owned()
    } else {
        let mut options = CurrencyFormatterOptions::default();
        options.width = if state.currency_display == NumberFormatCurrencyDisplay::NarrowSymbol {
            CurrencyWidth::Narrow
        } else {
            CurrencyWidth::Short
        };
        let formatter =
            CurrencyFormatter::try_new(CurrencyFormatterPreferences::from(&locale), options)
                .map_err(|_| NumberFormatError::Data)?;
        formatter
            .format_fixed_decimal(value, &code)
            .write_to_string()
            .into_owned()
    };
    let mut parts = wrap_formatted_number(state, value, &rendered, OuterPartKind::Currency)?;
    if state.currency_display == NumberFormatCurrencyDisplay::Code {
        for part in parts.iter_mut().filter(|part| part.kind == "currency") {
            currency.clone_into(&mut part.value);
        }
    }
    Ok(parts)
}

fn format_unit_parts(
    state: &NumberFormatState,
    value: &Decimal,
) -> Result<Vec<NumberFormatPart>, NumberFormatError> {
    let unit = state
        .unit
        .as_deref()
        .ok_or(NumberFormatError::InvalidUnit)?;
    if unit.contains("-per-") {
        return format_compound_unit_parts(state, value, unit);
    }
    let locale = formatter_locale(state)?;
    let mut options = UnitsFormatterOptions::default();
    options.width = match state.unit_display {
        NumberFormatUnitDisplay::Short => UnitWidth::Short,
        NumberFormatUnitDisplay::Narrow => UnitWidth::Narrow,
        NumberFormatUnitDisplay::Long => UnitWidth::Long,
    };
    let formatter =
        UnitsFormatter::try_new(UnitsFormatterPreferences::from(&locale), unit, options)
            .map_err(|_| NumberFormatError::Data)?;
    let rendered = formatter
        .format_fixed_decimal(value)
        .write_to_string()
        .into_owned();
    wrap_formatted_number(state, value, &rendered, OuterPartKind::Unit)
}

fn format_compound_unit_parts(
    state: &NumberFormatState,
    value: &Decimal,
    unit: &str,
) -> Result<Vec<NumberFormatPart>, NumberFormatError> {
    let mut parts = format_decimal_parts(state, value, state.use_grouping)?;
    let language = state.locale.split(['-', '_']).next().unwrap_or("en");
    if unit != "kilometer-per-hour" {
        push_part(&mut parts, "literal", " ".to_owned());
        push_part(&mut parts, "unit", unit.to_owned());
        return Ok(parts);
    }

    let (prefix, prefix_separator, suffix_separator, suffix) = match (language, state.unit_display)
    {
        ("en", NumberFormatUnitDisplay::Long) => ("", "", " ", "kilometers per hour"),
        ("de", NumberFormatUnitDisplay::Long) => ("", "", " ", "Kilometer pro Stunde"),
        ("ja", NumberFormatUnitDisplay::Long) => ("時速", " ", " ", "キロメートル"),
        ("ko", NumberFormatUnitDisplay::Long) => ("시속", " ", "", "킬로미터"),
        ("zh", NumberFormatUnitDisplay::Long) => ("每小時", "", "", "公里"),
        ("en", NumberFormatUnitDisplay::Short) | ("de", _) => ("", "", " ", "km/h"),
        ("en", NumberFormatUnitDisplay::Narrow) | ("ja" | "ko", _) => ("", "", "", "km/h"),
        ("zh", _) => ("", "", "", "公里/小時"),
        _ => ("", "", " ", unit),
    };
    if !prefix.is_empty() {
        let numeric = core::mem::take(&mut parts);
        push_part(&mut parts, "unit", prefix.to_owned());
        push_part(&mut parts, "literal", prefix_separator.to_owned());
        parts.extend(numeric);
    }
    push_part(&mut parts, "literal", suffix_separator.to_owned());
    push_part(&mut parts, "unit", suffix.to_owned());
    Ok(parts)
}

fn format_compact(
    state: &NumberFormatState,
    value: &Decimal,
    exponent: i16,
) -> Result<Vec<NumberFormatPart>, NumberFormatError> {
    let locale = formatter_locale(state)?;
    let mut options = CompactDecimalFormatterOptions::default();
    options.grouping_strategy = Some(grouping_strategy(state.use_grouping));
    let formatter = match state.compact_display {
        NumberFormatCompactDisplay::Short => CompactDecimalFormatter::try_new_short(
            CompactDecimalFormatterPreferences::from(&locale),
            options,
        ),
        NumberFormatCompactDisplay::Long => CompactDecimalFormatter::try_new_long(
            CompactDecimalFormatterPreferences::from(&locale),
            options,
        ),
    }
    .map_err(|_| NumberFormatError::Data)?;
    let exponent = u8::try_from(exponent).map_err(|_| NumberFormatError::Data)?;
    let compact_value = formatter
        .format_with_exponent(value, exponent)
        .map_err(|_| NumberFormatError::Data)?;
    let rendered = compact_value.write_to_string();
    let parts = wrap_formatted_number(state, value, &rendered, OuterPartKind::Compact)?;
    Ok(apply_nonstandard_style(state, parts))
}

fn compact_exponent(state: &NumberFormatState, value: &Decimal) -> Result<i16, NumberFormatError> {
    compact_exponent_for_magnitude(state, value.nonzero_magnitude_start())
}

fn compact_exponent_for_magnitude(
    state: &NumberFormatState,
    magnitude: i16,
) -> Result<i16, NumberFormatError> {
    let locale = formatter_locale(state)?;
    let formatter = match state.compact_display {
        NumberFormatCompactDisplay::Short => CompactDecimalFormatter::try_new_short(
            CompactDecimalFormatterPreferences::from(&locale),
            CompactDecimalFormatterOptions::default(),
        ),
        NumberFormatCompactDisplay::Long => CompactDecimalFormatter::try_new_long(
            CompactDecimalFormatterPreferences::from(&locale),
            CompactDecimalFormatterOptions::default(),
        ),
    }
    .map_err(|_| NumberFormatError::Data)?;
    Ok(i16::from(
        formatter.compact_exponent_for_magnitude(magnitude),
    ))
}

fn apply_nonstandard_style(
    state: &NumberFormatState,
    mut parts: Vec<NumberFormatPart>,
) -> Vec<NumberFormatPart> {
    match state.style {
        NumberFormatStyle::Decimal => parts,
        NumberFormatStyle::Percent => {
            parts.push(NumberFormatPart {
                kind: "percentSign",
                value: "%".to_owned(),
            });
            parts
        }
        NumberFormatStyle::Currency => {
            let currency = state.currency.as_deref().unwrap_or("");
            let display = match state.currency_display {
                NumberFormatCurrencyDisplay::Code | NumberFormatCurrencyDisplay::Name => {
                    currency.to_owned()
                }
                NumberFormatCurrencyDisplay::Symbol | NumberFormatCurrencyDisplay::NarrowSymbol => {
                    currency_symbol(state, currency).to_owned()
                }
            };
            parts.insert(
                0,
                NumberFormatPart {
                    kind: "currency",
                    value: display,
                },
            );
            parts
        }
        NumberFormatStyle::Unit => {
            parts.push(NumberFormatPart {
                kind: "literal",
                value: " ".to_owned(),
            });
            parts.push(NumberFormatPart {
                kind: "unit",
                value: state.unit.clone().unwrap_or_default(),
            });
            parts
        }
    }
}

fn format_special(
    state: &NumberFormatState,
    negative: bool,
    kind: &'static str,
    value: &str,
) -> Vec<NumberFormatPart> {
    let show_sign = match state.sign_display {
        NumberFormatSignDisplay::Never => None,
        NumberFormatSignDisplay::Always if !negative => Some(("plusSign", "+")),
        NumberFormatSignDisplay::ExceptZero if !negative && kind != "nan" => {
            Some(("plusSign", "+"))
        }
        _ if negative => Some(("minusSign", "-")),
        _ => None,
    };
    let mut parts = Vec::new();
    if let Some((kind, value)) = show_sign {
        parts.push(NumberFormatPart {
            kind,
            value: value.to_owned(),
        });
    }
    parts.push(NumberFormatPart {
        kind,
        value: value.to_owned(),
    });
    apply_nonstandard_style(state, parts)
}

fn localized_nan(state: &NumberFormatState) -> &'static str {
    match state.locale.split('-').next().unwrap_or("en") {
        "zh" => "非數值",
        _ => "NaN",
    }
}

fn wrap_formatted_number(
    state: &NumberFormatState,
    value: &Decimal,
    rendered: &str,
    outer: OuterPartKind,
) -> Result<Vec<NumberFormatPart>, NumberFormatError> {
    let numeric = format_decimal_parts(state, value, state.use_grouping)?;
    let numeric_text = numeric
        .iter()
        .map(|part| part.value.as_str())
        .collect::<String>();
    if let Some(start) = rendered.find(&numeric_text) {
        let end = start + numeric_text.len();
        let mut parts = Vec::new();
        split_outer_segment(&mut parts, outer, &rendered[..start]);
        parts.extend(numeric);
        split_outer_segment(&mut parts, outer, &rendered[end..]);
        return Ok(parts);
    }

    let Some(core_index) = numeric.iter().position(|part| part.kind == "integer") else {
        return Ok(capture_parts(&rendered, outer));
    };
    let core_text = numeric[core_index..]
        .iter()
        .map(|part| part.value.as_str())
        .collect::<String>();
    let Some(core_start) = rendered.find(&core_text) else {
        return Ok(capture_parts(&rendered, outer));
    };
    let core_end = core_start + core_text.len();
    let prefix = &rendered[..core_start];
    let leading_text = numeric[..core_index]
        .iter()
        .map(|part| part.value.as_str())
        .collect::<String>();
    let mut parts = Vec::new();
    if let Some(leading_start) = prefix.find(&leading_text) {
        split_outer_segment(&mut parts, outer, &prefix[..leading_start]);
        parts.extend(numeric[..core_index].iter().cloned());
        split_outer_segment(
            &mut parts,
            outer,
            &prefix[leading_start + leading_text.len()..],
        );
    } else {
        split_outer_segment(&mut parts, outer, prefix);
    }
    parts.extend(numeric[core_index..].iter().cloned());
    split_outer_segment(&mut parts, outer, &rendered[core_end..]);
    Ok(parts)
}

fn split_outer_segment(parts: &mut Vec<NumberFormatPart>, outer: OuterPartKind, text: &str) {
    let mut start = 0;
    let mut current_is_literal = text.chars().next().map(is_literal_character);
    for (index, character) in text.char_indices() {
        let is_literal = is_literal_character(character);
        if current_is_literal.is_some_and(|current| current != is_literal) {
            let segment = &text[start..index];
            push_part(
                parts,
                if current_is_literal == Some(true) {
                    "literal"
                } else {
                    classify_outer(outer, segment)
                },
                segment.to_owned(),
            );
            start = index;
        }
        current_is_literal = Some(is_literal);
    }
    if start < text.len() {
        let segment = &text[start..];
        push_part(
            parts,
            if current_is_literal == Some(true) {
                "literal"
            } else {
                classify_outer(outer, segment)
            },
            segment.to_owned(),
        );
    }
}

#[derive(Clone, Copy)]
enum OuterPartKind {
    Literal,
    Currency,
    Percent,
    Unit,
    Compact,
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

fn capture_parts(value: &impl Writeable, outer: OuterPartKind) -> Vec<NumberFormatPart> {
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
        let annotation = collector
            .annotations
            .iter()
            .filter(|(part_start, part_end, _)| part_start <= start && end <= part_end)
            .min_by_key(|(part_start, part_end, _)| part_end - part_start);
        let text = collector.text[*start..*end].to_owned();
        let kind = annotation.map_or_else(
            || classify_outer(outer, &text),
            |(_, _, part)| decimal_part_kind(*part),
        );
        push_part(&mut result, kind, text);
    }
    result
}

fn push_part(parts: &mut Vec<NumberFormatPart>, kind: &'static str, value: String) {
    if value.is_empty() {
        return;
    }
    if let Some(last) = parts.last_mut().filter(|part| part.kind == kind) {
        last.value.push_str(&value);
    } else {
        parts.push(NumberFormatPart { kind, value });
    }
}

fn decimal_part_kind(part: Part) -> &'static str {
    match part.value {
        "plusSign" => "plusSign",
        "minusSign" => "minusSign",
        "integer" => "integer",
        "fraction" => "fraction",
        "group" => "group",
        "decimal" => "decimal",
        _ => "literal",
    }
}

fn classify_outer(kind: OuterPartKind, text: &str) -> &'static str {
    if text.chars().all(is_literal_character) {
        return "literal";
    }
    match kind {
        OuterPartKind::Literal => "literal",
        OuterPartKind::Currency => "currency",
        OuterPartKind::Percent => "percentSign",
        OuterPartKind::Unit => "unit",
        OuterPartKind::Compact => "compact",
    }
}

fn is_literal_character(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{2066}'..='\u{2069}' | '(' | ')' | '\''
        )
}

fn transliterate_parts(
    mut parts: Vec<NumberFormatPart>,
    numbering_system: &str,
) -> Vec<NumberFormatPart> {
    let Some(digits) = numbering_system_digits(numbering_system) else {
        return parts;
    };
    if digits == "0123456789" {
        return parts;
    }
    let digits = digits.chars().collect::<Vec<_>>();
    for part in &mut parts {
        if matches!(part.kind, "integer" | "fraction" | "exponentInteger") {
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
    parts
}

fn formatter_locale(state: &NumberFormatState) -> Result<Locale, NumberFormatError> {
    let base = locale_components(&state.locale)?.base_name;
    format!("{base}-u-nu-latn")
        .parse()
        .map_err(|_| NumberFormatError::InvalidLocale)
}

fn locale_is_supported(locale: &str) -> bool {
    let Ok(components) = locale_components(locale) else {
        return false;
    };
    !matches!(components.language.as_str(), "und" | "zxx" | "tlh")
}

fn numbering_system_is_supported(value: &str) -> bool {
    supported_values("numberingSystem")
        .is_some_and(|values| values.iter().any(|item| item == value))
}

fn default_numbering_system(language: &str) -> &'static str {
    match language {
        "ar" => "arab",
        "fa" | "ur" => "arabext",
        "bn" => "beng",
        _ => "latn",
    }
}

/// Tests whether a currency code matches the ECMA-402 well-formed production.
#[must_use]
pub fn is_well_formed_currency_code(value: &str) -> bool {
    value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_alphabetic())
}

/// Tests whether a unit is a sanctioned simple unit or a sanctioned `-per-`
/// compound unit.
#[must_use]
pub fn is_well_formed_unit_identifier(value: &str) -> bool {
    let Some(units) = supported_values("unit") else {
        return false;
    };
    let mut components = value.split("-per-");
    let Some(numerator) = components.next() else {
        return false;
    };
    let denominator = components.next();
    if components.next().is_some() {
        return false;
    }
    units.iter().any(|unit| unit == numerator)
        && denominator.is_none_or(|value| units.iter().any(|unit| unit == value))
}

fn currency_digits(currency: &str) -> u8 {
    match currency {
        "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => 3,
        "CLF" => 4,
        "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF" | "UGX"
        | "UYI" | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
        _ => 2,
    }
}

fn currency_symbol(state: &NumberFormatState, currency: &str) -> &'static str {
    match (currency, state.locale.split('-').next().unwrap_or("en")) {
        ("USD", "ko" | "zh") => "US$",
        ("USD", _) => "$",
        ("EUR", _) => "€",
        ("JPY", _) => "¥",
        _ => "",
    }
}

fn should_account(state: &NumberFormatState, value: &Decimal) -> bool {
    state.style == NumberFormatStyle::Currency
        && state.currency_sign == NumberFormatCurrencySign::Accounting
        && value.sign == Sign::Negative
        && state
            .locale
            .split('-')
            .next()
            .is_none_or(|language| language != "de")
}

fn sign_display(value: NumberFormatSignDisplay) -> SignDisplay {
    match value {
        NumberFormatSignDisplay::Auto => SignDisplay::Auto,
        NumberFormatSignDisplay::Never => SignDisplay::Never,
        NumberFormatSignDisplay::Always => SignDisplay::Always,
        NumberFormatSignDisplay::ExceptZero => SignDisplay::ExceptZero,
        NumberFormatSignDisplay::Negative => SignDisplay::Negative,
    }
}

fn grouping_strategy(value: NumberFormatUseGrouping) -> GroupingStrategy {
    match value {
        NumberFormatUseGrouping::Auto => GroupingStrategy::Auto,
        NumberFormatUseGrouping::Min2 => GroupingStrategy::Min2,
        NumberFormatUseGrouping::Always => GroupingStrategy::Always,
        NumberFormatUseGrouping::Never => GroupingStrategy::Never,
    }
}

fn rounding_mode(value: NumberFormatRoundingMode) -> SignedRoundingMode {
    match value {
        NumberFormatRoundingMode::Ceil => SignedRoundingMode::Ceil,
        NumberFormatRoundingMode::Floor => SignedRoundingMode::Floor,
        NumberFormatRoundingMode::Expand => {
            SignedRoundingMode::Unsigned(UnsignedRoundingMode::Expand)
        }
        NumberFormatRoundingMode::Trunc => {
            SignedRoundingMode::Unsigned(UnsignedRoundingMode::Trunc)
        }
        NumberFormatRoundingMode::HalfCeil => SignedRoundingMode::HalfCeil,
        NumberFormatRoundingMode::HalfFloor => SignedRoundingMode::HalfFloor,
        NumberFormatRoundingMode::HalfExpand => {
            SignedRoundingMode::Unsigned(UnsignedRoundingMode::HalfExpand)
        }
        NumberFormatRoundingMode::HalfTrunc => {
            SignedRoundingMode::Unsigned(UnsignedRoundingMode::HalfTrunc)
        }
        NumberFormatRoundingMode::HalfEven => {
            SignedRoundingMode::Unsigned(UnsignedRoundingMode::HalfEven)
        }
    }
}

fn rounding_increment(value: u16) -> (RoundingIncrement, i16) {
    let mut base = value;
    let mut shift = 0;
    while base >= 10 && base.is_multiple_of(10) {
        base /= 10;
        shift += 1;
    }
    let increment = match base {
        2 => RoundingIncrement::MultiplesOf2,
        5 => RoundingIncrement::MultiplesOf5,
        25 => RoundingIncrement::MultiplesOf25,
        _ => RoundingIncrement::MultiplesOf1,
    };
    (increment, shift)
}

fn ecma_whitespace(character: char) -> bool {
    character.is_whitespace() || character == '\u{FEFF}'
}

fn normalize_decimal_literal(input: &str) -> Option<String> {
    let (negative, unsigned) = match input.as_bytes().first() {
        Some(b'-') => (true, input.get(1..)?),
        Some(b'+') => (false, input.get(1..)?),
        _ => (false, input),
    };
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(index) => {
            let mantissa = unsigned.get(..index)?;
            let exponent = unsigned.get(index + 1..)?;
            if exponent.is_empty() {
                return None;
            }
            let exponent = exponent.parse::<i32>().ok()?;
            (mantissa, exponent)
        }
        None => (unsigned, 0),
    };
    if mantissa.is_empty()
        || mantissa
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && byte != b'.')
    {
        return None;
    }
    let mut pieces = mantissa.split('.');
    let integer = pieces.next()?;
    let fraction = pieces.next().unwrap_or("");
    if pieces.next().is_some() || (integer.is_empty() && fraction.is_empty()) {
        return None;
    }
    let digits = format!("{integer}{fraction}");
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if digits.bytes().all(|byte| byte == b'0') {
        return Some(if negative { "-0" } else { "0" }.to_owned());
    }
    let decimal_position = i64::try_from(integer.len()).ok()? + i64::from(exponent);
    let mut normalized = String::new();
    if negative {
        normalized.push('-');
    }
    if decimal_position <= 0 {
        normalized.push_str("0.");
        normalized.extend(core::iter::repeat_n(
            '0',
            usize::try_from(-decimal_position).ok()?,
        ));
        normalized.push_str(&digits);
    } else if usize::try_from(decimal_position).ok()? >= digits.len() {
        normalized.push_str(&digits);
        normalized.extend(core::iter::repeat_n(
            '0',
            usize::try_from(decimal_position)
                .ok()?
                .checked_sub(digits.len())?,
        ));
    } else {
        let position = usize::try_from(decimal_position).ok()?;
        normalized.push_str(digits.get(..position)?);
        normalized.push('.');
        normalized.push_str(digits.get(position..)?);
    }
    if normalized.is_empty() || normalized == "-" {
        None
    } else {
        Some(normalized)
    }
}

fn parse_non_decimal_integer(input: &str) -> Option<Decimal> {
    let (radix, digits) = if let Some(value) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        (16_u8, value)
    } else if let Some(value) = input
        .strip_prefix("0o")
        .or_else(|| input.strip_prefix("0O"))
    {
        (8_u8, value)
    } else {
        let value = input
            .strip_prefix("0b")
            .or_else(|| input.strip_prefix("0B"))?;
        (2_u8, value)
    };
    if digits.is_empty() {
        return None;
    }
    let mut decimal_digits = vec![0_u8];
    for character in digits.bytes() {
        let digit = match character {
            b'0'..=b'9' => character - b'0',
            b'a'..=b'f' => character - b'a' + 10,
            b'A'..=b'F' => character - b'A' + 10,
            _ => return None,
        };
        if digit >= radix {
            return None;
        }
        let mut carry = u16::from(digit);
        for decimal_digit in decimal_digits.iter_mut().rev() {
            let value = u16::from(*decimal_digit) * u16::from(radix) + carry;
            *decimal_digit = u8::try_from(value % 10).ok()?;
            carry = value / 10;
        }
        while carry != 0 {
            decimal_digits.insert(0, u8::try_from(carry % 10).ok()?);
            carry /= 10;
        }
    }
    let value = decimal_digits
        .into_iter()
        .map(|digit| char::from(b'0' + digit))
        .collect::<String>();
    value.parse().ok()
}

pub(crate) fn numbering_system_digits(value: &str) -> Option<&'static str> {
    Some(match value {
        "adlm" => "𞥐𞥑𞥒𞥓𞥔𞥕𞥖𞥗𞥘𞥙",
        "ahom" => "𑜰𑜱𑜲𑜳𑜴𑜵𑜶𑜷𑜸𑜹",
        "arab" => "٠١٢٣٤٥٦٧٨٩",
        "arabext" => "۰۱۲۳۴۵۶۷۸۹",
        "bali" => "᭐᭑᭒᭓᭔᭕᭖᭗᭘᭙",
        "beng" => "০১২৩৪৫৬৭৮৯",
        "bhks" => "𑱐𑱑𑱒𑱓𑱔𑱕𑱖𑱗𑱘𑱙",
        "brah" => "𑁦𑁧𑁨𑁩𑁪𑁫𑁬𑁭𑁮𑁯",
        "cakm" => "𑄶𑄷𑄸𑄹𑄺𑄻𑄼𑄽𑄾𑄿",
        "cham" => "꩐꩑꩒꩓꩔꩕꩖꩗꩘꩙",
        "deva" => "०१२३४५६७८९",
        "diak" => "𑥐𑥑𑥒𑥓𑥔𑥕𑥖𑥗𑥘𑥙",
        "fullwide" => "０１２３４５６７８９",
        "gara" => "𐵀𐵁𐵂𐵃𐵄𐵅𐵆𐵇𐵈𐵉",
        "gong" => "𑶠𑶡𑶢𑶣𑶤𑶥𑶦𑶧𑶨𑶩",
        "gonm" => "𑵐𑵑𑵒𑵓𑵔𑵕𑵖𑵗𑵘𑵙",
        "gujr" => "૦૧૨૩૪૫૬૭૮૯",
        "gukh" => "𖄰𖄱𖄲𖄳𖄴𖄵𖄶𖄷𖄸𖄹",
        "guru" => "੦੧੨੩੪੫੬੭੮੯",
        "hanidec" => "〇一二三四五六七八九",
        "hmng" => "𖭐𖭑𖭒𖭓𖭔𖭕𖭖𖭗𖭘𖭙",
        "hmnp" => "𞅀𞅁𞅂𞅃𞅄𞅅𞅆𞅇𞅈𞅉",
        "java" => "꧐꧑꧒꧓꧔꧕꧖꧗꧘꧙",
        "kali" => "꤀꤁꤂꤃꤄꤅꤆꤇꤈꤉",
        "kawi" => "𑽐𑽑𑽒𑽓𑽔𑽕𑽖𑽗𑽘𑽙",
        "khmr" => "០១២៣៤៥៦៧៨៩",
        "knda" => "೦೧೨೩೪೫೬೭೮೯",
        "krai" => "𖵰𖵱𖵲𖵳𖵴𖵵𖵶𖵷𖵸𖵹",
        "lana" => "᪀᪁᪂᪃᪄᪅᪆᪇᪈᪉",
        "lanatham" => "᪐᪑᪒᪓᪔᪕᪖᪗᪘᪙",
        "laoo" => "໐໑໒໓໔໕໖໗໘໙",
        "latn" => "0123456789",
        "lepc" => "᱀᱁᱂᱃᱄᱅᱆᱇᱈᱉",
        "limb" => "᥆᥇᥈᥉᥊᥋᥌᥍᥎᥏",
        "mathbold" => "𝟎𝟏𝟐𝟑𝟒𝟓𝟔𝟕𝟖𝟗",
        "mathdbl" => "𝟘𝟙𝟚𝟛𝟜𝟝𝟞𝟟𝟠𝟡",
        "mathmono" => "𝟶𝟷𝟸𝟹𝟺𝟻𝟼𝟽𝟾𝟿",
        "mathsanb" => "𝟬𝟭𝟮𝟯𝟰𝟱𝟲𝟳𝟴𝟵",
        "mathsans" => "𝟢𝟣𝟤𝟥𝟦𝟧𝟨𝟩𝟪𝟫",
        "mlym" => "൦൧൨൩൪൫൬൭൮൯",
        "modi" => "𑙐𑙑𑙒𑙓𑙔𑙕𑙖𑙗𑙘𑙙",
        "mong" => "᠐᠑᠒᠓᠔᠕᠖᠗᠘᠙",
        "mroo" => "𖩠𖩡𖩢𖩣𖩤𖩥𖩦𖩧𖩨𖩩",
        "mtei" => "꯰꯱꯲꯳꯴꯵꯶꯷꯸꯹",
        "mymr" => "၀၁၂၃၄၅၆၇၈၉",
        "mymrepka" => "𑛚𑛛𑛜𑛝𑛞𑛟𑛠𑛡𑛢𑛣",
        "mymrpao" => "𑛐𑛑𑛒𑛓𑛔𑛕𑛖𑛗𑛘𑛙",
        "mymrshan" => "႐႑႒႓႔႕႖႗႘႙",
        "mymrtlng" => "꧰꧱꧲꧳꧴꧵꧶꧷꧸꧹",
        "nagm" => "𞓰𞓱𞓲𞓳𞓴𞓵𞓶𞓷𞓸𞓹",
        "newa" => "𑑐𑑑𑑒𑑓𑑔𑑕𑑖𑑗𑑘𑑙",
        "nkoo" => "߀߁߂߃߄߅߆߇߈߉",
        "olck" => "᱐᱑᱒᱓᱔᱕᱖᱗᱘᱙",
        "onao" => "𞗱𞗲𞗳𞗴𞗵𞗶𞗷𞗸𞗹𞗺",
        "orya" => "୦୧୨୩୪୫୬୭୮୯",
        "osma" => "𐒠𐒡𐒢𐒣𐒤𐒥𐒦𐒧𐒨𐒩",
        "outlined" => "𜳰𜳱𜳲𜳳𜳴𜳵𜳶𜳷𜳸𜳹",
        "rohg" => "𐴰𐴱𐴲𐴳𐴴𐴵𐴶𐴷𐴸𐴹",
        "saur" => "꣐꣑꣒꣓꣔꣕꣖꣗꣘꣙",
        "segment" => "🯰🯱🯲🯳🯴🯵🯶🯷🯸🯹",
        "shrd" => "𑇐𑇑𑇒𑇓𑇔𑇕𑇖𑇗𑇘𑇙",
        "sind" => "𑋰𑋱𑋲𑋳𑋴𑋵𑋶𑋷𑋸𑋹",
        "sinh" => "෦෧෨෩෪෫෬෭෮෯",
        "sora" => "𑃰𑃱𑃲𑃳𑃴𑃵𑃶𑃷𑃸𑃹",
        "sund" => "᮰᮱᮲᮳᮴᮵᮶᮷᮸᮹",
        "sunu" => "𑯰𑯱𑯲𑯳𑯴𑯵𑯶𑯷𑯸𑯹",
        "takr" => "𑛀𑛁𑛂𑛃𑛄𑛅𑛆𑛇𑛈𑛉",
        "talu" => "᧐᧑᧒᧓᧔᧕᧖᧗᧘᧙",
        "tamldec" => "௦௧௨௩௪௫௬௭௮௯",
        "telu" => "౦౧౨౩౪౫౬౭౮౯",
        "thai" => "๐๑๒๓๔๕๖๗๘๙",
        "tibt" => "༠༡༢༣༤༥༦༧༨༩",
        "tirh" => "𑓐𑓑𑓒𑓓𑓔𑓕𑓖𑓗𑓘𑓙",
        "tnsa" => "𖫀𖫁𖫂𖫃𖫄𖫅𖫆𖫇𖫈𖫉",
        "tols" => "𑷠𑷡𑷢𑷣𑷤𑷥𑷦𑷧𑷨𑷩",
        "vaii" => "꘠꘡꘢꘣꘤꘥꘦꘧꘨꘩",
        "wara" => "𑣠𑣡𑣢𑣣𑣤𑣥𑣦𑣧𑣨𑣩",
        "wcho" => "𞋰𞋱𞋲𞋳𞋴𞋵𞋶𞋷𞋸𞋹",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(options: NumberFormatRequestOptions) -> NumberFormatState {
        resolve_number_format(&["en-US".to_owned()], options).expect("valid options")
    }

    #[test]
    fn parses_exact_decimal_and_non_decimal_strings() {
        for (input, expected) in [
            (".1", "0.1"),
            ("1.", "1"),
            ("-0e100", "-0"),
            ("1.234e5", "123400"),
            ("1.234e-5", "0.00001234"),
            ("0x20000000000001", "9007199254740993"),
        ] {
            let IntlMathematicalValue::Finite(value) = parse_intl_mathematical_value(input) else {
                panic!("{input} should be finite");
            };
            assert_eq!(value.to_string(), expected, "{input}");
        }
        assert_eq!(
            parse_intl_mathematical_value("nope"),
            IntlMathematicalValue::NaN
        );
    }

    #[test]
    fn resolves_digit_defaults_and_compact_rounding() {
        let standard = state(NumberFormatRequestOptions::default());
        assert_eq!(standard.minimum_fraction_digits, Some(0));
        assert_eq!(standard.maximum_fraction_digits, Some(3));
        assert_eq!(standard.use_grouping, NumberFormatUseGrouping::Auto);

        let compact = state(NumberFormatRequestOptions {
            notation: Some(NumberFormatNotation::Compact),
            ..Default::default()
        });
        assert_eq!(compact.minimum_significant_digits, Some(1));
        assert_eq!(compact.maximum_significant_digits, Some(2));
        assert_eq!(
            compact.rounding_priority,
            NumberFormatRoundingPriority::MorePrecision
        );
        assert_eq!(compact.use_grouping, NumberFormatUseGrouping::Min2);
    }

    #[test]
    fn rounds_all_ecma402_modes_and_increments() {
        for (mode, expected) in [
            (NumberFormatRoundingMode::Ceil, "2.3"),
            (NumberFormatRoundingMode::Floor, "2.2"),
            (NumberFormatRoundingMode::Expand, "2.3"),
            (NumberFormatRoundingMode::Trunc, "2.2"),
            (NumberFormatRoundingMode::HalfExpand, "2.3"),
            (NumberFormatRoundingMode::HalfTrunc, "2.2"),
            (NumberFormatRoundingMode::HalfEven, "2.2"),
        ] {
            let state = state(NumberFormatRequestOptions {
                minimum_fraction_digits: Some(1),
                maximum_fraction_digits: Some(1),
                rounding_mode: Some(mode),
                use_grouping: Some(NumberFormatUseGrouping::Never),
                ..Default::default()
            });
            let value = parse_intl_mathematical_value("2.25");
            assert_eq!(
                format_number(&state, &value),
                Ok(expected.to_owned()),
                "{mode:?}"
            );
        }

        let state = state(NumberFormatRequestOptions {
            minimum_fraction_digits: Some(2),
            maximum_fraction_digits: Some(2),
            rounding_increment: Some(25),
            use_grouping: Some(NumberFormatUseGrouping::Never),
            ..Default::default()
        });
        assert_eq!(
            format_number(&state, &parse_intl_mathematical_value("7.235")),
            Ok("7.25".to_owned())
        );
    }

    #[test]
    fn formats_locale_digits_grouping_and_parts() {
        let state =
            resolve_number_format(&["de-DE".to_owned()], NumberFormatRequestOptions::default())
                .expect("valid");
        let value = parse_intl_mathematical_value("123456.7894");
        assert_eq!(format_number(&state, &value), Ok("123.456,789".to_owned()));
        assert_eq!(
            format_number_to_parts(&state, &value),
            Ok(vec![
                NumberFormatPart {
                    kind: "integer",
                    value: "123".to_owned()
                },
                NumberFormatPart {
                    kind: "group",
                    value: ".".to_owned()
                },
                NumberFormatPart {
                    kind: "integer",
                    value: "456".to_owned()
                },
                NumberFormatPart {
                    kind: "decimal",
                    value: ",".to_owned()
                },
                NumberFormatPart {
                    kind: "fraction",
                    value: "789".to_owned()
                },
            ])
        );

        let arab = resolve_number_format(
            &["en-US-u-nu-arab".to_owned()],
            NumberFormatRequestOptions {
                use_grouping: Some(NumberFormatUseGrouping::Never),
                ..Default::default()
            },
        )
        .expect("valid");
        assert_eq!(
            format_number(&arab, &parse_intl_mathematical_value("123.4")),
            Ok("١٢٣.٤".to_owned())
        );
    }
}
