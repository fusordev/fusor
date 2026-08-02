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

/// Applies the string-prefix grammar used by the global `parseFloat` function.
///
/// Unlike [`string_to_number`], trailing code units are ignored and an
/// incomplete exponent is excluded from the longest valid prefix. Parsing
/// stays on UTF-16 code units so a lone surrogate terminates the prefix rather
/// than being replaced at a host-string boundary.
pub(crate) fn string_to_parse_float(value: &JsString) -> Result<JsNumber, JsStringError> {
    let mut units = value.code_units().peekable();
    consume_spaces(&mut units);

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
        for expected in b"Infinity" {
            if units.next() != Some(u16::from(*expected)) {
                return Ok(nan());
            }
        }
        return Ok(if sign == Some(b'-') {
            JsNumber::from_f64(f64::NEG_INFINITY)
        } else {
            JsNumber::from_f64(f64::INFINITY)
        });
    }

    let mut decimal = DecimalBytes::new();
    if let Some(sign) = sign {
        decimal.push(sign)?;
    }

    let mut has_mantissa_digit = false;
    while let Some(unit) = units.peek().copied().filter(|unit| is_ascii_digit(*unit)) {
        units.next();
        decimal.push(ascii_byte(unit))?;
        has_mantissa_digit = true;
    }

    if units.peek().copied() == Some(u16::from(b'.')) {
        units.next();
        decimal.push(b'.')?;
        while let Some(unit) = units.peek().copied().filter(|unit| is_ascii_digit(*unit)) {
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
        let marker = units.next().expect("the exponent marker was peeked");
        let exponent_sign = if matches!(
            units.peek().copied(),
            Some(unit) if unit == u16::from(b'+') || unit == u16::from(b'-')
        ) {
            units.next()
        } else {
            None
        };
        if units.peek().copied().is_some_and(is_ascii_digit) {
            decimal.push(ascii_byte(marker))?;
            if let Some(exponent_sign) = exponent_sign {
                decimal.push(ascii_byte(exponent_sign))?;
            }
            while let Some(unit) = units.peek().copied().filter(|unit| is_ascii_digit(*unit)) {
                units.next();
                decimal.push(ascii_byte(unit))?;
            }
        }
    }

    let Ok(source) = std::str::from_utf8(decimal.as_slice()) else {
        return Ok(nan());
    };
    Ok(JsNumber::from_f64(
        source.parse::<f64>().unwrap_or(f64::NAN),
    ))
}

