//! ECMA-402 `DisplayNames` resolution and ICU4X-backed display-name lookup.

use std::collections::HashSet;

use icu::{
    experimental::displaynames::{
        DisplayNamesOptions, Fallback, LanguageDisplay, Style,
        multi::{
            LanguageDisplayNames, LocaleDisplayNamesFormatter, RegionDisplayNames,
            ScriptDisplayNames,
        },
    },
    locale::{Locale, subtags::Region, subtags::Script},
};

use crate::{InvalidLocale, canonicalize_locale, locale_components};

const DEFAULT_LOCALE: &str = "en-US";

/// Display-name construction or lookup failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayNamesError {
    InvalidLocale,
    InvalidCode,
    MissingType,
    Data,
}

impl From<InvalidLocale> for DisplayNamesError {
    fn from(_: InvalidLocale) -> Self {
        Self::InvalidLocale
    }
}

/// Width of a localized display name.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DisplayNamesStyle {
    Narrow,
    Short,
    #[default]
    Long,
}

impl DisplayNamesStyle {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Narrow => "narrow",
            Self::Short => "short",
            Self::Long => "long",
        }
    }

    const fn as_icu(self) -> Style {
        match self {
            Self::Narrow => Style::Narrow,
            Self::Short => Style::Short,
            Self::Long => Style::Long,
        }
    }
}

/// Kind of code accepted by a display-name object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayNamesType {
    Language,
    Region,
    Script,
    Currency,
    Calendar,
    DateTimeField,
}

impl DisplayNamesType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Language => "language",
            Self::Region => "region",
            Self::Script => "script",
            Self::Currency => "currency",
            Self::Calendar => "calendar",
            Self::DateTimeField => "dateTimeField",
        }
    }
}

/// Result used when locale data has no display name for a code.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DisplayNamesFallback {
    #[default]
    Code,
    None,
}

impl DisplayNamesFallback {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::None => "none",
        }
    }

    const fn as_icu(self) -> Fallback {
        match self {
            Self::Code => Fallback::Code,
            Self::None => Fallback::None,
        }
    }
}

/// Whether language names prefer dialect or standard forms.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DisplayNamesLanguageDisplay {
    #[default]
    Dialect,
    Standard,
}

impl DisplayNamesLanguageDisplay {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dialect => "dialect",
            Self::Standard => "standard",
        }
    }

    const fn as_icu(self) -> LanguageDisplay {
        match self {
            Self::Dialect => LanguageDisplay::Dialect,
            Self::Standard => LanguageDisplay::Standard,
        }
    }
}

/// Already-coerced JavaScript options passed into display-name resolution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DisplayNamesRequestOptions {
    pub style: Option<DisplayNamesStyle>,
    pub name_type: Option<DisplayNamesType>,
    pub fallback: Option<DisplayNamesFallback>,
    pub language_display: Option<DisplayNamesLanguageDisplay>,
}

/// Fully resolved immutable `DisplayNames` internal slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayNamesState {
    pub locale: String,
    pub style: DisplayNamesStyle,
    pub name_type: DisplayNamesType,
    pub fallback: DisplayNamesFallback,
    pub language_display: DisplayNamesLanguageDisplay,
}

/// Resolves requested locales and options into `DisplayNames` internal slots.
///
/// # Errors
///
/// Returns an error when the required `type` option is absent, the selected
/// locale is malformed, or ICU locale data cannot be loaded.
pub fn resolve_display_names(
    requested: &[String],
    options: DisplayNamesRequestOptions,
) -> Result<DisplayNamesState, DisplayNamesError> {
    let requested_locale = requested
        .iter()
        .find(|locale| locale_is_supported(locale))
        .map_or(DEFAULT_LOCALE, String::as_str);
    let locale = locale_components(requested_locale)?.base_name;
    let state = DisplayNamesState {
        locale,
        style: options.style.unwrap_or_default(),
        name_type: options.name_type.ok_or(DisplayNamesError::MissingType)?,
        fallback: options.fallback.unwrap_or_default(),
        language_display: options.language_display.unwrap_or_default(),
    };
    validate_data(&state)?;
    Ok(state)
}

/// Returns requested locales supported by the ICU4X display-name profile.
#[must_use]
pub fn display_names_supported_locales(requested: &[String]) -> Vec<String> {
    requested
        .iter()
        .filter(|locale| locale_is_supported(locale))
        .cloned()
        .collect()
}

