//! Pure ECMA-402 `%Intl.Locale%` record operations.

use icu::locale::{
    Locale, LocaleExpander,
    extensions::unicode::{Key, Value},
    subtags::{Language, Region, Script, Variant, Variants},
};

use super::{
    InvalidLocale, canonicalize_extension_aliases, canonicalize_locale, split_long_language,
};

/// One string-valued `%Intl.Locale%` constructor option.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocaleOptionKind {
    Language,
    Script,
    Region,
    Variants,
    Calendar,
    Collation,
    FirstDayOfWeek,
    HourCycle,
    CaseFirst,
    NumberingSystem,
}

/// Already converted and validated `%Intl.Locale%` constructor options.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocaleOptions {
    pub language: Option<String>,
    pub script: Option<String>,
    pub region: Option<String>,
    pub variants: Option<String>,
    pub calendar: Option<String>,
    pub collation: Option<String>,
    pub first_day_of_week: Option<String>,
    pub hour_cycle: Option<String>,
    pub case_first: Option<String>,
    pub numeric: Option<bool>,
    pub numbering_system: Option<String>,
}

/// Observable fields derived from a canonical `%Intl.Locale%` identifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocaleComponents {
    pub base_name: String,
    pub language: String,
    pub script: Option<String>,
    pub region: Option<String>,
    pub variants: Option<String>,
    pub calendar: Option<String>,
    pub collation: Option<String>,
    pub first_day_of_week: Option<String>,
    pub hour_cycle: Option<String>,
    pub case_first: Option<String>,
    pub numeric: bool,
    pub numbering_system: Option<String>,
}

/// Validates and case-normalizes one string-valued Locale option immediately
/// after JavaScript `ToString` completes.
///
/// # Errors
///
/// Returns [`InvalidLocale`] when `input` does not match the option's UTS #35
/// production or its closed ECMA-402 value set.
pub fn canonicalize_locale_option(
    kind: LocaleOptionKind,
    input: &str,
) -> Result<String, InvalidLocale> {
    if !input.is_ascii() {
        return Err(InvalidLocale);
    }
    let lower = input.to_ascii_lowercase();
    match kind {
        LocaleOptionKind::Language => {
            let valid = matches!(lower.len(), 2..=3 | 5..=8)
                && lower.bytes().all(|byte| byte.is_ascii_alphabetic())
                && lower != "root";
            valid.then_some(lower).ok_or(InvalidLocale)
        }
        LocaleOptionKind::Script => {
            if lower.len() != 4 || !lower.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                return Err(InvalidLocale);
            }
            let mut bytes = lower.into_bytes();
            bytes[0] = bytes[0].to_ascii_uppercase();
            String::from_utf8(bytes).map_err(|_| InvalidLocale)
        }
        LocaleOptionKind::Region => {
            let valid = (lower.len() == 2 && lower.bytes().all(|byte| byte.is_ascii_alphabetic()))
                || (lower.len() == 3 && lower.bytes().all(|byte| byte.is_ascii_digit()));
            valid
                .then(|| {
                    if lower.len() == 2 {
                        lower.to_ascii_uppercase()
                    } else {
                        lower
                    }
                })
                .ok_or(InvalidLocale)
        }
        LocaleOptionKind::Variants => canonicalize_variants(&lower),
        LocaleOptionKind::HourCycle => matches!(input, "h11" | "h12" | "h23" | "h24")
            .then_some(input.to_owned())
            .ok_or(InvalidLocale),
        LocaleOptionKind::CaseFirst => matches!(input, "upper" | "lower" | "false")
            .then_some(input.to_owned())
            .ok_or(InvalidLocale),
        LocaleOptionKind::FirstDayOfWeek => {
            let weekday = match lower.as_str() {
                "0" | "7" => "sun",
                "1" => "mon",
                "2" => "tue",
                "3" => "wed",
                "4" => "thu",
                "5" => "fri",
                "6" => "sat",
                _ => &lower,
            };
            validate_type_sequence(weekday).map(|()| weekday.to_owned())
        }
        LocaleOptionKind::Calendar
        | LocaleOptionKind::Collation
        | LocaleOptionKind::NumberingSystem => validate_type_sequence(&lower).map(|()| lower),
    }
}

