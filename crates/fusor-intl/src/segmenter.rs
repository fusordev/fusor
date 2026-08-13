//! ECMA-402 `Segmenter` resolution and ICU4X-backed boundary discovery.

use icu::{
    locale::{Locale, LocaleExpander, TransformResult},
    segmenter::{
        GraphemeClusterSegmenter, SentenceSegmenter, WordSegmenter,
        options::{SentenceBreakInvariantOptions, WordBreakOptions},
    },
};

use crate::{InvalidLocale, locale_components};

const DEFAULT_LOCALE: &str = "en-US";

/// Segmenter construction failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmenterError {
    InvalidLocale,
    Data,
}

impl From<InvalidLocale> for SegmenterError {
    fn from(_: InvalidLocale) -> Self {
        Self::InvalidLocale
    }
}

/// The boundary class selected by `Intl.Segmenter`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SegmenterGranularity {
    #[default]
    Grapheme,
    Word,
    Sentence,
}

impl SegmenterGranularity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Grapheme => "grapheme",
            Self::Word => "word",
            Self::Sentence => "sentence",
        }
    }
}

/// Already-coerced JavaScript options passed into Segmenter resolution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SegmenterRequestOptions {
    pub granularity: Option<SegmenterGranularity>,
}

/// Fully resolved immutable `Segmenter` internal slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SegmenterState {
    pub locale: String,
    pub granularity: SegmenterGranularity,
}

/// One non-empty segment in UTF-16 code-unit coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentBoundary {
    pub start: usize,
    pub end: usize,
    pub is_word_like: Option<bool>,
}

/// Resolves requested locales and options into `Segmenter` internal slots.
///
/// # Errors
///
/// Returns an error if the selected locale cannot be reduced to its base name.
pub fn resolve_segmenter(
    requested: &[String],
    options: SegmenterRequestOptions,
) -> Result<SegmenterState, SegmenterError> {
    let requested_locale = requested
        .iter()
        .find(|locale| locale_is_supported(locale))
        .map_or(DEFAULT_LOCALE, String::as_str);
    Ok(SegmenterState {
        locale: locale_components(requested_locale)?.base_name,
        granularity: options.granularity.unwrap_or_default(),
    })
}

/// Returns the requested locales supported by the ICU4X segmentation profile.
#[must_use]
pub fn segmenter_supported_locales(requested: &[String]) -> Vec<String> {
    requested
        .iter()
        .filter(|locale| locale_is_supported(locale))
        .cloned()
        .collect()
}

/// Finds every segment boundary in UTF-16 code-unit coordinates.
///
/// # Errors
///
/// Returns [`SegmenterError::Data`] when the selected ICU4X segmenter data
/// cannot be constructed.
pub fn segment_boundaries(
    state: &SegmenterState,
    input: &[u16],
) -> Result<Vec<SegmentBoundary>, SegmenterError> {
    Ok(match state.granularity {
        SegmenterGranularity::Grapheme => {
            boundaries_without_word_type(GraphemeClusterSegmenter::new().segment_utf16(input))
        }
        SegmenterGranularity::Sentence => boundaries_without_word_type(
            SentenceSegmenter::new(SentenceBreakInvariantOptions::default()).segment_utf16(input),
        ),
        SegmenterGranularity::Word => {
            let mut boundaries = Vec::new();
            let segmenter = WordSegmenter::try_new_dictionary(WordBreakOptions::default())
                .map_err(|_| SegmenterError::Data)?;
            let mut iterator = segmenter
                .as_borrowed()
                .segment_utf16(input)
                .iter_with_word_type();
            let Some((mut start, _)) = iterator.next() else {
                return Ok(boundaries);
            };
            for (end, word_type) in iterator {
                if start != end {
                    boundaries.push(SegmentBoundary {
                        start,
                        end,
                        is_word_like: Some(word_type.is_word_like()),
                    });
                }
                start = end;
            }
            boundaries
        }
    })
}

fn boundaries_without_word_type(iterator: impl Iterator<Item = usize>) -> Vec<SegmentBoundary> {
    let mut boundaries = Vec::new();
    let mut iterator = iterator;
    let Some(mut start) = iterator.next() else {
        return boundaries;
    };
    for end in iterator {
        if start != end {
            boundaries.push(SegmentBoundary {
                start,
                end,
                is_word_like: None,
            });
        }
        start = end;
    }
    boundaries
}

fn locale_is_supported(locale: &str) -> bool {
    let Ok(components) = locale_components(locale) else {
        return false;
    };
    let Ok(mut language) = components.language.parse::<Locale>() else {
        return false;
    };
    LocaleExpander::new_extended().maximize(&mut language.id) == TransformResult::Modified
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_locale_and_granularity() {
        assert_eq!(
            resolve_segmenter(
                &["fr-u-ca-gregory".to_owned()],
                SegmenterRequestOptions {
                    granularity: Some(SegmenterGranularity::Word),
                },
            ),
            Ok(SegmenterState {
                locale: "fr".to_owned(),
                granularity: SegmenterGranularity::Word,
            })
        );
    }

    #[test]
    fn filters_languages_missing_from_the_icu_locale_profile() {
        assert_eq!(
            segmenter_supported_locales(&["xyz".to_owned(), "ar".to_owned()]),
            ["ar"]
        );
    }

    #[test]
    fn segments_utf16_without_losing_surrogates() {
        let grapheme =
            resolve_segmenter(&[], SegmenterRequestOptions::default()).expect("grapheme Segmenter");
        let input = [u16::from(b'a'), 0xd800, u16::from(b' '), 0xdc00];
        assert_eq!(
            segment_boundaries(&grapheme, &input),
            Ok(vec![
                SegmentBoundary {
                    start: 0,
                    end: 1,
                    is_word_like: None,
                },
                SegmentBoundary {
                    start: 1,
                    end: 2,
                    is_word_like: None,
                },
                SegmentBoundary {
                    start: 2,
                    end: 3,
                    is_word_like: None,
                },
                SegmentBoundary {
                    start: 3,
                    end: 4,
                    is_word_like: None,
                },
            ])
        );
    }

    #[test]
    fn word_segments_expose_word_like_classification() {
        let word = resolve_segmenter(
            &["en".to_owned()],
            SegmenterRequestOptions {
                granularity: Some(SegmenterGranularity::Word),
            },
        )
        .expect("word Segmenter");
        assert_eq!(
            segment_boundaries(&word, &"hello 42!".encode_utf16().collect::<Vec<_>>()),
            Ok(vec![
                SegmentBoundary {
                    start: 0,
                    end: 5,
                    is_word_like: Some(true),
                },
                SegmentBoundary {
                    start: 5,
                    end: 6,
                    is_word_like: Some(false),
                },
                SegmentBoundary {
                    start: 6,
                    end: 8,
                    is_word_like: Some(true),
                },
                SegmentBoundary {
                    start: 8,
                    end: 9,
                    is_word_like: Some(false),
                },
            ])
        );
    }
}
