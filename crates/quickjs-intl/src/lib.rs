//! ECMA-402 locale algorithms backed directly by pinned ICU4X data.
//!
//! This crate owns Unicode locale parsing, validation, canonicalization, and
//! service data. JavaScript coercion and observable property access remain in
//! `quickjs-runtime`; the two layers exchange owned, validated Rust values.

#![forbid(unsafe_code)]

use icu::locale::{
    Locale, LocaleCanonicalizer,
    extensions::{transform, unicode},
};

mod collator;
mod date_time_format;
mod locale;
mod locale_info;
mod number_format;
mod plural_rules;
mod relative_time_format;
mod supported_values;

pub use collator::{
    CollatorRequestOptions, CollatorSensitivity, CollatorState, CollatorUsage,
    collator_supported_locales, compare_with_collator, resolve_collator,
};
pub use date_time_format::{
    DateTimeComponentStyle, DateTimeFormatError, DateTimeFormatInput, DateTimeFormatInputKind,
    DateTimeFormatMatcher, DateTimeFormatPart, DateTimeFormatRequestOptions, DateTimeFormatState,
    DateTimeHourCycle, DateTimeStyle, DateTimeTimeZoneName, canonicalize_time_zone,
    date_time_format_supported_locales, format_datetime, format_datetime_to_parts,
    resolve_date_time_format,
};

pub use locale::{
    LocaleComponents, LocaleOptionKind, LocaleOptions, apply_locale_options,
    canonicalize_locale_option, locale_components, maximize_locale, minimize_locale,
};
pub use locale_info::{
    LocaleWeekInfo, calendars_of_locale, collations_of_locale, hour_cycles_of_locale,
    numbering_systems_of_locale, text_direction_of_locale, time_zones_of_locale,
    week_info_of_locale,
};
pub use number_format::{
    IntlMathematicalValue, NumberFormatCompactDisplay, NumberFormatCurrencyDisplay,
    NumberFormatCurrencySign, NumberFormatError, NumberFormatNotation, NumberFormatPart,
    NumberFormatRequestOptions, NumberFormatRoundingMode, NumberFormatRoundingPriority,
    NumberFormatSignDisplay, NumberFormatState, NumberFormatStyle, NumberFormatTrailingZeroDisplay,
    NumberFormatUnitDisplay, NumberFormatUseGrouping, format_number, format_number_to_parts,
    intl_mathematical_value_from_f64, is_well_formed_currency_code, is_well_formed_unit_identifier,
    number_format_supported_locales, parse_intl_mathematical_value, resolve_number_format,
};
pub use plural_rules::{
    PluralCategory, PluralRuleType, PluralRulesError, PluralRulesRequestOptions, PluralRulesState,
    plural_rules_supported_locales, resolve_plural_rules, select_plural, select_plural_range,
};
pub use relative_time_format::{
    RelativeTimeFormatError, RelativeTimeFormatNumeric, RelativeTimeFormatPart,
    RelativeTimeFormatRequestOptions, RelativeTimeFormatState, RelativeTimeFormatStyle,
    RelativeTimeUnit, format_relative_time, format_relative_time_to_parts,
    relative_time_format_supported_locales, resolve_relative_time_format,
};
pub use supported_values::supported_values;

/// A locale identifier that is not structurally valid under UTS #35.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLocale;

/// Implements ECMA-402 `CanonicalizeUnicodeLocaleId` with ICU4X's extended
/// UTS #35 canonicalizer and compiled CLDR alias data.
///
/// # Errors
///
/// Returns [`InvalidLocale`] when `input` is not a structurally valid Unicode
/// locale identifier.
pub fn canonicalize_locale(input: &str) -> Result<String, InvalidLocale> {
    let (long_language, icu_input) = split_long_language(input)?;
    let mut locale = icu_input.parse::<Locale>().map_err(|_| InvalidLocale)?;
    LocaleCanonicalizer::new_extended().canonicalize(&mut locale);
    canonicalize_extension_aliases(&mut locale);

    let canonical = locale.to_string();
    match long_language {
        Some(language) => canonical
            .strip_prefix("und")
            .map(|suffix| format!("{}{}", language.to_ascii_lowercase(), suffix))
            .ok_or(InvalidLocale),
        None => Ok(canonical),
    }
}

/// ICU4X intentionally stores ISO 639 language subtags in a compact 2-3 byte
/// representation. ECMA-402 also admits the reserved 5-8 letter form, so use
/// `und` as an ICU parsing/canonicalization sentinel and restore that subtag
/// after the remaining locale identifier has been validated.
fn split_long_language(input: &str) -> Result<(Option<&str>, String), InvalidLocale> {
    let language_end = input.find('-').unwrap_or(input.len());
    let language = input.get(..language_end).ok_or(InvalidLocale)?;
    if !(5..=8).contains(&language.len()) {
        return Ok((None, input.to_owned()));
    }
    if !language.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(InvalidLocale);
    }
    let suffix = input.get(language_end..).ok_or(InvalidLocale)?;
    Ok((Some(language), format!("und{suffix}")))
}

