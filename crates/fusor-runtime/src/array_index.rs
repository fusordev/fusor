/*
 * JavaScript array-index recognition derived from QuickJS.
 *
 * Copyright (c) 2017-2018 Fabrice Bellard
 * Copyright (c) 2017-2018 Charlie Gordon
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

use crate::{JsString, JsStringError};

/// Largest integer that is a JavaScript array index.
///
/// `u32::MAX` is the special `length` boundary and is therefore not an array
/// index.
pub const MAX_ARRAY_INDEX: u32 = u32::MAX - 1;

/// A canonical JavaScript array index in the inclusive range `0..=2^32 - 2`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArrayIndex(u32);

impl ArrayIndex {
    /// The numeric index value (the snapshot serializer's content, §8.2).
    #[must_use]
    pub(crate) const fn value(self) -> u32 {
        self.0
    }

    /// Creates an array index when `value` is within the JavaScript range.
    #[must_use]
    pub const fn new(value: u32) -> Option<Self> {
        if value <= MAX_ARRAY_INDEX {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Parses a canonical decimal property key as an array index.
    ///
    /// The accepted spelling is exactly `"0"` or a non-zero ASCII decimal
    /// sequence without leading zeroes. Values above `2^32 - 2`, non-ASCII
    /// digits, signs, whitespace, and lone UTF-16 surrogates are rejected.
    #[must_use]
    pub fn parse_property_key(key: &JsString) -> Option<Self> {
        let len = key.len();
        if len == 0 || len > 10 {
            return None;
        }

        let first = key.code_unit_at(0)?;
        if first == u16::from(b'0') {
            return (len == 1).then_some(Self(0));
        }
        if !is_ascii_digit(first) {
            return None;
        }

        let mut value = u32::from(first - u16::from(b'0'));
        for index in 1..len {
            let unit = key.code_unit_at(index)?;
            if !is_ascii_digit(unit) {
                return None;
            }
            value = value
                .checked_mul(10)?
                .checked_add(u32::from(unit - u16::from(b'0')))?;
        }
        Self::new(value)
    }

    /// Returns the integer value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Formats the index as its canonical decimal JavaScript string.
    ///
    /// # Errors
    ///
    /// Returns an error if the string backing buffer cannot be allocated.
    pub fn to_js_string(self) -> Result<JsString, JsStringError> {
        let mut digits = [0_u8; 10];
        let mut start = digits.len();
        let mut value = self.0;

        loop {
            start -= 1;
            digits[start] = decimal_digit(value);
            value /= 10;
            if value == 0 {
                break;
            }
        }

        JsString::from_latin1(&digits[start..])
    }
}

const fn is_ascii_digit(unit: u16) -> bool {
    unit >= b'0' as u16 && unit <= b'9' as u16
}

const fn decimal_digit(value: u32) -> u8 {
    b'0' + (value % 10) as u8
}

#[cfg(test)]
mod tests {
    use super::{ArrayIndex, MAX_ARRAY_INDEX};
    use crate::JsString;

    #[test]
    fn accepts_canonical_decimal_boundaries() {
        for (text, expected) in [
            ("0", 0),
            ("1", 1),
            ("2147483647", 2_147_483_647),
            ("2147483648", 2_147_483_648),
            ("4294967294", MAX_ARRAY_INDEX),
        ] {
            let key = JsString::from_utf8(text).unwrap();
            assert_eq!(
                ArrayIndex::parse_property_key(&key).map(ArrayIndex::get),
                Some(expected),
                "{text}"
            );
        }
    }

    #[test]
    fn rejects_noncanonical_and_out_of_range_keys() {
        for text in [
            "",
            "00",
            "01",
            "-0",
            "+1",
            " 1",
            "1 ",
            "1.0",
            "4294967295",
            "4294967296",
            "10000000000",
            "１２",
        ] {
            let key = JsString::from_utf8(text).unwrap();
            assert_eq!(ArrayIndex::parse_property_key(&key), None, "{text:?}");
        }
    }

    #[test]
    fn rejects_non_digit_utf16_code_units() {
        for units in [[0xD800].as_slice(), [u16::from(b'1'), 0xDFFF].as_slice()] {
            let key = JsString::from_code_units(units.iter().copied()).unwrap();
            assert_eq!(ArrayIndex::parse_property_key(&key), None);
        }
    }

    #[test]
    fn parses_concatenated_property_keys() {
        let left = JsString::from_utf8("42949").unwrap();
        let right = JsString::from_utf8("67294").unwrap();
        let key = left.concat(&right).unwrap();

        assert_eq!(
            ArrayIndex::parse_property_key(&key),
            ArrayIndex::new(MAX_ARRAY_INDEX)
        );
    }

    #[test]
    fn construction_rejects_only_u32_max() {
        assert_eq!(ArrayIndex::new(0).map(ArrayIndex::get), Some(0));
        assert_eq!(
            ArrayIndex::new(MAX_ARRAY_INDEX).map(ArrayIndex::get),
            Some(MAX_ARRAY_INDEX)
        );
        assert_eq!(ArrayIndex::new(u32::MAX), None);
    }

    #[test]
    fn formatting_round_trips_boundaries() {
        for value in [0, 1, 9, 10, 2_147_483_648, MAX_ARRAY_INDEX] {
            let index = ArrayIndex::new(value).unwrap();
            let key = index.to_js_string().unwrap();

            assert_eq!(ArrayIndex::parse_property_key(&key), Some(index));
        }
    }
}
