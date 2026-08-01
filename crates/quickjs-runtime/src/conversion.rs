/*
 * JavaScript numeric conversions derived from QuickJS.
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

use std::{cmp::Ordering, iter::Peekable};

use crate::{
    number::JsNumber,
    string::{CodeUnits, JsString, JsStringError},
};

const BINARY64_FRACTION_BITS: u64 = 52;
const BINARY64_EXPONENT_BIAS: u64 = 1023;
const BINARY64_EXPONENT_MASK: u64 = 0x7ff;
const BINARY64_FRACTION_MASK: u64 = (1_u64 << BINARY64_FRACTION_BITS) - 1;
const RADIX_SIGNIFICAND_BITS: u8 = 53;
const RADIX_GUARD_BITS: u8 = RADIX_SIGNIFICAND_BITS + 1;
const INLINE_DECIMAL_BYTES: usize = 64;

/// Applies the pinned `QuickJS` `String` branch of `ToNumber`.
///
/// Parsing operates on UTF-16 code units so lone surrogates remain observable
/// as invalid input instead of being replaced during a lossy UTF-8 conversion.
/// Decimal conversion uses a fallibly grown ASCII buffer; power-of-two radix
/// integers are rounded directly to binary64 without an intermediate integer
/// allocation.
pub(crate) fn string_to_number(value: &JsString) -> Result<JsNumber, JsStringError> {
    let mut units = value.code_units().peekable();
    consume_spaces(&mut units);
    if units.peek().is_none() {
        return Ok(JsNumber::from_i32(0));
    }

    let sign = match units.peek().copied() {
        Some(unit) if unit == u16::from(b'+') => {
            units.next();
            Some(b'+')
        }
        Some(unit) if unit == u16::from(b'-') => {
            units.next();
            Some(b'-')
        }
        _ => None,
    };

    if units.peek().copied() == Some(u16::from(b'I')) {
        return Ok(parse_infinity(&mut units, sign));
    }

    let mut decimal = DecimalBytes::new();
    if let Some(sign) = sign {
        decimal.push(sign)?;
    }

    let mut has_mantissa_digit = false;
    if sign.is_none() && units.peek().copied() == Some(u16::from(b'0')) {
        units.next();
        if let Some((radix, bits_per_digit)) = radix_prefix(units.peek().copied()) {
            units.next();
            return Ok(parse_radix_integer(&mut units, radix, bits_per_digit));
        }
        decimal.push(b'0')?;
        has_mantissa_digit = true;
    }

    while let Some(unit) = units.peek().copied() {
        if !is_ascii_digit(unit) {
            break;
        }
        units.next();
        decimal.push(ascii_byte(unit))?;
        has_mantissa_digit = true;
    }

    if units.peek().copied() == Some(u16::from(b'.')) {
        units.next();
        if !has_mantissa_digit && !units.peek().copied().is_some_and(is_ascii_digit) {
            return Ok(nan());
        }
        decimal.push(b'.')?;
        while let Some(unit) = units.peek().copied() {
            if !is_ascii_digit(unit) {
                break;
            }
            units.next();
            decimal.push(ascii_byte(unit))?;
            has_mantissa_digit = true;
        }
    }

    if !has_mantissa_digit {
        return Ok(nan());
    }

    if matches!(
        units.peek().copied(),
        Some(unit) if unit == u16::from(b'e') || unit == u16::from(b'E')
    ) {
        let exponent_marker = units.next().expect("the exponent marker was peeked");
        decimal.push(ascii_byte(exponent_marker))?;
        if matches!(
            units.peek().copied(),
            Some(unit) if unit == u16::from(b'+') || unit == u16::from(b'-')
        ) {
            let exponent_sign = units.next().expect("the exponent sign was peeked");
            decimal.push(ascii_byte(exponent_sign))?;
        }
        if !units.peek().copied().is_some_and(is_ascii_digit) {
            return Ok(nan());
        }
        while let Some(unit) = units.peek().copied() {
            if !is_ascii_digit(unit) {
                break;
            }
            units.next();
            decimal.push(ascii_byte(unit))?;
        }
    }

    consume_spaces(&mut units);
    if units.peek().is_some() {
        return Ok(nan());
    }

    let Ok(source) = std::str::from_utf8(decimal.as_slice()) else {
        return Ok(nan());
    };
    Ok(JsNumber::from_f64(
        source.parse::<f64>().unwrap_or(f64::NAN),
    ))
}

/// Applies `ToUint32` to an already-converted ECMAScript Number.
///
/// The bit-level implementation mirrors `QuickJS`'s avoidance of `fmod`: binary64
/// values with exponent 84 or greater are already multiples of `2^32`, while
/// smaller finite values can be truncated by shifting their significand.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    reason = "discarding high significand bits is the required modulo-2^32 operation"
)]
pub(crate) fn number_to_uint32(value: JsNumber) -> u32 {
    let bits = value.as_f64().to_bits();
    let encoded_exponent = (bits >> BINARY64_FRACTION_BITS) & BINARY64_EXPONENT_MASK;

    if encoded_exponent == 0
        || encoded_exponent == BINARY64_EXPONENT_MASK
        || encoded_exponent < BINARY64_EXPONENT_BIAS
    {
        return 0;
    }

    let exponent = encoded_exponent - BINARY64_EXPONENT_BIAS;
    if exponent > 83 {
        return 0;
    }

    let significand = (bits & BINARY64_FRACTION_MASK) | (1_u64 << BINARY64_FRACTION_BITS);
    let magnitude = if exponent <= BINARY64_FRACTION_BITS {
        (significand >> (BINARY64_FRACTION_BITS - exponent)) as u32
    } else {
        (significand << (exponent - BINARY64_FRACTION_BITS)) as u32
    };

    if bits >> 63 == 0 {
        magnitude
    } else {
        0_u32.wrapping_sub(magnitude)
    }
}

/// Applies `ToInt32` to an already-converted ECMAScript Number.
#[must_use]
pub(crate) fn number_to_int32(value: JsNumber) -> i32 {
    i32::from_ne_bytes(number_to_uint32(value).to_ne_bytes())
}

/// The largest integer binary64 represents exactly, which bounds `ToIndex` and
/// `ToLength` (`MAX_SAFE_INTEGER`, `quickjs.c:13485`).
pub(crate) const MAX_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;

/// Applies ECMAScript `ToIntegerOrInfinity` to an already-converted Number.
///
/// `NaN` becomes `+0`, and every other finite value truncates toward zero.
/// Infinities are preserved so a caller can distinguish an unbounded length
/// from a saturated one.
#[must_use]
pub(crate) fn number_to_integer_or_infinity(value: JsNumber) -> f64 {
    let value = value.as_f64();
    if value.is_nan() {
        return 0.0;
    }
    if value.is_infinite() {
        return value;
    }
    value.trunc()
}

/// Applies ECMAScript `ToLength` to an already-converted Number.
///
/// The result is clamped into `0..=MAX_SAFE_INTEGER`, matching
/// `JS_ToLengthFree` (`quickjs.c:13509`). This is deliberately distinct from
/// the `ToUint32` length read that `js_get_length32` performs
/// (`quickjs.c:41008`), which the array-iterator path keeps using.
#[must_use]
pub(crate) fn number_to_length(value: JsNumber) -> u64 {
    let integer = number_to_integer_or_infinity(value);
    if integer <= 0.0 {
        return 0;
    }
    if integer >= max_safe_integer_as_f64() {
        return MAX_SAFE_INTEGER;
    }
    // The value is a positive, finite, truncated double below 2^53, so it is
    // exactly representable as a u64.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the preceding bounds prove the truncated value fits the u64 domain exactly"
    )]
    let clamped = integer as u64;
    clamped
}

/// Applies ECMAScript `ToIndex` to an already-converted Number.
///
/// Returns `None` when the truncated value falls outside
/// `0..=MAX_SAFE_INTEGER`, which the caller reports as
/// `RangeError: invalid array index` (`quickjs.c:13498`). `NaN` and `undefined`
/// convert to `0` rather than failing, which the pinned oracle confirms:
/// `BigInt.asUintN(NaN, 5n)` is `0n`.
#[must_use]
#[allow(
    dead_code,
    reason = "ToIndex is required by BigInt.asIntN/asUintN, whose BigInt domain is a separate milestone"
)]
pub(crate) fn number_to_index(value: JsNumber) -> Option<u64> {
    let integer = number_to_integer_or_infinity(value);
    if integer < 0.0 || integer > max_safe_integer_as_f64() {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the preceding bounds prove the truncated value fits the u64 domain exactly"
    )]
    let index = integer as u64;
    Some(index)
}

/// Returns `MAX_SAFE_INTEGER` as an exact binary64 value.
#[expect(
    clippy::cast_precision_loss,
    reason = "2^53 - 1 is exactly representable in binary64"
)]
fn max_safe_integer_as_f64() -> f64 {
    MAX_SAFE_INTEGER as f64
}

fn parse_infinity(units: &mut Peekable<CodeUnits<'_>>, sign: Option<u8>) -> JsNumber {
    for expected in b"Infinity" {
        if units.next() != Some(u16::from(*expected)) {
            return nan();
        }
    }
    consume_spaces(units);
    if units.peek().is_some() {
        return nan();
    }
    if sign == Some(b'-') {
        JsNumber::from_f64(f64::NEG_INFINITY)
    } else {
        JsNumber::from_f64(f64::INFINITY)
    }
}

fn parse_radix_integer(
    units: &mut Peekable<CodeUnits<'_>>,
    radix: u8,
    bits_per_digit: u8,
) -> JsNumber {
    let mut accumulator = RadixAccumulator::new();
    let mut has_digit = false;

    while let Some(unit) = units.peek().copied() {
        let Some(digit) = ascii_digit_value(unit) else {
            break;
        };
        if digit >= radix {
            break;
        }
        units.next();
        accumulator.push_digit(digit, bits_per_digit);
        has_digit = true;
    }

    if !has_digit {
        return nan();
    }
    consume_spaces(units);
    if units.peek().is_some() {
        return nan();
    }
    JsNumber::from_f64(accumulator.finish())
}

fn radix_prefix(unit: Option<u16>) -> Option<(u8, u8)> {
    match unit {
        Some(unit) if unit == u16::from(b'x') || unit == u16::from(b'X') => Some((16, 4)),
        Some(unit) if unit == u16::from(b'o') || unit == u16::from(b'O') => Some((8, 3)),
        Some(unit) if unit == u16::from(b'b') || unit == u16::from(b'B') => Some((2, 1)),
        _ => None,
    }
}

fn consume_spaces(units: &mut Peekable<CodeUnits<'_>>) {
    while units.peek().copied().is_some_and(is_quickjs_space) {
        units.next();
    }
}

fn is_quickjs_space(unit: u16) -> bool {
    matches!(
        unit,
        0x0009..=0x000d
            | 0x0020
            | 0x00a0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028..=0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}

fn is_ascii_digit(unit: u16) -> bool {
    (u16::from(b'0')..=u16::from(b'9')).contains(&unit)
}

fn ascii_digit_value(unit: u16) -> Option<u8> {
    match unit {
        unit if (u16::from(b'0')..=u16::from(b'9')).contains(&unit) => {
            Some(ascii_byte(unit) - b'0')
        }
        unit if (u16::from(b'a')..=u16::from(b'f')).contains(&unit) => {
            Some(ascii_byte(unit) - b'a' + 10)
        }
        unit if (u16::from(b'A')..=u16::from(b'F')).contains(&unit) => {
            Some(ascii_byte(unit) - b'A' + 10)
        }
        _ => None,
    }
}

fn ascii_byte(unit: u16) -> u8 {
    u8::try_from(unit).expect("the caller established that the UTF-16 code unit is ASCII")
}

fn nan() -> JsNumber {
    JsNumber::from_f64(f64::NAN)
}

struct DecimalBytes {
    inline: [u8; INLINE_DECIMAL_BYTES],
    inline_len: usize,
    heap: Option<Vec<u8>>,
}

impl DecimalBytes {
    fn new() -> Self {
        Self {
            inline: [0; INLINE_DECIMAL_BYTES],
            inline_len: 0,
            heap: None,
        }
    }

    fn push(&mut self, byte: u8) -> Result<(), JsStringError> {
        if let Some(heap) = self.heap.as_mut() {
            if heap.len() == heap.capacity() {
                heap.try_reserve(1)
                    .map_err(|_| JsStringError::AllocationFailed { additional: 1 })?;
            }
            heap.push(byte);
            return Ok(());
        }

        if self.inline_len < INLINE_DECIMAL_BYTES {
            self.inline[self.inline_len] = byte;
            self.inline_len += 1;
            return Ok(());
        }

        let additional = self.inline_len + 1;
        let mut heap = Vec::new();
        heap.try_reserve_exact(additional)
            .map_err(|_| JsStringError::AllocationFailed { additional })?;
        heap.extend_from_slice(&self.inline);
        heap.push(byte);
        self.heap = Some(heap);
        Ok(())
    }

    fn as_slice(&self) -> &[u8] {
        self.heap
            .as_deref()
            .unwrap_or(&self.inline[..self.inline_len])
    }
}

struct RadixAccumulator {
    significant_bits: u64,
    leading_significand: u64,
    captured_bits: u8,
    guard: bool,
    sticky: bool,
}

impl RadixAccumulator {
    const fn new() -> Self {
        Self {
            significant_bits: 0,
            leading_significand: 0,
            captured_bits: 0,
            guard: false,
            sticky: false,
        }
    }

    fn push_digit(&mut self, digit: u8, bits_per_digit: u8) {
        let width = if self.significant_bits == 0 {
            if digit == 0 {
                return;
            }
            significant_width(digit)
        } else {
            bits_per_digit
        };
        self.significant_bits += u64::from(width);

        if self.captured_bits == RADIX_GUARD_BITS {
            self.sticky |= digit != 0;
            return;
        }

        for shift in (0..width).rev() {
            let bit = (digit >> shift) & 1;
            match self.captured_bits.cmp(&RADIX_SIGNIFICAND_BITS) {
                Ordering::Less => {
                    self.leading_significand = (self.leading_significand << 1) | u64::from(bit);
                    self.captured_bits += 1;
                }
                Ordering::Equal => {
                    self.guard = bit != 0;
                    self.captured_bits += 1;
                }
                Ordering::Greater => self.sticky |= bit != 0,
            }
        }
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "values with at most 53 significant bits convert exactly to binary64"
    )]
    fn finish(self) -> f64 {
        if self.significant_bits == 0 {
            return 0.0;
        }
        if self.significant_bits <= u64::from(RADIX_SIGNIFICAND_BITS) {
            return self.leading_significand as f64;
        }

        let mut exponent = self.significant_bits - 1;
        let mut significand = self.leading_significand;
        if self.guard && (self.sticky || significand & 1 != 0) {
            significand += 1;
            if significand == 1_u64 << RADIX_SIGNIFICAND_BITS {
                significand >>= 1;
                exponent += 1;
            }
        }
        if exponent > 1023 {
            return f64::INFINITY;
        }

        let exponent_bits = (exponent + BINARY64_EXPONENT_BIAS) << BINARY64_FRACTION_BITS;
        f64::from_bits(exponent_bits | (significand & BINARY64_FRACTION_MASK))
    }
}

fn significant_width(digit: u8) -> u8 {
    match digit {
        0 => 0,
        1 => 1,
        2..=3 => 2,
        4..=7 => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_SAFE_INTEGER, max_safe_integer_as_f64, number_to_index, number_to_int32,
        number_to_integer_or_infinity, number_to_length, number_to_uint32, string_to_number,
    };
    use crate::{JsNumber, JsString};

    fn parse(input: &str) -> f64 {
        string_to_number(&JsString::from_utf8(input).expect("test string"))
            .expect("temporary decimal storage")
            .as_f64()
    }

    fn parse_units(units: impl IntoIterator<Item = u16>) -> f64 {
        string_to_number(&JsString::from_code_units(units).expect("test string"))
            .expect("temporary decimal storage")
            .as_f64()
    }

    #[test]
    fn quickjs_string_to_number_trims_exact_whitespace_set() {
        let quickjs_spaces = [
            0x0009, 0x000a, 0x000b, 0x000c, 0x000d, 0x0020, 0x00a0, 0x1680, 0x2000, 0x2001, 0x2002,
            0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a, 0x2028, 0x2029, 0x202f,
            0x205f, 0x3000, 0xfeff,
        ];
        for space in quickjs_spaces {
            assert_eq!(
                parse_units([space, u16::from(b'4'), u16::from(b'2'), space]).to_bits(),
                42.0_f64.to_bits()
            );
        }
        assert_eq!(parse_units(quickjs_spaces).to_bits(), 0.0_f64.to_bits());

        for non_space in [0x0085, 0x180e, 0x200b, 0xd800] {
            assert!(parse_units([non_space]).is_nan());
        }
    }

    #[test]
    fn quickjs_string_to_number_accepts_decimal_infinity_and_radix_grammar() {
        for (source, expected_bits) in [
            ("", 0_u64),
            (" \t\n", 0),
            ("+0", 0),
            ("-0", 0x8000_0000_0000_0000),
            (".1", 0x3fb9_9999_9999_999a),
            ("1.", 0x3ff0_0000_0000_0000),
            ("1.e2", 0x4059_0000_0000_0000),
            ("1e-2", 0x3f84_7ae1_47ae_147b),
            ("Infinity", 0x7ff0_0000_0000_0000),
            ("+Infinity", 0x7ff0_0000_0000_0000),
            ("-Infinity", 0xfff0_0000_0000_0000),
            ("0x10", 0x4030_0000_0000_0000),
            ("0Xf", 0x402e_0000_0000_0000),
            ("0b11", 0x4008_0000_0000_0000),
            ("0o10", 0x4020_0000_0000_0000),
        ] {
            assert_eq!(parse(source).to_bits(), expected_bits, "{source:?}");
        }
    }

    #[test]
    fn quickjs_string_to_number_rejects_partial_and_signed_radix_grammar() {
        for source in [
            "+",
            "-",
            ".",
            "1e",
            "1e+",
            "1e-",
            "+0x1",
            "-0x1",
            "0x",
            "0xg",
            "0b2",
            "0o8",
            "1_0",
            "12junk",
            "Infinityx",
            "infinity",
        ] {
            assert!(parse(source).is_nan(), "{source:?}");
        }
    }

    #[test]
    fn quickjs_decimal_rounding_and_heap_buffer_boundaries_match_oracle() {
        for (source, expected_bits) in [
            ("1.7976931348623157e308", 0x7fef_ffff_ffff_ffff),
            ("1.7976931348623159e308", 0x7ff0_0000_0000_0000),
            ("2.4703282292062327e-324", 0),
            ("2.4703282292062328e-324", 1),
        ] {
            assert_eq!(parse(source).to_bits(), expected_bits, "{source}");
        }

        let long_decimal = format!("1{}", "0".repeat(99));
        assert_eq!(parse(&long_decimal).to_bits(), 0x547d_42ae_a287_9f2e);
    }

    #[test]
    fn quickjs_radix_integer_rounding_uses_ties_to_even() {
        for (source, expected_bits) in [
            ("0x1fffffffffffff", 0x433f_ffff_ffff_ffff),
            ("0x20000000000001", 0x4340_0000_0000_0000),
            ("0x20000000000003", 0x4340_0000_0000_0002),
            ("0x1fffffffffffff8", 0x4380_0000_0000_0000),
            (
                "0b100000000000000000000000000000000000000000000000000001",
                0x4340_0000_0000_0000,
            ),
            ("0o400000000000000001", 0x4340_0000_0000_0000),
        ] {
            assert_eq!(parse(source).to_bits(), expected_bits, "{source}");
        }

        let overflowing_hex_integer = format!("0x1{}", "0".repeat(256));
        assert_eq!(
            parse(&overflowing_hex_integer).to_bits(),
            f64::INFINITY.to_bits()
        );
    }

    #[test]
    fn quickjs_number_to_uint32_and_int32_match_oracle_boundaries() {
        for (source, expected_unsigned, expected_signed) in [
            (0.0, 0, 0),
            (-0.0, 0, 0),
            (f64::NAN, 0, 0),
            (f64::INFINITY, 0, 0),
            (f64::NEG_INFINITY, 0, 0),
            (1.9, 1, 1),
            (-1.9, u32::MAX, -1),
            (2_147_483_648.0, 0x8000_0000, i32::MIN),
            (2_147_483_649.0, 0x8000_0001, -2_147_483_647),
            (4_294_967_295.0, u32::MAX, -1),
            (4_294_967_296.0, 0, 0),
            (4_294_967_297.0, 1, 1),
            (-4_294_967_297.0, u32::MAX, -1),
            (9_007_199_254_740_992.0, 0, 0),
            (9_007_199_254_740_994.0, 2, 2),
        ] {
            let number = JsNumber::from_f64(source);
            assert_eq!(number_to_uint32(number), expected_unsigned, "{source}");
            assert_eq!(number_to_int32(number), expected_signed, "{source}");
        }

        let exponent_83_with_low_bit = f64::from_bits((1106_u64 << 52) | 1);
        assert_eq!(
            number_to_uint32(JsNumber::from_f64(exponent_83_with_low_bit)),
            0x8000_0000
        );
        assert_eq!(
            number_to_int32(JsNumber::from_f64(exponent_83_with_low_bit)),
            i32::MIN
        );
    }

    /// `ToIntegerOrInfinity` truncates toward zero, maps `NaN` to `+0`, and
    /// preserves infinities.
    #[test]
    fn to_integer_or_infinity_truncates_toward_zero() {
        for (input, expected) in [
            (f64::NAN, 0.0),
            (0.0, 0.0),
            (-0.0, 0.0),
            (1.9, 1.0),
            (-1.9, -1.0),
            (-0.5, -0.0),
            (f64::INFINITY, f64::INFINITY),
            (f64::NEG_INFINITY, f64::NEG_INFINITY),
        ] {
            let actual = number_to_integer_or_infinity(JsNumber::from_f64(input));
            // Compare bit patterns so the assertion is exact, normalizing both
            // signed zeros to `+0`. Truncation preserves the sign of a zero
            // result (`Math.trunc(-0.5)` is `-0` in the pinned oracle), and
            // every consumer here treats the two zeros alike.
            let normalize = |value: f64| if value == 0.0 { 0.0 } else { value };
            assert_eq!(
                normalize(actual).to_bits(),
                normalize(expected).to_bits(),
                "ToIntegerOrInfinity({input}) produced {actual}, expected {expected}"
            );
        }
    }

    /// `ToLength` clamps into `0..=MAX_SAFE_INTEGER`.
    ///
    /// The pinned oracle exposes this through `Function.prototype.apply`:
    /// `count.apply(null, {length: -5})` sees `0` arguments,
    /// `{length: 1.9}` sees `1`, and `{length: NaN}` sees `0`.
    #[test]
    fn to_length_clamps_into_the_safe_integer_range() {
        for (input, expected) in [
            (f64::NAN, 0),
            (-5.0, 0),
            (-0.0, 0),
            (0.0, 0),
            (1.9, 1),
            (3.0, 3),
            (f64::INFINITY, MAX_SAFE_INTEGER),
            (f64::NEG_INFINITY, 0),
        ] {
            assert_eq!(
                number_to_length(JsNumber::from_f64(input)),
                expected,
                "ToLength({input})"
            );
        }
        // The boundary itself is preserved rather than saturated away.
        let boundary = max_safe_integer_as_f64();
        assert_eq!(
            number_to_length(JsNumber::from_f64(boundary)),
            MAX_SAFE_INTEGER
        );
    }

    /// `ToIndex` rejects a negative or too-large value and maps `NaN` to zero.
    ///
    /// The pinned oracle exposes this through `BigInt.asUintN`:
    /// `BigInt.asUintN(NaN, 5n)` is `0n`, `BigInt.asUintN(1.5, 5n)` is `1n`,
    /// and both `-1` and `2**53` raise `RangeError: invalid array index`.
    #[test]
    fn to_index_rejects_values_outside_the_safe_integer_range() {
        assert_eq!(number_to_index(JsNumber::from_f64(f64::NAN)), Some(0));
        assert_eq!(number_to_index(JsNumber::from_f64(1.5)), Some(1));
        assert_eq!(number_to_index(JsNumber::from_f64(0.0)), Some(0));
        assert_eq!(number_to_index(JsNumber::from_f64(-0.0)), Some(0));
        assert_eq!(number_to_index(JsNumber::from_f64(8.0)), Some(8));
        assert_eq!(
            number_to_index(JsNumber::from_f64(max_safe_integer_as_f64())),
            Some(MAX_SAFE_INTEGER)
        );

        assert_eq!(number_to_index(JsNumber::from_f64(-1.0)), None);
        assert_eq!(number_to_index(JsNumber::from_f64(-1.5)), None);
        assert_eq!(
            number_to_index(JsNumber::from_f64(max_safe_integer_as_f64() + 1.0)),
            None
        );
        assert_eq!(number_to_index(JsNumber::from_f64(f64::INFINITY)), None);
        assert_eq!(number_to_index(JsNumber::from_f64(f64::NEG_INFINITY)), None);
    }
}