/// Canonicalizes a code according to the resolved `DisplayNames` `type`.
///
/// # Errors
///
/// Returns [`DisplayNamesError::InvalidCode`] when `code` does not match the
/// corresponding UTS #35 grammar or ECMA-402 enumerated field set.
pub fn canonicalize_display_names_code(
    name_type: DisplayNamesType,
    code: &str,
) -> Result<String, DisplayNamesError> {
    match name_type {
        DisplayNamesType::Language => canonicalize_language_code(code),
        DisplayNamesType::Region => canonicalize_region_code(code),
        DisplayNamesType::Script => canonicalize_script_code(code),
        DisplayNamesType::Currency => canonicalize_currency_code(code),
        DisplayNamesType::Calendar => canonicalize_calendar_code(code),
        DisplayNamesType::DateTimeField => canonicalize_date_time_field(code),
    }
}

/// Looks up a localized display name, applying the resolved fallback policy.
///
/// # Errors
///
/// Returns an error for an invalid code or unavailable ICU data.
pub fn display_name(
    state: &DisplayNamesState,
    code: &str,
) -> Result<Option<String>, DisplayNamesError> {
    let canonical = canonicalize_display_names_code(state.name_type, code)?;
    let localized = match state.name_type {
        DisplayNamesType::Language => language_display_name(state, &canonical)?,
        DisplayNamesType::Region => region_display_name(state, &canonical)?,
        DisplayNamesType::Script => script_display_name(state, &canonical)?,
        // ICU4X's experimental DisplayNames service currently publishes no
        // direct calendar or date-time-field lookup API. Its currency payload
        // is formatter-oriented rather than DisplayNames-oriented. Treat a
        // missing entry exactly as ECMA-402 requires and let `fallback` decide.
        DisplayNamesType::Calendar => calendar_display_name(&canonical).map(str::to_owned),
        DisplayNamesType::Currency | DisplayNamesType::DateTimeField => None,
    };
    Ok(localized.or(match state.fallback {
        DisplayNamesFallback::Code => Some(canonical),
        DisplayNamesFallback::None => None,
    }))
}

fn validate_data(state: &DisplayNamesState) -> Result<(), DisplayNamesError> {
    let locale = display_locale(state)?;
    let options = icu_options(state);
    match state.name_type {
        DisplayNamesType::Language => {
            LocaleDisplayNamesFormatter::try_new(locale.into(), options)
                .map_err(|_| DisplayNamesError::Data)?;
        }
        DisplayNamesType::Region => {
            RegionDisplayNames::try_new(locale.into(), options)
                .map_err(|_| DisplayNamesError::Data)?;
        }
        DisplayNamesType::Script => {
            ScriptDisplayNames::try_new(locale.into(), options)
                .map_err(|_| DisplayNamesError::Data)?;
        }
        DisplayNamesType::Currency
        | DisplayNamesType::Calendar
        | DisplayNamesType::DateTimeField => {}
    }
    Ok(())
}

fn language_display_name(
    state: &DisplayNamesState,
    canonical: &str,
) -> Result<Option<String>, DisplayNamesError> {
    let Ok(code_locale) = canonical.parse::<Locale>() else {
        return Ok(None);
    };
    let locale = display_locale(state)?;
    let options = icu_options(state);
    if matches!(state.fallback, DisplayNamesFallback::None) {
        let names = LanguageDisplayNames::try_new(locale.clone().into(), options)
            .map_err(|_| DisplayNamesError::Data)?;
        if names.of(code_locale.id.language).is_none() {
            return Ok(None);
        }
    }
    let names = LocaleDisplayNamesFormatter::try_new(locale.into(), options)
        .map_err(|_| DisplayNamesError::Data)?;
    Ok(Some(names.of(&code_locale).into_owned()))
}

fn region_display_name(
    state: &DisplayNamesState,
    canonical: &str,
) -> Result<Option<String>, DisplayNamesError> {
    let region = canonical
        .parse::<Region>()
        .map_err(|_| DisplayNamesError::InvalidCode)?;
    let names = RegionDisplayNames::try_new(display_locale(state)?.into(), icu_options(state))
        .map_err(|_| DisplayNamesError::Data)?;
    Ok(names.of(region).map(str::to_owned))
}

fn script_display_name(
    state: &DisplayNamesState,
    canonical: &str,
) -> Result<Option<String>, DisplayNamesError> {
    let script = canonical
        .parse::<Script>()
        .map_err(|_| DisplayNamesError::InvalidCode)?;
    let names = ScriptDisplayNames::try_new(display_locale(state)?.into(), icu_options(state))
        .map_err(|_| DisplayNamesError::Data)?;
    Ok(names.of(script).map(str::to_owned))
}

fn display_locale(state: &DisplayNamesState) -> Result<Locale, DisplayNamesError> {
    state
        .locale
        .parse::<Locale>()
        .map_err(|_| DisplayNamesError::InvalidLocale)
}

