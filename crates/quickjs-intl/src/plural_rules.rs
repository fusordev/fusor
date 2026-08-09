//! ECMA-402 `PluralRules` locale resolution and ICU4X-backed selection.

use fixed_decimal::CompactDecimal;
use icu::{
    locale::Locale,
    plurals::{
        PluralCategory as IcuPluralCategory, PluralRuleType as IcuPluralRuleType, PluralRules,
        PluralRulesOptions, PluralRulesWithRanges,
    },
};

use crate::{
    IntlMathematicalValue, NumberFormatCompactDisplay, NumberFormatNotation,
    NumberFormatRequestOptions, NumberFormatRoundingMode, NumberFormatRoundingPriority,
    NumberFormatState, NumberFormatTrailingZeroDisplay, locale_components,
    number_format::prepare_notation, number_format_supported_locales, resolve_number_format,
};

const DEFAULT_LOCALE: &str = "en-US";

/// `PluralRules` construction or selection failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluralRulesError {
    InvalidLocale,
    InvalidOption,
    Data,
}

/// The resolved plural-rule family.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PluralRuleType {
    #[default]
    Cardinal,
    Ordinal,
}

impl PluralRuleType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cardinal => "cardinal",
            Self::Ordinal => "ordinal",
        }
    }

    const fn as_icu(self) -> IcuPluralRuleType {
        match self {
            Self::Cardinal => IcuPluralRuleType::Cardinal,
            Self::Ordinal => IcuPluralRuleType::Ordinal,
        }
    }
}

/// One ECMA-402 plural category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluralCategory {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

impl PluralCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::One => "one",
            Self::Two => "two",
            Self::Few => "few",
            Self::Many => "many",
            Self::Other => "other",
        }
    }
}

impl From<IcuPluralCategory> for PluralCategory {
    fn from(value: IcuPluralCategory) -> Self {
        match value {
            IcuPluralCategory::Zero => Self::Zero,
            IcuPluralCategory::One => Self::One,
            IcuPluralCategory::Two => Self::Two,
            IcuPluralCategory::Few => Self::Few,
            IcuPluralCategory::Many => Self::Many,
            IcuPluralCategory::Other => Self::Other,
        }
    }
}

/// Already-coerced JavaScript options passed into `PluralRules` resolution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PluralRulesRequestOptions {
    pub rule_type: Option<PluralRuleType>,
    pub notation: Option<NumberFormatNotation>,
    pub compact_display: Option<NumberFormatCompactDisplay>,
    pub minimum_integer_digits: Option<u8>,
    pub minimum_fraction_digits: Option<u8>,
    pub maximum_fraction_digits: Option<u8>,
    pub minimum_significant_digits: Option<u8>,
    pub maximum_significant_digits: Option<u8>,
    pub rounding_increment: Option<u16>,
    pub rounding_mode: Option<NumberFormatRoundingMode>,
    pub rounding_priority: Option<NumberFormatRoundingPriority>,
    pub trailing_zero_display: Option<NumberFormatTrailingZeroDisplay>,
}

/// Fully resolved immutable `PluralRules` internal slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluralRulesState {
    pub locale: String,
    pub rule_type: PluralRuleType,
    pub notation: NumberFormatNotation,
    pub compact_display: NumberFormatCompactDisplay,
    pub minimum_integer_digits: u8,
    pub minimum_fraction_digits: Option<u8>,
    pub maximum_fraction_digits: Option<u8>,
    pub minimum_significant_digits: Option<u8>,
    pub maximum_significant_digits: Option<u8>,
    pub plural_categories: Vec<PluralCategory>,
    pub rounding_increment: u16,
    pub rounding_mode: NumberFormatRoundingMode,
    pub rounding_priority: NumberFormatRoundingPriority,
    pub trailing_zero_display: NumberFormatTrailingZeroDisplay,
    number_format: NumberFormatState,
}