fn canonicalize_extension_aliases(locale: &mut Locale) {
    // LocaleCanonicalizer handles language, script, region, variant, and
    // subdivision aliases. ICU4X 2.2 deliberately leaves the general U/T
    // extension type aliases to service-specific code, while ECMA-402's
    // CanonicalizeUnicodeLocaleId requires them here as well.
    for (key, from, to) in [
        ("ca", "ethiopic-amete-alem", "ethioaa"),
        ("ca", "islamicc", "islamic-civil"),
        ("ks", "primary", "level1"),
        ("ks", "tertiary", "level3"),
        ("ms", "imperial", "uksystem"),
        ("tz", "cnckg", "cnsha"),
        ("tz", "eire", "iedub"),
        ("tz", "est", "papty"),
        ("tz", "gmt0", "gmt"),
        ("tz", "uct", "utc"),
        ("tz", "zulu", "utc"),
    ] {
        replace_unicode_keyword(locale, key, from, to);
    }
    for key in ["kb", "kc", "kh", "kk", "kn"] {
        replace_unicode_keyword(locale, key, "yes", "true");
    }

    let key = "m0".parse::<transform::Key>().expect("static T key");
    let alias = "names".parse::<transform::Value>().expect("static T value");
    let is_alias = locale
        .extensions
        .transform
        .fields
        .get(&key)
        .is_some_and(|value| value == &alias);
    if is_alias {
        let canonical = "prprname"
            .parse::<transform::Value>()
            .expect("static T value");
        locale.extensions.transform.fields.set(key, canonical);
    }
}

fn replace_unicode_keyword(locale: &mut Locale, key: &str, from: &str, to: &str) {
    let key = key.parse::<unicode::Key>().expect("static U key");
    let alias = from.parse::<unicode::Value>().expect("static U value");
    let Some(value) = locale.extensions.unicode.keywords.get_mut(&key) else {
        return;
    };
    if *value == alias {
        *value = to.parse::<unicode::Value>().expect("static U value");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_test262_language_and_region_aliases() {
        for (input, expected) in [
            ("de", "de"),
            ("DE-de", "de-DE"),
            ("cmn", "zh"),
            ("CMN-hANS", "zh-Hans"),
            ("cmn-hans-cn", "zh-Hans-CN"),
            ("sgn-GR", "gss"),
            ("ji", "yi"),
            ("de-DD", "de-DE"),
            ("in", "id"),
            ("sl-rozaj-biske-1994", "sl-1994-biske-rozaj"),
        ] {
            assert_eq!(
                canonicalize_locale(input),
                Ok(expected.to_owned()),
                "{input}"
            );
        }
    }

    #[test]
    fn canonicalizes_and_orders_extensions() {
        for (input, expected) in [
            ("es-419-u-nu-latn", "es-419-u-nu-latn"),
            ("cmn-hans-cn-u-ca-t-ca-x-t-u", "zh-Hans-CN-t-ca-u-ca-x-t-u"),
            ("da-u-attr-co-search", "da-u-attr-co-search"),
            ("und-u-ca-ethiopic-amete-alem", "und-u-ca-ethioaa"),
            ("und-u-ca-islamicc", "und-u-ca-islamic-civil"),
            ("und-u-ks-primary", "und-u-ks-level1"),
            ("und-u-ms-imperial", "und-u-ms-uksystem"),
            ("und-u-tz-zulu", "und-u-tz-utc"),
            ("und-u-kn-yes", "und-u-kn"),
            ("und-u-ka-yes", "und-u-ka-yes"),
            ("en-t-iw", "en-t-he"),
            ("en-t-m0-names", "en-t-m0-prprname"),
        ] {
            assert_eq!(
                canonicalize_locale(input),
                Ok(expected.to_owned()),
                "{input}"
            );
        }
    }

    #[test]
    fn admits_reserved_long_language_subtags() {
        for (input, expected) in [
            ("posix", "posix"),
            ("GERMAN-latn-us-u-nu-latn", "german-Latn-US-u-nu-latn"),
        ] {
            assert_eq!(
                canonicalize_locale(input),
                Ok(expected.to_owned()),
                "{input}"
            );
        }
    }

    #[test]
    fn rejects_test262_structural_failures() {
        for input in [
            "",
            "i",
            "x",
            "u",
            "419",
            "de_DE",
            "de-u",
            "de-1996-1996",
            "pt-u-ca-gregory-u-nu-latn",
            "x-foo",
            " en",
            "en ",
        ] {
            assert_eq!(canonicalize_locale(input), Err(InvalidLocale), "{input}");
        }
    }
}