fn icu_options(state: &DisplayNamesState) -> DisplayNamesOptions {
    let mut options = DisplayNamesOptions::default();
    options.style = Some(state.style.as_icu());
    options.fallback = state.fallback.as_icu();
    options.language_display = state.language_display.as_icu();
    options
}

fn canonicalize_language_code(code: &str) -> Result<String, DisplayNamesError> {
    let subtags = code.split('-').collect::<Vec<_>>();
    let Some(language) = subtags.first().copied() else {
        return Err(DisplayNamesError::InvalidCode);
    };
    if language.eq_ignore_ascii_case("root")
        || !matches!(language.len(), 2 | 3 | 5..=8)
        || !language.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        return Err(DisplayNamesError::InvalidCode);
    }

    let mut index = 1;
    if subtags.get(index).is_some_and(|subtag| {
        subtag.len() == 4 && subtag.bytes().all(|byte| byte.is_ascii_alphabetic())
    }) {
        index += 1;
    }
    if subtags.get(index).is_some_and(|subtag| {
        (subtag.len() == 2 && subtag.bytes().all(|byte| byte.is_ascii_alphabetic()))
            || (subtag.len() == 3 && subtag.bytes().all(|byte| byte.is_ascii_digit()))
    }) {
        index += 1;
    }

    let mut variants = HashSet::new();
    for variant in &subtags[index..] {
        let bytes = variant.as_bytes();
        let well_formed = ((5..=8).contains(&bytes.len())
            && bytes.iter().all(u8::is_ascii_alphanumeric))
            || (bytes.len() == 4
                && bytes.first().is_some_and(u8::is_ascii_digit)
                && bytes.iter().all(u8::is_ascii_alphanumeric));
        if !well_formed || !variants.insert(variant.to_ascii_lowercase()) {
            return Err(DisplayNamesError::InvalidCode);
        }
    }

    canonicalize_locale(code).map_err(|_| DisplayNamesError::InvalidCode)
}

fn canonicalize_region_code(code: &str) -> Result<String, DisplayNamesError> {
    if (code.len() == 2 && code.bytes().all(|byte| byte.is_ascii_alphabetic()))
        || (code.len() == 3 && code.bytes().all(|byte| byte.is_ascii_digit()))
    {
        Ok(code.to_ascii_uppercase())
    } else {
        Err(DisplayNamesError::InvalidCode)
    }
}

fn canonicalize_script_code(code: &str) -> Result<String, DisplayNamesError> {
    if code.len() != 4 || !code.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(DisplayNamesError::InvalidCode);
    }
    let mut canonical = code.to_ascii_lowercase();
    canonical
        .get_mut(..1)
        .ok_or(DisplayNamesError::InvalidCode)?
        .make_ascii_uppercase();
    Ok(canonical)
}

fn canonicalize_currency_code(code: &str) -> Result<String, DisplayNamesError> {
    if code.len() == 3 && code.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        Ok(code.to_ascii_uppercase())
    } else {
        Err(DisplayNamesError::InvalidCode)
    }
}

fn canonicalize_calendar_code(code: &str) -> Result<String, DisplayNamesError> {
    if code.split('-').all(|subtag| {
        (3..=8).contains(&subtag.len()) && subtag.bytes().all(|byte| byte.is_ascii_alphanumeric())
    }) {
        Ok(code.to_ascii_lowercase())
    } else {
        Err(DisplayNamesError::InvalidCode)
    }
}

fn canonicalize_date_time_field(code: &str) -> Result<String, DisplayNamesError> {
    if matches!(
        code,
        "era"
            | "year"
            | "quarter"
            | "month"
            | "weekOfYear"
            | "weekday"
            | "day"
            | "dayPeriod"
            | "hour"
            | "minute"
            | "second"
            | "timeZoneName"
    ) {
        Ok(code.to_owned())
    } else {
        Err(DisplayNamesError::InvalidCode)
    }
}

fn calendar_display_name(code: &str) -> Option<&'static str> {
    match code {
        "buddhist" => Some("Buddhist Calendar"),
        "chinese" => Some("Chinese Calendar"),
        "coptic" => Some("Coptic Calendar"),
        "dangi" => Some("Dangi Calendar"),
        "ethioaa" => Some("Ethiopic Amete Alem Calendar"),
        "ethiopic" => Some("Ethiopic Calendar"),
        "gregory" => Some("Gregorian Calendar"),
        "hebrew" => Some("Hebrew Calendar"),
        "indian" => Some("Indian National Calendar"),
        "islamic-civil" => Some("Islamic Civil Calendar"),
        "islamic-tbla" => Some("Islamic Tabular Calendar"),
        "islamic-umalqura" => Some("Islamic Umm al-Qura Calendar"),
        "iso8601" => Some("ISO-8601 Calendar"),
        "japanese" => Some("Japanese Calendar"),
        "persian" => Some("Persian Calendar"),
        "roc" => Some("Minguo Calendar"),
        _ => None,
    }
}