/// Resolves `PluralRules` locales, digit options, and available categories.
///
/// # Errors
///
/// Returns an error for invalid locale/number options or unavailable ICU data.
pub fn resolve_plural_rules(
    requested: &[String],
    options: PluralRulesRequestOptions,
) -> Result<PluralRulesState, PluralRulesError> {
    let requested_locale = requested
        .iter()
        .find(|locale| !number_format_supported_locales(&[(*locale).clone()]).is_empty())
        .map_or(DEFAULT_LOCALE, String::as_str);
    let locale = locale_components(requested_locale)
        .map_err(|_| PluralRulesError::InvalidLocale)?
        .base_name;
    let number_options = NumberFormatRequestOptions {
        notation: options.notation,
        compact_display: options.compact_display,
        minimum_integer_digits: options.minimum_integer_digits,
        minimum_fraction_digits: options.minimum_fraction_digits,
        maximum_fraction_digits: options.maximum_fraction_digits,
        minimum_significant_digits: options.minimum_significant_digits,
        maximum_significant_digits: options.maximum_significant_digits,
        rounding_increment: options.rounding_increment,
        rounding_mode: options.rounding_mode,
        rounding_priority: options.rounding_priority,
        trailing_zero_display: options.trailing_zero_display,
        ..NumberFormatRequestOptions::default()
    };
    let mut number_format = resolve_number_format(std::slice::from_ref(&locale), number_options)
        .map_err(|_| PluralRulesError::InvalidOption)?;
    // PluralRules has no relevant Unicode extension keys. Keep the selected
    // base locale even when the input contains a NumberFormat `nu` keyword.
    number_format.locale.clone_from(&locale);

    let rule_type = options.rule_type.unwrap_or_default();
    let plural_categories = if is_manx_cardinal(&locale, rule_type) {
        // ICU4X's compiled plural data currently omits Manx even though the
        // locale itself is supported. Keep the ECMA-402 service aligned with
        // CLDR's published `gv` cardinal rules.
        vec![
            PluralCategory::One,
            PluralCategory::Two,
            PluralCategory::Few,
            PluralCategory::Many,
            PluralCategory::Other,
        ]
    } else {
        plural_rules(&locale, rule_type)?
            .categories()
            .map(PluralCategory::from)
            .collect()
    };

    Ok(PluralRulesState {
        locale,
        rule_type,
        notation: number_format.notation,
        compact_display: number_format.compact_display,
        minimum_integer_digits: number_format.minimum_integer_digits,
        minimum_fraction_digits: number_format.minimum_fraction_digits,
        maximum_fraction_digits: number_format.maximum_fraction_digits,
        minimum_significant_digits: number_format.minimum_significant_digits,
        maximum_significant_digits: number_format.maximum_significant_digits,
        plural_categories,
        rounding_increment: number_format.rounding_increment,
        rounding_mode: number_format.rounding_mode,
        rounding_priority: number_format.rounding_priority,
        trailing_zero_display: number_format.trailing_zero_display,
        number_format,
    })
}

/// Returns the requested locales supported by the `PluralRules` data profile.
#[must_use]
pub fn plural_rules_supported_locales(requested: &[String]) -> Vec<String> {
    number_format_supported_locales(requested)
}

/// Selects the plural category for one already-coerced mathematical value.
///
/// # Errors
///
/// Returns an error if ICU plural or compact-notation data cannot be loaded.
pub fn select_plural(
    state: &PluralRulesState,
    value: &IntlMathematicalValue,
) -> Result<PluralCategory, PluralRulesError> {
    let IntlMathematicalValue::Finite(value) = value else {
        return Ok(PluralCategory::Other);
    };
    let (rounded, exponent) = prepare_notation(&state.number_format, value.clone())
        .map_err(|_| PluralRulesError::Data)?;
    if is_manx_cardinal(&state.locale, state.rule_type) {
        return Ok(select_manx_cardinal(&rounded));
    }
    let rules = plural_rules(&state.locale, state.rule_type)?;
    let category = if state.notation == NumberFormatNotation::Compact {
        let exponent = u8::try_from(exponent).map_err(|_| PluralRulesError::Data)?;
        let value = CompactDecimal::from_significand_and_exponent(rounded, exponent);
        rules.category_for(&value)
    } else {
        rules.category_for(&rounded)
    };
    Ok(category.into())
}