/// Applies validated language and Unicode-keyword overrides and performs the
/// second canonicalization required by the Locale constructor.
///
/// # Errors
///
/// Returns [`InvalidLocale`] if `input` or an allegedly validated option can no
/// longer be represented as a structurally valid locale identifier.
pub fn apply_locale_options(input: &str, options: &LocaleOptions) -> Result<String, InvalidLocale> {
    let canonical = canonicalize_locale(input)?;
    let (original_long_language, icu_input) = split_long_language(&canonical)?;
    let mut long_language = original_long_language.map(str::to_owned);
    let mut locale = icu_input.parse::<Locale>().map_err(|_| InvalidLocale)?;

    if let Some(language) = &options.language {
        if language.len() <= 3 {
            locale.id.language = language.parse::<Language>().map_err(|_| InvalidLocale)?;
            long_language = None;
        } else {
            locale.id.language = Language::UNKNOWN;
            long_language = Some(language.clone());
        }
    }
    if let Some(script) = &options.script {
        locale.id.script = Some(script.parse::<Script>().map_err(|_| InvalidLocale)?);
    }
    if let Some(region) = &options.region {
        locale.id.region = Some(region.parse::<Region>().map_err(|_| InvalidLocale)?);
    }
    if let Some(variants) = &options.variants {
        let mut parsed = Vec::new();
        parsed
            .try_reserve_exact(variants.split('-').count())
            .map_err(|_| InvalidLocale)?;
        for variant in variants.split('-') {
            parsed.push(variant.parse::<Variant>().map_err(|_| InvalidLocale)?);
        }
        parsed.sort_unstable();
        locale.id.variants = Variants::from_vec_unchecked(parsed);
    }

    for (key, value) in [
        ("ca", options.calendar.as_deref()),
        ("co", options.collation.as_deref()),
        ("fw", options.first_day_of_week.as_deref()),
        ("hc", options.hour_cycle.as_deref()),
        ("kf", options.case_first.as_deref()),
        ("nu", options.numbering_system.as_deref()),
    ] {
        if let Some(value) = value {
            set_unicode_keyword(&mut locale, key, value)?;
        }
    }
    if let Some(numeric) = options.numeric {
        set_unicode_keyword(&mut locale, "kn", if numeric { "true" } else { "false" })?;
    }

    canonicalize_extension_aliases(&mut locale);
    let interim = restore_long_language(locale.to_string(), long_language.as_deref())?;
    canonicalize_locale(&interim)
}

/// Extracts all standardized Locale accessors from a canonical identifier.
///
/// # Errors
///
/// Returns [`InvalidLocale`] when `input` is not structurally valid.
pub fn locale_components(input: &str) -> Result<LocaleComponents, InvalidLocale> {
    let canonical = canonicalize_locale(input)?;
    let (long_language, icu_input) = split_long_language(&canonical)?;
    let locale = icu_input.parse::<Locale>().map_err(|_| InvalidLocale)?;
    let language = long_language.map_or_else(|| locale.id.language.to_string(), str::to_owned);
    let base_name = restore_long_language(locale.id.to_string(), long_language)?;
    let variants = (!locale.id.variants.is_empty()).then(|| locale.id.variants.to_string());
    let numeric_value = unicode_keyword(&locale, "kn");

    Ok(LocaleComponents {
        base_name,
        language,
        script: locale.id.script.map(|value| value.to_string()),
        region: locale.id.region.map(|value| value.to_string()),
        variants,
        calendar: unicode_keyword(&locale, "ca"),
        collation: unicode_keyword(&locale, "co"),
        first_day_of_week: unicode_keyword(&locale, "fw"),
        hour_cycle: unicode_keyword(&locale, "hc"),
        case_first: unicode_keyword(&locale, "kf"),
        numeric: numeric_value.as_deref().is_some_and(str::is_empty),
        numbering_system: unicode_keyword(&locale, "nu"),
    })
}

/// Runs UTS #35 Add Likely Subtags while preserving variants and extensions.
///
/// # Errors
///
/// Returns [`InvalidLocale`] when `input` is not structurally valid.
pub fn maximize_locale(input: &str) -> Result<String, InvalidLocale> {
    transform_likely_subtags(input, |expander, locale| {
        expander.maximize(&mut locale.id);
    })
}

/// Runs UTS #35 Remove Likely Subtags while preserving variants and extensions.
///
/// # Errors
///
/// Returns [`InvalidLocale`] when `input` is not structurally valid.
pub fn minimize_locale(input: &str) -> Result<String, InvalidLocale> {
    transform_likely_subtags(input, |expander, locale| {
        expander.minimize(&mut locale.id);
    })
}

fn transform_likely_subtags(
    input: &str,
    transform: impl FnOnce(&LocaleExpander, &mut Locale),
) -> Result<String, InvalidLocale> {
    let canonical = canonicalize_locale(input)?;
    let (long_language, icu_input) = split_long_language(&canonical)?;
    if long_language.is_some() {
        return Ok(canonical);
    }
    let mut locale = icu_input.parse::<Locale>().map_err(|_| InvalidLocale)?;
    transform(&LocaleExpander::new_extended(), &mut locale);
    canonicalize_extension_aliases(&mut locale);
    Ok(locale.to_string())
}