/// Applies the string-prefix grammar used by the global `parseInt` function.
///
/// `radix` is the result of the specification's prior `ToInt32` conversion.
/// Power-of-two radices use the exact guard-and-sticky accumulator shared with
/// `ToNumber`; decimal input delegates correctly rounded conversion to Rust's
/// binary64 parser; the remaining radices use the implementation-approximated
/// result explicitly permitted by ECMA-262.
pub(crate) fn string_to_parse_int(
    value: &JsString,
    mut radix: i32,
) -> Result<JsNumber, JsStringError> {
    if radix != 0 && !(2..=36).contains(&radix) {
        return Ok(nan());
    }

    let mut units = value.code_units().peekable();
    consume_spaces(&mut units);
    let negative = match units.peek().copied() {
        Some(unit) if unit == u16::from(b'-') => {
            units.next();
            true
        }
        Some(unit) if unit == u16::from(b'+') => {
            units.next();
            false
        }
        _ => false,
    };

    let mut first = units.next();
    if matches!(radix, 0 | 16)
        && first == Some(u16::from(b'0'))
        && matches!(units.peek().copied(), Some(unit) if unit == u16::from(b'x') || unit == u16::from(b'X'))
    {
        units.next();
        first = None;
        radix = 16;
    }
    if radix == 0 {
        radix = 10;
    }
    let radix = u8::try_from(radix).expect("the radix was restricted to 2 through 36");

    let mut digits = DecimalBytes::new();
    let mut has_digit = false;
    let mut has_significant_digit = false;
    let significant_limit = if radix == 10 { 400 } else { 1_024 };
    for unit in first.into_iter().chain(units) {
        let Some(digit) = ascii_digit_value(unit) else {
            break;
        };
        if digit >= radix {
            break;
        }
        has_digit = true;
        if digit != 0 || has_significant_digit {
            has_significant_digit = true;
            if digits.as_slice().len() == significant_limit {
                return Ok(signed_parse_int_result(f64::INFINITY, negative));
            }
            digits.push(ascii_byte(unit))?;
        }
    }

    if !has_digit {
        return Ok(nan());
    }
    if !has_significant_digit {
        return Ok(signed_parse_int_result(0.0, negative));
    }

    let magnitude = match radix {
        2 | 4 | 8 | 16 | 32 => {
            let bits_per_digit = match radix {
                2 => 1,
                4 => 2,
                8 => 3,
                16 => 4,
                32 => 5,
                _ => unreachable!("the power-of-two radix match is exhaustive"),
            };
            let mut accumulator = RadixAccumulator::new();
            for byte in digits.as_slice() {
                let digit = ascii_digit_value(u16::from(*byte))
                    .expect("the digit buffer contains only validated ASCII digits");
                accumulator.push_digit(digit, bits_per_digit);
            }
            accumulator.finish()
        }
        10 => {
            let Ok(source) = std::str::from_utf8(digits.as_slice()) else {
                return Ok(nan());
            };
            source.parse::<f64>().unwrap_or(f64::INFINITY)
        }
        _ => digits.as_slice().iter().fold(0.0_f64, |accumulator, byte| {
            let digit = ascii_digit_value(u16::from(*byte))
                .expect("the digit buffer contains only validated ASCII digits");
            accumulator.mul_add(f64::from(radix), f64::from(digit))
        }),
    };
    Ok(signed_parse_int_result(magnitude, negative))
}

fn signed_parse_int_result(magnitude: f64, negative: bool) -> JsNumber {
    JsNumber::from_f64(if negative { -magnitude } else { magnitude })
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

/// Applies `ToUint16` to an already-converted ECMAScript Number.
///
/// The modular narrow conversions are all defined as `ToUint32` followed by a
/// truncation, which upstream performs by assigning the `int32_t` result into a
/// narrower integer (`quickjs.c:9985` and the typed-array element writes).
/// Sharing `number_to_uint32` therefore keeps the `NaN`, infinity, and
/// multiple-of-2^32 cases consistent across every width rather than restating
/// them per type.
#[must_use]
pub(crate) fn number_to_uint16(value: JsNumber) -> u16 {
    // Masking before the conversion makes it infallible, so the modular result
    // needs no lossy cast.
    let truncated = number_to_uint32(value) & u32::from(u16::MAX);
    u16::try_from(truncated).expect("the mask leaves at most 16 bits")
}

/// Applies `ToInt16` to an already-converted ECMAScript Number.
#[must_use]
#[allow(
    dead_code,
    reason = "the narrow conversions are verified against the oracle before the typed-array surface that consumes them exists"
)]
pub(crate) fn number_to_int16(value: JsNumber) -> i16 {
    i16::from_ne_bytes(number_to_uint16(value).to_ne_bytes())
}

/// Applies `ToUint8` to an already-converted ECMAScript Number.
#[must_use]
#[allow(
    dead_code,
    reason = "the narrow conversions are verified against the oracle before the typed-array surface that consumes them exists"
)]
pub(crate) fn number_to_uint8(value: JsNumber) -> u8 {
    let truncated = number_to_uint32(value) & u32::from(u8::MAX);
    u8::try_from(truncated).expect("the mask leaves at most 8 bits")
}

/// Applies `ToInt8` to an already-converted ECMAScript Number.
#[must_use]
#[allow(
    dead_code,
    reason = "the narrow conversions are verified against the oracle before the typed-array surface that consumes them exists"
)]
pub(crate) fn number_to_int8(value: JsNumber) -> i8 {
    i8::from_ne_bytes(number_to_uint8(value).to_ne_bytes())
}