/// Selects the plural category for a range from the independently resolved
/// endpoint categories, as required by `ResolvePluralRange`.
///
/// # Errors
///
/// Returns an error if ICU plural-range data cannot be loaded.
pub fn select_plural_range(
    state: &PluralRulesState,
    start: &IntlMathematicalValue,
    end: &IntlMathematicalValue,
) -> Result<PluralCategory, PluralRulesError> {
    let start = select_plural(state, start)?;
    let end = select_plural(state, end)?;
    if is_manx_cardinal(&state.locale, state.rule_type) {
        // CLDR defines no Manx range overrides, so LDML's default is the end
        // category.
        return Ok(end);
    }
    let locale = plural_locale(&state.locale)?;
    let options = PluralRulesOptions::default().with_type(state.rule_type.as_icu());
    let ranges = PluralRulesWithRanges::try_new(locale.into(), options)
        .map_err(|_| PluralRulesError::Data)?;
    Ok(ranges
        .resolve_range(icu_category(start), icu_category(end))
        .into())
}

fn plural_rules(locale: &str, rule_type: PluralRuleType) -> Result<PluralRules, PluralRulesError> {
    let locale = plural_locale(locale)?;
    let options = PluralRulesOptions::default().with_type(rule_type.as_icu());
    PluralRules::try_new(locale.into(), options).map_err(|_| PluralRulesError::Data)
}

fn plural_locale(locale: &str) -> Result<Locale, PluralRulesError> {
    locale.parse().map_err(|_| PluralRulesError::InvalidLocale)
}

fn is_manx_cardinal(locale: &str, rule_type: PluralRuleType) -> bool {
    rule_type == PluralRuleType::Cardinal
        && (locale == "gv" || locale.strip_prefix("gv-").is_some())
}

fn select_manx_cardinal(value: &fixed_decimal::Decimal) -> PluralCategory {
    if *value.magnitude_range().start() < 0 {
        return PluralCategory::Many;
    }

    match (value.digit_at(1), value.digit_at(0)) {
        (_, 1) => PluralCategory::One,
        (_, 2) => PluralCategory::Two,
        (0 | 2 | 4 | 6 | 8, 0) => PluralCategory::Few,
        _ => PluralCategory::Other,
    }
}

