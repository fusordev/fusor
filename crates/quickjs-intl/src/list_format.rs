//! ECMA-402 `ListFormat` resolution and ICU4X-backed formatting.

use core::fmt;

use icu::{
    list::{
        ListFormatter, ListFormatterPreferences,
        options::{ListFormatterOptions, ListLength},
        parts,
    },
    locale::Locale,
};
use writeable::{Part, PartsWrite, Writeable};

use crate::{InvalidLocale, locale_components};

const DEFAULT_LOCALE: &str = "en-US";

/// List-format construction or formatting failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListFormatError {
    InvalidLocale,
    Data,
}

impl From<InvalidLocale> for ListFormatError {
    fn from(_: InvalidLocale) -> Self {
        Self::InvalidLocale
    }
}

/// The semantic relationship between list elements.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ListFormatType {
    #[default]
    Conjunction,
    Disjunction,
    Unit,
}

impl ListFormatType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Conjunction => "conjunction",
            Self::Disjunction => "disjunction",
            Self::Unit => "unit",
        }
    }
}

/// The width of the selected list pattern.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ListFormatStyle {
    #[default]
    Long,
    Short,
    Narrow,
}

impl ListFormatStyle {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Long => "long",
            Self::Short => "short",
            Self::Narrow => "narrow",
        }
    }

    const fn as_icu(self) -> ListLength {
        match self {
            Self::Long => ListLength::Wide,
            Self::Short => ListLength::Short,
            Self::Narrow => ListLength::Narrow,
        }
    }
}

/// Already-coerced JavaScript options passed into list-format resolution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ListFormatRequestOptions {
    pub list_type: Option<ListFormatType>,
    pub style: Option<ListFormatStyle>,
}

/// Fully resolved immutable `ListFormat` internal slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListFormatState {
    pub locale: String,
    pub list_type: ListFormatType,
    pub style: ListFormatStyle,
}

/// One ECMA-402 list-format part.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListFormatPart {
    pub kind: &'static str,
    pub value: String,
}

/// Resolves requested locales and options into `ListFormat` internal slots.
///
/// # Errors
///
/// Returns an error if the selected locale or ICU list data is unavailable.
pub fn resolve_list_format(
    requested: &[String],
    options: ListFormatRequestOptions,
) -> Result<ListFormatState, ListFormatError> {
    let requested_locale = requested
        .iter()
        .find(|locale| locale_is_supported(locale))
        .map_or(DEFAULT_LOCALE, String::as_str);
    let locale = locale_components(requested_locale)?.base_name;
    let state = ListFormatState {
        locale,
        list_type: options.list_type.unwrap_or_default(),
        style: options.style.unwrap_or_default(),
    };
    list_formatter(&state)?;
    Ok(state)
}

/// Returns the requested locales supported by the ICU4X list profile.
#[must_use]
pub fn list_format_supported_locales(requested: &[String]) -> Vec<String> {
    requested
        .iter()
        .filter(|locale| locale_is_supported(locale))
        .cloned()
        .collect()
}

/// Formats a list of strings.
///
/// # Errors
///
/// Returns an error if the resolved ICU data cannot be loaded or rendered.
pub fn format_list(state: &ListFormatState, values: &[String]) -> Result<String, ListFormatError> {
    Ok(format_list_to_parts(state, values)?
        .into_iter()
        .map(|part| part.value)
        .collect())
}

/// Formats a list into ECMA-402 element and literal parts.
///
/// # Errors
///
/// Returns an error if the resolved ICU data cannot be loaded or rendered.
pub fn format_list_to_parts(
    state: &ListFormatState,
    values: &[String],
) -> Result<Vec<ListFormatPart>, ListFormatError> {
    let formatter = list_formatter(state)?;
    let rendering = formatter.format(values.iter());
    let mut sink = ListPartSink::default();
    rendering
        .write_to_parts(&mut sink)
        .map_err(|_| ListFormatError::Data)?;
    Ok(sink.parts)
}

fn list_formatter(state: &ListFormatState) -> Result<ListFormatter, ListFormatError> {
    let locale = state
        .locale
        .parse::<Locale>()
        .map_err(|_| ListFormatError::InvalidLocale)?;
    let preferences = ListFormatterPreferences::from(&locale);
    let options = ListFormatterOptions::default().with_length(state.style.as_icu());
    match state.list_type {
        ListFormatType::Conjunction => ListFormatter::try_new_and(preferences, options),
        ListFormatType::Disjunction => ListFormatter::try_new_or(preferences, options),
        ListFormatType::Unit => ListFormatter::try_new_unit(preferences, options),
    }
    .map_err(|_| ListFormatError::Data)
}

fn locale_is_supported(locale: &str) -> bool {
    let Ok(components) = locale_components(locale) else {
        return false;
    };
    !matches!(components.language.as_str(), "und" | "zxx" | "tlh")
}

#[derive(Default)]
struct ListPartSink {
    parts: Vec<ListFormatPart>,
    active: Option<&'static str>,
}

impl fmt::Write for ListPartSink {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if value.is_empty() {
            return Ok(());
        }
        let kind = self.active.unwrap_or("literal");
        if let Some(last) = self.parts.last_mut().filter(|part| part.kind == kind) {
            last.value.push_str(value);
        } else {
            self.parts.push(ListFormatPart {
                kind,
                value: value.to_owned(),
            });
        }
        Ok(())
    }
}

impl PartsWrite for ListPartSink {
    type SubPartsWrite = Self;

    fn with_part(
        &mut self,
        part: Part,
        mut write: impl FnMut(&mut Self::SubPartsWrite) -> fmt::Result,
    ) -> fmt::Result {
        let previous = self.active;
        self.active = Some(if part == parts::ELEMENT {
            "element"
        } else {
            "literal"
        });
        let result = write(self);
        self.active = previous;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_locale_type_and_style() {
        let state = resolve_list_format(
            &["en-u-ca-gregory".to_owned()],
            ListFormatRequestOptions {
                list_type: Some(ListFormatType::Disjunction),
                style: Some(ListFormatStyle::Short),
            },
        )
        .expect("ListFormat");
        assert_eq!(state.locale, "en");
        assert_eq!(state.list_type, ListFormatType::Disjunction);
        assert_eq!(state.style, ListFormatStyle::Short);
    }

    #[test]
    fn formats_lists_and_preserves_part_boundaries() {
        let state = resolve_list_format(&["en-US".to_owned()], ListFormatRequestOptions::default())
            .expect("ListFormat");
        let values = ["Motorcycle".to_owned(), "Bus".to_owned(), "Car".to_owned()];
        assert_eq!(
            format_list(&state, &values),
            Ok("Motorcycle, Bus, and Car".to_owned())
        );
        assert_eq!(
            format_list_to_parts(&state, &values),
            Ok(vec![
                ListFormatPart {
                    kind: "element",
                    value: "Motorcycle".to_owned()
                },
                ListFormatPart {
                    kind: "literal",
                    value: ", ".to_owned()
                },
                ListFormatPart {
                    kind: "element",
                    value: "Bus".to_owned()
                },
                ListFormatPart {
                    kind: "literal",
                    value: ", and ".to_owned()
                },
                ListFormatPart {
                    kind: "element",
                    value: "Car".to_owned()
                },
            ])
        );
    }

    #[test]
    fn uses_context_sensitive_cldr_patterns() {
        let state = resolve_list_format(&["es".to_owned()], ListFormatRequestOptions::default())
            .expect("ListFormat");
        assert_eq!(
            format_list(&state, &["España".to_owned(), "Italia".to_owned()]),
            Ok("España e Italia".to_owned())
        );
    }
}