/// Applies `ToUint8Clamp` to an already-converted ECMAScript Number.
///
/// This is the one narrow conversion that is not modular: `NaN` becomes `0`,
/// out-of-range values saturate, and an in-range value is rounded half-to-even
/// because upstream uses `lrint` under the default rounding mode
/// (`JS_ToUint8ClampFree`, `quickjs.c:13381`). The oracle confirms the tie
/// direction: writing `0.5`, `1.5`, `2.5`, and `3.5` into a `Uint8ClampedArray`
/// yields `0`, `2`, `2`, and `4`.
#[must_use]
#[allow(
    dead_code,
    reason = "the clamped conversion is verified against the oracle before the Uint8ClampedArray surface that consumes it exists"
)]
pub(crate) fn number_to_uint8_clamp(value: JsNumber) -> u8 {
    let value = value.as_f64();
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    if value >= 255.0 {
        return 255;
    }
    round_half_to_even(value)
}

/// Rounds a value in `0.0..255.0` half-to-even without a floating-point cast.
fn round_half_to_even(value: f64) -> u8 {
    let floor = value.floor();
    // `floor` lies in `0.0..255.0`, so the truncation is exact.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the caller's range check proves the floor is an exact u8"
    )]
    let lower = floor as u8;
    let fraction = value - floor;
    // A tie is an exact property of the halfway value, so the comparison is
    // deliberately exact rather than approximate.
    #[expect(
        clippy::float_cmp,
        reason = "the tie case is exactly 0.5, so an epsilon comparison would round the wrong values"
    )]
    let tie = fraction == 0.5;
    if fraction > 0.5 || (tie && lower % 2 == 1) {
        lower.saturating_add(1)
    } else {
        lower
    }
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

/// Applies ECMAScript `CanonicalNumericIndexString`.
///
/// Answers `Some(number)` when `key` is the canonical `ToString` spelling of a
/// Number, and `None` otherwise. `"-0"` is the one key whose round trip fails
/// yet is still canonical, so it is answered directly, matching the
/// `JS_ATOM_minus_zero` case of `JS_AtomIsNumericIndex1` (`quickjs.c:3675`).
///
/// This is what separates a typed array's integer-indexed slots from its
/// ordinary properties: `"1.0"`, `"00"`, `"+1"`, and `"1e0"` all name Numbers but
/// are not canonical, so they stay ordinary properties, while `"NaN"`,
/// `"Infinity"`, and `"0.5"` are canonical and therefore reach the exotic
/// integer-index path. The pinned oracle confirms the split through
/// `Object.defineProperty` on an `Int8Array`: `"1.0"` becomes an own property
/// while `"0.5"` reports `non integer index in typed array`.
///
/// # Errors
///
/// Returns an error only if a temporary string buffer cannot be allocated.
#[allow(
    dead_code,
    reason = "the canonical-index predicate is verified against the oracle before the integer-indexed exotic objects that consume it exist"
)]
pub(crate) fn canonical_numeric_index_string(
    key: &JsString,
) -> Result<Option<JsNumber>, JsStringError> {
    // Upstream's fast rejection: a canonical spelling starts with a digit or a
    // minus sign, except for the three named values handled below. Keeping it
    // means an ordinary method name never runs a numeric conversion.
    let first = key.code_unit_at(0);
    let numeric_start = first.is_some_and(|unit| is_ascii_digit(unit) || unit == u16::from(b'-'));
    if !numeric_start {
        // `NaN` and `Infinity` are canonical yet start with a letter.
        let named = if string_equals_ascii(key, "NaN") {
            Some(f64::NAN)
        } else if string_equals_ascii(key, "Infinity") {
            Some(f64::INFINITY)
        } else {
            None
        };
        return Ok(named.map(JsNumber::from_f64));
    }

    // `ToString(-0)` is `"0"`, so the round trip below would reject `"-0"`
    // even though it is the canonical spelling of negative zero.
    if string_equals_ascii(key, "-0") {
        return Ok(Some(JsNumber::from_f64(-0.0)));
    }

    let number = string_to_number(key)?;
    let rendered = number.to_javascript_string()?;
    Ok((rendered == *key).then_some(number))
}