fn canonicalize_variants(input: &str) -> Result<String, InvalidLocale> {
    if input.is_empty() {
        return Err(InvalidLocale);
    }
    let mut variants = input.split('-').collect::<Vec<_>>();
    for variant in &variants {
        variant.parse::<Variant>().map_err(|_| InvalidLocale)?;
    }
    variants.sort_unstable();
    if variants.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(InvalidLocale);
    }
    Ok(variants.join("-"))
}

fn validate_type_sequence(input: &str) -> Result<(), InvalidLocale> {
    let mut subtags = input.split('-');
    let Some(first) = subtags.next() else {
        return Err(InvalidLocale);
    };
    if !valid_type_subtag(first) || !subtags.all(valid_type_subtag) {
        return Err(InvalidLocale);
    }
    Ok(())
}

fn valid_type_subtag(subtag: &str) -> bool {
    (3..=8).contains(&subtag.len()) && subtag.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn set_unicode_keyword(locale: &mut Locale, key: &str, value: &str) -> Result<(), InvalidLocale> {
    let key = key.parse::<Key>().map_err(|_| InvalidLocale)?;
    let value = value.parse::<Value>().map_err(|_| InvalidLocale)?;
    locale.extensions.unicode.keywords.set(key, value);
    Ok(())
}

fn unicode_keyword(locale: &Locale, key: &str) -> Option<String> {
    let key = key.parse::<Key>().ok()?;
    locale
        .extensions
        .unicode
        .keywords
        .get(&key)
        .map(ToString::to_string)
}

fn restore_long_language(
    serialized: String,
    long_language: Option<&str>,
) -> Result<String, InvalidLocale> {
    let Some(language) = long_language else {
        return Ok(serialized);
    };
    serialized
        .strip_prefix("und")
        .map(|suffix| format!("{language}{suffix}"))
        .ok_or(InvalidLocale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_constructor_options_at_their_spec_boundaries() {
        for (kind, input, expected) in [
            (LocaleOptionKind::Language, "DE", "de"),
            (LocaleOptionKind::Script, "lATN", "Latn"),
            (LocaleOptionKind::Region, "us", "US"),
            (LocaleOptionKind::Region, "554", "554"),
            (
                LocaleOptionKind::Variants,
                "1xyz-1234-abcde-12345678",
                "1234-12345678-1xyz-abcde",
            ),
            (LocaleOptionKind::FirstDayOfWeek, "0", "sun"),
            (LocaleOptionKind::Calendar, "ABC123", "abc123"),
        ] {
            assert_eq!(
                canonicalize_locale_option(kind, input),
                Ok(expected.to_owned())
            );
        }
        for (kind, input) in [
            (LocaleOptionKind::Language, "abcd"),
            (LocaleOptionKind::Script, "lat"),
            (LocaleOptionKind::Region, "usa"),
            (LocaleOptionKind::Variants, "fonipa-FONIPA"),
            (LocaleOptionKind::HourCycle, "h25"),
            (LocaleOptionKind::HourCycle, "H12"),
            (LocaleOptionKind::CaseFirst, "true"),
            (LocaleOptionKind::CaseFirst, "Upper"),
            (LocaleOptionKind::Calendar, "ab"),
        ] {
            assert_eq!(canonicalize_locale_option(kind, input), Err(InvalidLocale));
        }
    }

    #[test]
    fn applies_language_and_unicode_keyword_options_then_canonicalizes_again() {
        let options = LocaleOptions {
            language: Some("ru".to_owned()),
            calendar: Some("islamicc".to_owned()),
            numeric: Some(true),
            ..LocaleOptions::default()
        };
        assert_eq!(
            apply_locale_options("und-Armn-SU", &options),
            Ok("ru-Armn-AM-u-ca-islamic-civil-kn".to_owned())
        );
    }

    #[test]
    fn extracts_locale_components() {
        let components = locale_components(
            "de-latn-de-fonipa-1996-u-ca-gregory-co-phonebk-hc-h23-kf-true-kn-false-nu-latn",
        )
        .expect("components");
        assert_eq!(components.base_name, "de-Latn-DE-1996-fonipa");
        assert_eq!(components.language, "de");
        assert_eq!(components.script.as_deref(), Some("Latn"));
        assert_eq!(components.region.as_deref(), Some("DE"));
        assert_eq!(components.variants.as_deref(), Some("1996-fonipa"));
        assert_eq!(components.calendar.as_deref(), Some("gregory"));
        assert_eq!(components.case_first.as_deref(), Some(""));
        assert!(!components.numeric);
    }

    #[test]
    fn transforms_likely_subtags_without_losing_extensions() {
        assert_eq!(
            maximize_locale("und-Thai-u-ca-gregory"),
            Ok("th-Thai-TH-u-ca-gregory".to_owned())
        );
        assert_eq!(
            minimize_locale("en-Latn-GB-x-private"),
            Ok("en-GB-x-private".to_owned())
        );
    }
}