const fn icu_category(value: PluralCategory) -> IcuPluralCategory {
    match value {
        PluralCategory::Zero => IcuPluralCategory::Zero,
        PluralCategory::One => IcuPluralCategory::One,
        PluralCategory::Two => IcuPluralCategory::Two,
        PluralCategory::Few => IcuPluralCategory::Few,
        PluralCategory::Many => IcuPluralCategory::Many,
        PluralCategory::Other => IcuPluralCategory::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{intl_mathematical_value_from_f64, parse_intl_mathematical_value};

    #[test]
    fn resolves_digits_categories_and_ignores_unicode_extensions() {
        let state = resolve_plural_rules(
            &["ar-u-nu-arab".to_owned()],
            PluralRulesRequestOptions::default(),
        )
        .unwrap();
        assert_eq!(state.locale, "ar");
        assert_eq!(state.minimum_integer_digits, 1);
        assert_eq!(state.minimum_fraction_digits, Some(0));
        assert_eq!(state.maximum_fraction_digits, Some(3));
        assert_eq!(
            state.plural_categories,
            [
                PluralCategory::Zero,
                PluralCategory::One,
                PluralCategory::Two,
                PluralCategory::Few,
                PluralCategory::Many,
                PluralCategory::Other,
            ]
        );
        for (locale, expected) in [
            ("en", vec![PluralCategory::One, PluralCategory::Other]),
            ("fa", vec![PluralCategory::One, PluralCategory::Other]),
            (
                "fr",
                vec![
                    PluralCategory::One,
                    PluralCategory::Many,
                    PluralCategory::Other,
                ],
            ),
            (
                "gv",
                vec![
                    PluralCategory::One,
                    PluralCategory::Two,
                    PluralCategory::Few,
                    PluralCategory::Many,
                    PluralCategory::Other,
                ],
            ),
            ("ko", vec![PluralCategory::Other]),
            (
                "sl",
                vec![
                    PluralCategory::One,
                    PluralCategory::Two,
                    PluralCategory::Few,
                    PluralCategory::Other,
                ],
            ),
        ] {
            let state =
                resolve_plural_rules(&[locale.to_owned()], PluralRulesRequestOptions::default())
                    .unwrap();
            assert_eq!(state.plural_categories, expected, "{locale}");
        }
    }

    #[test]
    fn selects_manx_cardinals_from_cldr_operands() {
        let state =
            resolve_plural_rules(&["gv".to_owned()], PluralRulesRequestOptions::default()).unwrap();
        for (value, expected) in [
            ("1", PluralCategory::One),
            ("12", PluralCategory::Two),
            ("80", PluralCategory::Few),
            ("103", PluralCategory::Other),
        ] {
            assert_eq!(
                select_plural(&state, &parse_intl_mathematical_value(value)),
                Ok(expected),
                "{value}"
            );
        }

        let fractional = resolve_plural_rules(
            &["gv".to_owned()],
            PluralRulesRequestOptions {
                minimum_fraction_digits: Some(1),
                ..PluralRulesRequestOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            select_plural(&fractional, &parse_intl_mathematical_value("1")),
            Ok(PluralCategory::Many)
        );
        assert_eq!(
            select_plural_range(
                &state,
                &parse_intl_mathematical_value("1"),
                &parse_intl_mathematical_value("12")
            ),
            Ok(PluralCategory::Two)
        );
    }

    #[test]
    fn selects_cardinal_ordinal_and_non_finite_categories() {
        let cardinal =
            resolve_plural_rules(&["en".to_owned()], PluralRulesRequestOptions::default()).unwrap();
        let ordinal = resolve_plural_rules(
            &["en".to_owned()],
            PluralRulesRequestOptions {
                rule_type: Some(PluralRuleType::Ordinal),
                ..PluralRulesRequestOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            select_plural(&cardinal, &intl_mathematical_value_from_f64(1.0)),
            Ok(PluralCategory::One)
        );
        assert_eq!(
            select_plural(&ordinal, &intl_mathematical_value_from_f64(2.0)),
            Ok(PluralCategory::Two)
        );
        assert_eq!(
            select_plural(&cardinal, &IntlMathematicalValue::NaN),
            Ok(PluralCategory::Other)
        );
    }

    #[test]
    fn compact_notation_preserves_the_cldr_exponent_operand() {
        let standard =
            resolve_plural_rules(&["fr".to_owned()], PluralRulesRequestOptions::default()).unwrap();
        let compact = resolve_plural_rules(
            &["fr".to_owned()],
            PluralRulesRequestOptions {
                notation: Some(NumberFormatNotation::Compact),
                ..PluralRulesRequestOptions::default()
            },
        )
        .unwrap();
        let value = parse_intl_mathematical_value("1500000");
        assert_eq!(select_plural(&standard, &value), Ok(PluralCategory::Other));
        assert_eq!(select_plural(&compact, &value), Ok(PluralCategory::Many));
    }

    #[test]
    fn resolves_en_range_categories_without_endpoint_order_restrictions() {
        let state =
            resolve_plural_rules(&["en-US".to_owned()], PluralRulesRequestOptions::default())
                .unwrap();
        for (start, end) in [(102.0, 201.0), (200.0, 200.0), (201.0, 102.0)] {
            assert_eq!(
                select_plural_range(
                    &state,
                    &intl_mathematical_value_from_f64(start),
                    &intl_mathematical_value_from_f64(end),
                ),
                Ok(PluralCategory::Other)
            );
        }
    }
}