/// Compares a string against an ASCII literal by code unit.
fn string_equals_ascii(value: &JsString, expected: &str) -> bool {
    if usize::try_from(value.len()).ok() != Some(expected.len()) {
        return false;
    }
    expected
        .bytes()
        .zip(0_u32..)
        .all(|(byte, index)| value.code_unit_at(index) == Some(u16::from(byte)))
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
        unit if (u16::from(b'a')..=u16::from(b'z')).contains(&unit) => {
            Some(ascii_byte(unit) - b'a' + 10)
        }
        unit if (u16::from(b'A')..=u16::from(b'Z')).contains(&unit) => {
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
        8..=15 => 4,
        _ => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_SAFE_INTEGER, canonical_numeric_index_string, max_safe_integer_as_f64, number_to_index,
        number_to_int8, number_to_int16, number_to_int32, number_to_integer_or_infinity,
        number_to_length, number_to_uint8, number_to_uint8_clamp, number_to_uint16,
        number_to_uint32, string_to_number, string_to_parse_float, string_to_parse_int,
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

    fn parse_float(input: &str) -> JsNumber {
        string_to_parse_float(&JsString::from_utf8(input).expect("test string"))
            .expect("temporary decimal storage")
    }

    fn parse_int(input: &str, radix: i32) -> JsNumber {
        string_to_parse_int(&JsString::from_utf8(input).expect("test string"), radix)
            .expect("temporary digit storage")
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

    /// `parseFloat` accepts the longest decimal-literal prefix after trimming
    /// leading ECMAScript whitespace. The expected values were checked against
    /// the pinned `QuickJS` oracle.
    #[test]
    fn parse_float_uses_the_longest_decimal_prefix() {
        for (source, expected_bits) in [
            ("  -1.25e2tail", (-125.0_f64).to_bits()),
            ("1e", 1.0_f64.to_bits()),
            ("1e+", 1.0_f64.to_bits()),
            (".5x", 0.5_f64.to_bits()),
            ("+Infinitytail", f64::INFINITY.to_bits()),
            ("-Infinityx", f64::NEG_INFINITY.to_bits()),
            ("-0x", (-0.0_f64).to_bits()),
            ("0x10", 0.0_f64.to_bits()),
        ] {
            assert_eq!(
                parse_float(source).as_f64().to_bits(),
                expected_bits,
                "parseFloat({source:?})"
            );
        }

        for source in ["", " \t\n", ".", "+", "-", "NaN", "infinity"] {
            assert!(
                parse_float(source).as_f64().is_nan(),
                "parseFloat({source:?})"
            );
        }

        let surrogate_terminated =
            JsString::from_code_units([u16::from(b' '), u16::from(b'1'), 0xd800, u16::from(b'2')])
                .expect("test string");
        assert_eq!(
            string_to_parse_float(&surrogate_terminated)
                .expect("temporary decimal storage")
                .as_f64()
                .to_bits(),
            1.0_f64.to_bits()
        );
    }

    /// `parseInt` applies radix selection and prefix stripping before taking
    /// the longest valid digit prefix. The large-integer bit patterns make the
    /// required binary64 rounding independently visible.
    #[test]
    fn parse_int_applies_radix_prefix_and_binary64_rounding() {
        for (source, radix, expected_bits) in [
            ("  -0xFzz", 0, (-15.0_f64).to_bits()),
            ("0x10", 0, 16.0_f64.to_bits()),
            ("0x10", 16, 16.0_f64.to_bits()),
            ("0x10", 10, 0.0_f64.to_bits()),
            ("08", 0, 8.0_f64.to_bits()),
            ("11", 2, 3.0_f64.to_bits()),
            ("z", 36, 35.0_f64.to_bits()),
            ("900719925474099267", 10, 0x43a9_0000_0000_0001),
            ("ffffffffffffffff", 16, 0x43f0_0000_0000_0000),
            ("1000000000000081", 16, 0x43b0_0000_0000_0001),
            ("1000000000000181", 16, 0x43b0_0000_0000_0002),
        ] {
            assert_eq!(
                parse_int(source, radix).as_f64().to_bits(),
                expected_bits,
                "parseInt({source:?}, {radix})"
            );
        }

        assert!(parse_int("-0", 10).same_value(JsNumber::from_f64(-0.0)));
        for (source, radix) in [
            ("", 10),
            ("+", 10),
            ("0x", 0),
            ("1", 1),
            ("1", 37),
            ("1", -2),
        ] {
            assert!(
                parse_int(source, radix).as_f64().is_nan(),
                "parseInt({source:?}, {radix})"
            );
        }

        let overflowing_decimal = format!("1{}", "0".repeat(400));
        assert_eq!(
            parse_int(&overflowing_decimal, 10).as_f64().to_bits(),
            f64::INFINITY.to_bits()
        );
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

    /// The modular narrow conversions truncate a `ToUint32` result.
    ///
    /// Typed-array element writes are the only script-reachable path to these,
    /// so the expectations come from the pinned oracle:
    ///
    /// ```console
    /// $ /private/tmp/quickjs-2026-06-04/qjs -e 'const v=new Int8Array(1);\
    ///   for(const x of [128,-129,254.5,4294967297]){v[0]=x;console.log(x,v[0]);}'
    /// 128 -128
    /// -129 127
    /// 254.5 -2
    /// 4294967297 1
    /// ```
    #[test]
    fn the_modular_narrow_conversions_match_the_oracle() {
        // (input, Int8, Uint8, Int16, Uint16)
        let cases: [(f64, i8, u8, i16, u16); 21] = [
            (0.0, 0, 0, 0, 0),
            (-0.0, 0, 0, 0, 0),
            (1.9, 1, 1, 1, 1),
            (-1.9, -1, 255, -1, 65535),
            (127.5, 127, 127, 127, 127),
            (128.0, -128, 128, 128, 128),
            (255.0, -1, 255, 255, 255),
            (256.0, 0, 0, 256, 256),
            (257.0, 1, 1, 257, 257),
            (-1.0, -1, 255, -1, 65535),
            (-128.0, -128, 128, -128, 65408),
            (-129.0, 127, 127, -129, 65407),
            (32767.0, -1, 255, 32767, 32767),
            (32768.0, 0, 0, -32768, 32768),
            (-32769.0, -1, 255, 32767, 32767),
            (65535.0, -1, 255, -1, 65535),
            (65536.0, 0, 0, 0, 0),
            (254.5, -2, 254, 254, 254),
            (f64::NAN, 0, 0, 0, 0),
            (f64::INFINITY, 0, 0, 0, 0),
            (4_294_967_297.0, 1, 1, 1, 1),
        ];
        for (input, expected_int8, expected_uint8, expected_int16, expected_uint16) in cases {
            let number = JsNumber::from_f64(input);
            assert_eq!(number_to_int8(number), expected_int8, "ToInt8({input})");
            assert_eq!(number_to_uint8(number), expected_uint8, "ToUint8({input})");
            assert_eq!(number_to_int16(number), expected_int16, "ToInt16({input})");
            assert_eq!(
                number_to_uint16(number),
                expected_uint16,
                "ToUint16({input})"
            );
        }
    }

    /// `ToUint8Clamp` saturates instead of wrapping and rounds half-to-even.
    ///
    /// The tie direction is upstream's `lrint` (`quickjs.c:13381`), which the
    /// pinned oracle confirms:
    ///
    /// ```console
    /// $ /private/tmp/quickjs-2026-06-04/qjs -e 'const v=new Uint8ClampedArray(1);\
    ///   for(const x of [0.5,1.5,2.5,3.5,-1,256]){v[0]=x;console.log(x,v[0]);}'
    /// 0.5 0
    /// 1.5 2
    /// 2.5 2
    /// 3.5 4
    /// -1 0
    /// 256 255
    /// ```
    #[test]
    fn to_uint8_clamp_saturates_and_rounds_half_to_even() {
        for (input, expected) in [
            (0.0, 0),
            (-0.0, 0),
            (0.5, 0),
            (1.5, 2),
            (2.5, 2),
            (3.5, 4),
            (1.9, 2),
            (-1.9, 0),
            (-0.5, 0),
            (127.0, 127),
            (127.5, 128),
            (128.0, 128),
            (128.5, 128),
            (253.5, 254),
            (254.5, 254),
            (255.0, 255),
            (255.5, 255),
            (256.0, 255),
            (-1.0, 0),
            (-255.0, 0),
            (f64::NAN, 0),
            (f64::INFINITY, 255),
            (f64::NEG_INFINITY, 0),
            (4_294_967_296.0, 255),
            (9_007_199_254_740_992.0, 255),
        ] {
            assert_eq!(
                number_to_uint8_clamp(JsNumber::from_f64(input)),
                expected,
                "ToUint8Clamp({input})"
            );
        }
    }

    /// `CanonicalNumericIndexString` accepts only the exact `ToString` spelling.
    ///
    /// A key is canonical when `String(Number(key))` is the key itself, plus the
    /// `"-0"` special case whose round trip renders `"0"`. The pinned oracle
    /// enumerates the boundary:
    ///
    /// ```console
    /// $ /private/tmp/quickjs-2026-06-04/qjs -e 'for (const k of ["1.0","1e21","1e+21",\
    ///     "-0","NaN","1e-7","1e-6"]) console.log(k, k==="-0"||String(Number(k))===k);'
    /// 1.0 false
    /// 1e21 false
    /// 1e+21 true
    /// -0 true
    /// NaN true
    /// 1e-7 true
    /// 1e-6 false
    /// ```
    #[test]
    fn canonical_numeric_index_strings_match_the_oracle() {
        let canonical: [(&str, f64); 13] = [
            ("0", 0.0),
            ("1", 1.0),
            ("-1", -1.0),
            ("Infinity", f64::INFINITY),
            ("-Infinity", f64::NEG_INFINITY),
            ("4294967295", 4_294_967_295.0),
            ("4294967296", 4_294_967_296.0),
            ("0.5", 0.5),
            ("-0.5", -0.5),
            ("0.1", 0.1),
            ("1e-7", 1e-7),
            ("-1e-7", -1e-7),
            ("1e+21", 1e21),
        ];
        for (key, expected) in canonical {
            let key = JsString::from_utf8(key).expect("test key");
            let actual = canonical_numeric_index_string(&key)
                .expect("temporary storage")
                .expect("a canonical key");
            assert_eq!(
                actual.as_f64().to_bits(),
                expected.to_bits(),
                "CanonicalNumericIndexString({key:?})"
            );
        }

        // `NaN` is canonical but never equal to itself, so it is checked apart.
        let nan_key = JsString::from_utf8("NaN").expect("test key");
        assert!(
            canonical_numeric_index_string(&nan_key)
                .expect("temporary storage")
                .expect("NaN is canonical")
                .as_f64()
                .is_nan()
        );

        // `-0` is canonical even though `ToString(-0)` renders `"0"`.
        let negative_zero = JsString::from_utf8("-0").expect("test key");
        assert_eq!(
            canonical_numeric_index_string(&negative_zero)
                .expect("temporary storage")
                .expect("-0 is canonical")
                .as_f64()
                .to_bits(),
            (-0.0_f64).to_bits()
        );

        for key in [
            "",
            "00",
            "0.0",
            "1.0",
            " 1",
            "1 ",
            "+1",
            "1e0",
            "1e21",
            "0x1",
            "abc",
            "1.5e300",
            "9007199254740993",
            "1e100",
            "1e-6",
            "Infinityx",
            "nan",
            "-",
        ] {
            let key = JsString::from_utf8(key).expect("test key");
            assert!(
                canonical_numeric_index_string(&key)
                    .expect("temporary storage")
                    .is_none(),
                "CanonicalNumericIndexString({key:?}) must be undefined"
            );
        }
    }
}
