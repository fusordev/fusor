//! Exact UTF-16 recovery for cooked strings produced by Oxc.
//!
//! Oxc keeps its public cooked-string field as UTF-8 and uses a documented
//! replacement-character marker when an ECMAScript string contains lone UTF-16
//! surrogates. This module is the only place where that private transport
//! encoding is decoded into arena-independent code units.

use std::{error::Error, fmt, sync::Arc};

const OXC_LONE_SURROGATE_MARKER: &[u8; 3] = b"\xef\xbf\xbd";

/// A malformed Oxc cooked-string marker sequence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OxcStringDecodeError {
    encoded_offset: usize,
}

impl OxcStringDecodeError {
    /// Returns the UTF-8 byte offset of the malformed marker.
    #[must_use]
    pub const fn encoded_offset(self) -> usize {
        self.encoded_offset
    }
}

impl fmt::Display for OxcStringDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "malformed Oxc lone-surrogate marker at encoded offset {}",
            self.encoded_offset
        )
    }
}

impl Error for OxcStringDecodeError {}

/// Decodes one Oxc cooked string to exact ECMAScript UTF-16 code units.
///
/// The returned immutable allocation preserves lone high and low surrogates.
/// When `lone_surrogates` is false, ordinary Unicode scalar values are encoded
/// directly as one or two UTF-16 units.
///
/// # Errors
///
/// Returns the exact encoded byte offset when Oxc's marker stream is malformed.
pub fn decode_oxc_cooked_string(
    value: &str,
    lone_surrogates: bool,
) -> Result<Arc<[u16]>, OxcStringDecodeError> {
    if !lone_surrogates {
        return Ok(value.encode_utf16().collect::<Vec<_>>().into());
    }

    let bytes = value.as_bytes();
    let mut units = Vec::with_capacity(value.len());
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset..].starts_with(OXC_LONE_SURROGATE_MARKER) {
            let Some(hex) = bytes.get(
                offset + OXC_LONE_SURROGATE_MARKER.len()
                    ..offset + OXC_LONE_SURROGATE_MARKER.len() + 4,
            ) else {
                return Err(OxcStringDecodeError {
                    encoded_offset: offset,
                });
            };
            let Some(unit) = decode_hex_code_unit(hex) else {
                return Err(OxcStringDecodeError {
                    encoded_offset: offset,
                });
            };
            if unit != 0xfffd && !(0xd800..=0xdfff).contains(&unit) {
                return Err(OxcStringDecodeError {
                    encoded_offset: offset,
                });
            }
            units.push(unit);
            offset += OXC_LONE_SURROGATE_MARKER.len() + 4;
        } else {
            let Some(character) = value[offset..].chars().next() else {
                return Err(OxcStringDecodeError {
                    encoded_offset: offset,
                });
            };
            let mut encoded = [0; 2];
            units.extend_from_slice(character.encode_utf16(&mut encoded));
            offset += character.len_utf8();
        }
    }
    Ok(units.into())
}

fn decode_hex_code_unit(hex: &[u8]) -> Option<u16> {
    let mut value = 0_u16;
    for byte in hex {
        value = value.checked_mul(16)?;
        value = value.checked_add(u16::from(match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return None,
        }))?;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::{OxcStringDecodeError, decode_oxc_cooked_string};

    #[test]
    fn decodes_surrogate_and_replacement_markers_exactly() {
        let decoded = decode_oxc_cooked_string("a\u{fffd}D800\u{fffd}dc00\u{fffd}fffd😀", true)
            .expect("well-formed Oxc markers");
        assert_eq!(
            &*decoded,
            ['a' as u16, 0xd800, 0xdc00, 0xfffd, 0xd83d, 0xde00]
        );
    }

    #[test]
    fn rejects_non_surrogate_marker_payloads() {
        let error = decode_oxc_cooked_string("\u{fffd}0041", true)
            .expect_err("marker payload must be a surrogate or replacement character");
        assert_eq!(error, OxcStringDecodeError { encoded_offset: 0 });
    }
}
