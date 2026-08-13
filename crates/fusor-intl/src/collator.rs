//! ECMA-402 Collator resolution and ICU4X-backed comparison.

use core::cmp::Ordering;

use icu::{
    collator::{
        Collator, CollatorPreferences,
        options::{AlternateHandling, CaseLevel, CollatorOptions, Strength},
        preferences::{CollationCaseFirst, CollationNumericOrdering},
    },
    locale::{Locale, extensions::Extensions},
};

use crate::{InvalidLocale, canonicalize_locale, locale_components};

const DEFAULT_LOCALE: &str = "en-US";

/// The resolved `usage` option of an `Intl.Collator`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CollatorUsage {
    /// Locale-sensitive sorting.
    #[default]
    Sort,
    /// Full-string locale-sensitive searching.
    Search,
}

impl CollatorUsage {
    /// Returns the ECMA-402 option spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sort => "sort",
            Self::Search => "search",
        }
    }
}

/// The resolved `sensitivity` option of an `Intl.Collator`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CollatorSensitivity {
    Base,
    Accent,
    Case,
    #[default]
    Variant,
}

impl CollatorSensitivity {
    /// Returns the ECMA-402 option spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Accent => "accent",
            Self::Case => "case",
            Self::Variant => "variant",
        }
    }
}

/// Fully resolved, immutable Collator internal slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollatorState {
    pub locale: String,
    pub usage: CollatorUsage,
    pub sensitivity: CollatorSensitivity,
    pub ignore_punctuation: bool,
    pub collation: String,
    pub numeric: bool,
    pub case_first: String,
}

/// Already-coerced JavaScript options passed into Collator resolution.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CollatorRequestOptions {
    pub usage: Option<CollatorUsage>,
    pub collation: Option<String>,
    pub numeric: Option<bool>,
    pub case_first: Option<String>,
    pub sensitivity: Option<CollatorSensitivity>,
    pub ignore_punctuation: Option<bool>,
}

/// Resolves requested locales and options into Collator internal slots.
///
/// Inputs are canonical locale identifiers produced by
/// `CanonicalizeLocaleList`.
///
/// # Errors
///
/// Returns [`InvalidLocale`] if a requested identifier cannot be resolved.
pub fn resolve_collator(
    requested: &[String],
    options: CollatorRequestOptions,
) -> Result<CollatorState, InvalidLocale> {
    let requested_locale = requested
        .iter()
        .find(|locale| locale_is_supported(locale))
        .map_or(DEFAULT_LOCALE, String::as_str);
    let components = locale_components(requested_locale)?;
    let language = components.language.as_str();
    let usage = options.usage.unwrap_or_default();

    let extension_collation = components
        .collation
        .filter(|value| supported_collation(language, value));
    let option_collation = options
        .collation
        .filter(|value| supported_collation(language, value));
    let collation = option_collation
        .as_ref()
        .or(extension_collation.as_ref())
        .cloned()
        .unwrap_or_else(|| "default".to_owned());

    let extension_numeric = components.numeric;
    let numeric = options.numeric.unwrap_or(extension_numeric);
    let extension_case_first = components
        .case_first
        .filter(|value| matches!(value.as_str(), "upper" | "lower" | "false"));
    let case_first = options
        .case_first
        .as_ref()
        .or(extension_case_first.as_ref())
        .cloned()
        .unwrap_or_else(|| default_case_first(language).to_owned());

    let retain_collation = extension_collation
        .as_ref()
        .is_some_and(|extension| extension == &collation);
    let retain_numeric = requested_has_unicode_key(requested_locale, "kn")
        && options
            .numeric
            .is_none_or(|option| option == extension_numeric);
    let retain_case_first = extension_case_first.as_ref().is_some_and(|extension| {
        options
            .case_first
            .as_ref()
            .is_none_or(|option| option == extension)
    });

    let base = components.base_name;
    let mut extensions = Vec::new();
    if retain_collation {
        extensions.push(format!("co-{collation}"));
    }
    if retain_case_first {
        extensions.push(format!("kf-{case_first}"));
    }
    if retain_numeric {
        extensions.push(if numeric {
            "kn".to_owned()
        } else {
            "kn-false".to_owned()
        });
    }
    let locale = if extensions.is_empty() {
        base
    } else {
        canonicalize_locale(&format!("{base}-u-{}", extensions.join("-")))?
    };

    Ok(CollatorState {
        locale,
        usage,
        sensitivity: options.sensitivity.unwrap_or(match usage {
            CollatorUsage::Sort => CollatorSensitivity::Variant,
            CollatorUsage::Search => CollatorSensitivity::Base,
        }),
        ignore_punctuation: options.ignore_punctuation.unwrap_or(language == "th"),
        collation,
        numeric,
        case_first,
    })
}

/// Returns the requested locales supported by the ICU4X Collator profile.
#[must_use]
pub fn collator_supported_locales(requested: &[String]) -> Vec<String> {
    requested
        .iter()
        .filter(|locale| locale_is_supported(locale))
        .cloned()
        .collect()
}