fn locale_is_supported(locale: &str) -> bool {
    let Ok(components) = locale_components(locale) else {
        return false;
    };
    !matches!(components.language.as_str(), "und" | "zxx" | "tlh")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name_type: DisplayNamesType) -> DisplayNamesState {
        resolve_display_names(
            &["en-US".to_owned()],
            DisplayNamesRequestOptions {
                name_type: Some(name_type),
                ..DisplayNamesRequestOptions::default()
            },
        )
        .expect("DisplayNames")
    }

    #[test]
    fn resolves_required_type_and_options() {
        assert_eq!(
            resolve_display_names(&[], DisplayNamesRequestOptions::default()),
            Err(DisplayNamesError::MissingType)
        );
        let resolved = resolve_display_names(
            &["fr-u-ca-gregory".to_owned()],
            DisplayNamesRequestOptions {
                style: Some(DisplayNamesStyle::Short),
                name_type: Some(DisplayNamesType::Language),
                fallback: Some(DisplayNamesFallback::None),
                language_display: Some(DisplayNamesLanguageDisplay::Standard),
            },
        )
        .expect("DisplayNames");
        assert_eq!(resolved.locale, "fr");
        assert_eq!(resolved.style, DisplayNamesStyle::Short);
        assert_eq!(resolved.fallback, DisplayNamesFallback::None);
        assert_eq!(
            resolved.language_display,
            DisplayNamesLanguageDisplay::Standard
        );
    }

    #[test]
    fn canonicalizes_each_code_grammar() {
        for (name_type, input, expected) in [
            (DisplayNamesType::Language, "EN-latn-us", "en-Latn-US"),
            (DisplayNamesType::Language, "iw", "he"),
            (DisplayNamesType::Region, "us", "US"),
            (DisplayNamesType::Region, "419", "419"),
            (DisplayNamesType::Script, "lAtN", "Latn"),
            (DisplayNamesType::Currency, "usd", "USD"),
            (DisplayNamesType::Calendar, "ABC-def", "abc-def"),
            (DisplayNamesType::DateTimeField, "weekOfYear", "weekOfYear"),
        ] {
            assert_eq!(
                canonicalize_display_names_code(name_type, input),
                Ok(expected.to_owned()),
                "{input}"
            );
        }
    }

    #[test]
    fn rejects_test262_code_shapes() {
        for code in [
            "",
            "a",
            "abcd",
            "abcdefghi",
            "root",
            "en-u-hebrew",
            "aa-aaaa-bbbb",
            "aa-aaaaa-AAAAA",
            "aa-bb-cc",
            "en_GB",
        ] {
            assert_eq!(
                canonicalize_display_names_code(DisplayNamesType::Language, code),
                Err(DisplayNamesError::InvalidCode),
                "{code}"
            );
        }
        for code in ["00", "a", "aaa", "a01", "1a", " aa"] {
            assert_eq!(
                canonicalize_display_names_code(DisplayNamesType::Region, code),
                Err(DisplayNamesError::InvalidCode),
                "{code}"
            );
        }
        for code in ["00", "000000000", "abc_def", "abc-"] {
            assert_eq!(
                canonicalize_display_names_code(DisplayNamesType::Calendar, code),
                Err(DisplayNamesError::InvalidCode),
                "{code}"
            );
        }
    }

    #[test]
    fn uses_icu_names_and_fallback_policy() {
        assert_eq!(
            display_name(&state(DisplayNamesType::Language), "de"),
            Ok(Some("German".to_owned()))
        );
        assert_eq!(
            display_name(&state(DisplayNamesType::Region), "GB"),
            Ok(Some("United Kingdom".to_owned()))
        );
        assert_eq!(
            display_name(&state(DisplayNamesType::Calendar), "GREGORY"),
            Ok(Some("Gregorian Calendar".to_owned()))
        );

        let no_fallback = resolve_display_names(
            &["en".to_owned()],
            DisplayNamesRequestOptions {
                name_type: Some(DisplayNamesType::Calendar),
                fallback: Some(DisplayNamesFallback::None),
                ..DisplayNamesRequestOptions::default()
            },
        )
        .expect("DisplayNames");
        assert_eq!(
            display_name(&no_fallback, "gregory"),
            Ok(Some("Gregorian Calendar".to_owned()))
        );
        assert_eq!(display_name(&no_fallback, "abc"), Ok(None));
    }
}