/// Compares two Unicode strings according to resolved Collator slots.
///
/// # Errors
///
/// Returns [`InvalidLocale`] if the resolved locale or collation tailoring
/// cannot be instantiated by ICU4X.
pub fn compare_with_collator(
    state: &CollatorState,
    left: &str,
    right: &str,
) -> Result<Ordering, InvalidLocale> {
    let mut locale = state.locale.parse::<Locale>().map_err(|_| InvalidLocale)?;
    locale.extensions = Extensions::default();
    if state.collation != "default" {
        locale = format!("{}-u-co-{}", locale.id, state.collation)
            .parse::<Locale>()
            .map_err(|_| InvalidLocale)?;
    }
    let mut preferences = CollatorPreferences::from(&locale);
    preferences.numeric_ordering = Some(if state.numeric {
        CollationNumericOrdering::True
    } else {
        CollationNumericOrdering::False
    });
    preferences.case_first = Some(match state.case_first.as_str() {
        "upper" => CollationCaseFirst::Upper,
        "lower" => CollationCaseFirst::Lower,
        _ => CollationCaseFirst::False,
    });
    let mut options = CollatorOptions::default();
    match state.sensitivity {
        CollatorSensitivity::Base => options.strength = Some(Strength::Primary),
        CollatorSensitivity::Accent => options.strength = Some(Strength::Secondary),
        CollatorSensitivity::Case => {
            options.strength = Some(Strength::Primary);
            options.case_level = Some(CaseLevel::On);
        }
        CollatorSensitivity::Variant => options.strength = Some(Strength::Tertiary),
    }
    options.alternate_handling = Some(if state.ignore_punctuation {
        AlternateHandling::Shifted
    } else {
        AlternateHandling::NonIgnorable
    });
    let collator = Collator::try_new(preferences, options).map_err(|_| InvalidLocale)?;

    // ICU4X's compiled default data intentionally omits search tailorings.
    // German search collation expands the three umlauts and sharp-s before
    // applying the selected comparison strength.
    if state.usage == CollatorUsage::Search && locale.id.language.as_str() == "de" {
        let left = german_search_expansion(left);
        let right = german_search_expansion(right);
        return Ok(collator.compare(&left, &right));
    }
    Ok(collator.compare(left, right))
}

fn locale_is_supported(locale: &str) -> bool {
    let Ok(components) = locale_components(locale) else {
        return false;
    };
    !matches!(components.language.as_str(), "und" | "zxx" | "tlh")
}

fn supported_collation(language: &str, value: &str) -> bool {
    matches!(value, "emoji" | "eor")
        || matches!((language, value), ("de", "phonebk") | ("zh", "pinyin"))
}

fn default_case_first(language: &str) -> &'static str {
    if matches!(language, "da" | "mt") {
        "upper"
    } else {
        "false"
    }
}

fn requested_has_unicode_key(locale: &str, key: &str) -> bool {
    let Some((_, unicode)) = locale.split_once("-u-") else {
        return false;
    };
    let unicode = unicode.split("-x-").next().unwrap_or(unicode);
    unicode.split('-').any(|subtag| subtag == key)
}

fn german_search_expansion(input: &str) -> String {
    let mut output = String::new();
    for character in input.chars() {
        output.push_str(match character {
            'ä' => "ae",
            'Ä' => "AE",
            'ö' => "oe",
            'Ö' => "OE",
            'ü' => "ue",
            'Ü' => "UE",
            'ß' => "ss",
            _ => {
                output.push(character);
                continue;
            }
        });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relevant_unicode_extensions_and_option_precedence() {
        let state = resolve_collator(
            &["de-u-co-phonebk-kf-lower-kn".to_owned()],
            CollatorRequestOptions {
                numeric: Some(true),
                case_first: Some("upper".to_owned()),
                ..Default::default()
            },
        )
        .expect("Collator");
        assert_eq!(state.locale, "de-u-co-phonebk-kn");
        assert_eq!(state.collation, "phonebk");
        assert!(state.numeric);
        assert_eq!(state.case_first, "upper");
    }

    #[test]
    fn filters_unsupported_locales_without_reordering() {
        assert_eq!(
            collator_supported_locales(&[
                "tlh".to_owned(),
                "id".to_owned(),
                "zxx".to_owned(),
                "en-u-kn".to_owned(),
            ]),
            ["id", "en-u-kn"]
        );
    }

    #[test]
    fn icu_comparison_obeys_sensitivity_numeric_and_search() {
        let state = resolve_collator(
            &["en".to_owned()],
            CollatorRequestOptions {
                sensitivity: Some(CollatorSensitivity::Base),
                numeric: Some(true),
                ..Default::default()
            },
        )
        .expect("Collator");
        assert_eq!(compare_with_collator(&state, "A", "á"), Ok(Ordering::Equal));
        assert_eq!(
            compare_with_collator(&state, "10", "2"),
            Ok(Ordering::Greater)
        );

        let search = resolve_collator(
            &["de".to_owned()],
            CollatorRequestOptions {
                usage: Some(CollatorUsage::Search),
                ..Default::default()
            },
        )
        .expect("search Collator");
        assert_eq!(
            compare_with_collator(&search, "AE", "Ä"),
            Ok(Ordering::Equal)
        );
    }
}
