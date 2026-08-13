/*
 * JavaScript number representation derived from QuickJS.
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

use std::fmt;

use crate::string::{JsString, JsStringError, MAX_STRING_CODE_UNITS};

pub(crate) mod decimal;
mod radix;

/// An ECMAScript Number with the pinned `QuickJS` integer fast-path invariant.
///
/// JavaScript exposes one binary64 Number domain. Exact signed 32-bit values
/// may use a private integer representation, while every other value remains
/// binary64. In particular, negative zero is never collapsed to integer zero.
#[derive(Clone, Copy)]
pub struct JsNumber(NumberRepr);

#[derive(Clone, Copy)]
enum NumberRepr {
    Int(i32),
    Float(f64),
}

impl fmt::Debug for JsNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("JsNumber")
            .field(&self.as_f64())
            .finish()
    }
}

impl JsNumber {
    /// Creates a Number from binary64.
    ///
    /// Exact signed 32-bit values use the integer fast path only when their
    /// binary64 bit pattern is identical after a round trip. The bitwise check
    /// preserves negative zero.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the value is range-checked and its binary64 bits are compared after the cast"
    )]
    pub fn from_f64(value: f64) -> Self {
        if value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX) {
            let integer = value as i32;
            if value.to_bits() == f64::from(integer).to_bits() {
                return Self(NumberRepr::Int(integer));
            }
        }
        Self(NumberRepr::Float(value))
    }

    /// Creates an exact signed 32-bit Number.
    #[must_use]
    pub const fn from_i32(value: i32) -> Self {
        Self(NumberRepr::Int(value))
    }

    /// Creates a Number using `QuickJS`'s signed host-integer conversion.
    ///
    /// Values outside signed 32-bit range are rounded to binary64 as required
    /// by the ECMAScript Number domain.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "ECMAScript Number conversion intentionally rounds host i64 values to binary64"
    )]
    pub fn from_i64(value: i64) -> Self {
        i32::try_from(value).map_or_else(
            |_| Self(NumberRepr::Float(value as f64)),
            |value| Self(NumberRepr::Int(value)),
        )
    }

    /// Creates a Number using `QuickJS`'s unsigned host-integer conversion.
    ///
    /// Values above signed 32-bit range remain binary64.
    #[must_use]
    pub fn from_u32(value: u32) -> Self {
        i32::try_from(value).map_or_else(
            |_| Self(NumberRepr::Float(f64::from(value))),
            |value| Self(NumberRepr::Int(value)),
        )
    }

    /// Returns the observable binary64 value.
    #[must_use]
    pub fn as_f64(self) -> f64 {
        match self.0 {
            NumberRepr::Int(value) => f64::from(value),
            NumberRepr::Float(value) => value,
        }
    }

    /// Formats this Number for JavaScript source-string coercion.
    ///
    /// The binary64 digit sequence uses Rust's shortest round-tripping
    /// formatter, then applies the pinned `QuickJS` decimal-vs-exponent
    /// thresholds and exponent spelling. Integer fast-path values never pass
    /// through binary64 formatting.
    pub(crate) fn to_javascript_string(self) -> Result<JsString, JsStringError> {
        match self.0 {
            NumberRepr::Int(value) => format_i32_for_javascript(value),
            NumberRepr::Float(value) => format_binary64_for_javascript(value),
        }
    }

    /// Formats this Number in a validated radix from 2 through 36.
    ///
    /// Base ten retains the ordinary JavaScript decimal thresholds. Other
    /// radices use the pinned `QuickJS` free-format algorithm and never emit an
    /// exponent.
    pub(crate) fn to_radix_string(self, radix: u32) -> Result<JsString, JsStringError> {
        debug_assert!((2..=36).contains(&radix));
        if radix == 10 {
            return self.to_javascript_string();
        }
        match self.0 {
            NumberRepr::Int(value) => radix::format_i32_radix(value, radix),
            NumberRepr::Float(value) => radix::format_binary64_radix(value, radix),
        }
    }

    /// Applies the numeric fast path used by the `add` bytecode instruction.
    ///
    /// This method performs Number addition only. String concatenation and
    /// `ToPrimitive`/`ToNumeric` belong to the VM's general addition path.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "the i32 sum is bounded well within binary64's exact integer range"
    )]
    pub fn add_numeric(self, other: Self) -> Self {
        match (self.0, other.0) {
            (NumberRepr::Int(left), NumberRepr::Int(right)) => {
                let sum = i64::from(left) + i64::from(right);
                i32::try_from(sum).map_or_else(
                    |_| Self(NumberRepr::Float(sum as f64)),
                    |sum| Self(NumberRepr::Int(sum)),
                )
            }
            _ => Self(NumberRepr::Float(self.as_f64() + other.as_f64())),
        }
    }

    /// Implements ECMAScript strict numeric equality.
    ///
    /// NaN is unequal to every Number; positive and negative zero are equal.
    #[must_use]
    #[expect(
        clippy::float_cmp,
        reason = "ECMAScript strict equality requires exact binary64 comparison semantics"
    )]
    pub fn strict_equals(self, other: Self) -> bool {
        self.as_f64() == other.as_f64()
    }

    /// Implements ECMAScript `SameValue` for Numbers.
    ///
    /// All NaN values compare equal, while positive and negative zero remain
    /// distinct.
    #[must_use]
    #[expect(
        clippy::float_cmp,
        reason = "ECMAScript SameValue requires exact binary64 comparison semantics"
    )]
    pub fn same_value(self, other: Self) -> bool {
        let left = self.as_f64();
        let right = other.as_f64();
        if left.is_nan() {
            return right.is_nan();
        }
        if left == 0.0 && right == 0.0 {
            return left.to_bits() == right.to_bits();
        }
        left == right
    }

    /// Implements ECMAScript `SameValueZero` for Numbers.
    ///
    /// All NaN values compare equal, and both zero signs compare equal.
    #[must_use]
    #[expect(
        clippy::float_cmp,
        reason = "ECMAScript SameValueZero requires exact binary64 comparison semantics"
    )]
    pub fn same_value_zero(self, other: Self) -> bool {
        let left = self.as_f64();
        let right = other.as_f64();
        left == right || (left.is_nan() && right.is_nan())
    }

    #[cfg(test)]
    const fn is_int32_optimized(self) -> bool {
        matches!(self.0, NumberRepr::Int(_))
    }
}

const RENDERED_BINARY64_INLINE_BYTES: usize = 384;
const JAVASCRIPT_NUMBER_INLINE_BYTES: usize = 64;

fn format_i32_for_javascript(value: i32) -> Result<JsString, JsStringError> {
    let mut output = FallibleAsciiBuffer::<JAVASCRIPT_NUMBER_INLINE_BYTES>::new();
    output.write_arguments(format_args!("{value}"))?;
    output.into_js_string()
}

fn format_binary64_for_javascript(value: f64) -> Result<JsString, JsStringError> {
    let bits = value.to_bits();
    let absolute_bits = bits & 0x7fff_ffff_ffff_ffff;
    if absolute_bits > 0x7ff0_0000_0000_0000 {
        return JsString::from_latin1(b"NaN");
    }
    if absolute_bits == 0x7ff0_0000_0000_0000 {
        return if bits >> 63 == 0 {
            JsString::from_latin1(b"Infinity")
        } else {
            JsString::from_latin1(b"-Infinity")
        };
    }
    if absolute_bits == 0 {
        return JsString::from_latin1(b"0");
    }

    let mut rendered = FallibleAsciiBuffer::<RENDERED_BINARY64_INLINE_BYTES>::new();
    rendered.write_arguments(format_args!("{value}"))?;
    let rendered = rendered.as_slice();
    let (negative, unsigned) = rendered
        .strip_prefix(b"-")
        .map_or((false, rendered), |unsigned| (true, unsigned));
    let (mantissa, explicit_exponent) = unsigned
        .iter()
        .position(|byte| matches!(byte, b'e' | b'E'))
        .map_or((unsigned, 0), |position| {
            (
                &unsigned[..position],
                parse_decimal_exponent(&unsigned[position + 1..]),
            )
        });
    let decimal_position = mantissa
        .iter()
        .position(|byte| *byte == b'.')
        .unwrap_or(mantissa.len());

    let mut untrimmed_digits = FallibleAsciiBuffer::<RENDERED_BINARY64_INLINE_BYTES>::new();
    for byte in mantissa.iter().copied().filter(|byte| *byte != b'.') {
        untrimmed_digits.push_byte(byte)?;
    }
    let first_significant = untrimmed_digits
        .as_slice()
        .iter()
        .position(|byte| *byte != b'0')
        .unwrap_or(0);
    let scientific_exponent = explicit_exponent
        .saturating_add(i32::try_from(decimal_position).unwrap_or(i32::MAX))
        .saturating_sub(i32::try_from(first_significant).unwrap_or(i32::MAX))
        .saturating_sub(1);
    let mut significant_end = untrimmed_digits.len();
    while significant_end > first_significant + 1
        && untrimmed_digits.as_slice()[significant_end - 1] == b'0'
    {
        significant_end -= 1;
    }
    let digits = &untrimmed_digits.as_slice()[first_significant..significant_end];

    let mut output = FallibleAsciiBuffer::<JAVASCRIPT_NUMBER_INLINE_BYTES>::new();
    if negative {
        output.push_byte(b'-')?;
    }
    if !(-6..21).contains(&scientific_exponent) {
        output.push_byte(digits[0])?;
        if digits.len() > 1 {
            output.push_bytes(b".")?;
            output.push_bytes(&digits[1..])?;
        }
        output.push_byte(b'e')?;
        if scientific_exponent >= 0 {
            output.push_byte(b'+')?;
        }
        output.write_arguments(format_args!("{scientific_exponent}"))?;
        return output.into_js_string();
    }

    if scientific_exponent < 0 {
        output.push_bytes(b"0.")?;
        let zeros = usize::try_from(-scientific_exponent - 1).unwrap_or(0);
        output.push_repeated(b'0', zeros)?;
        output.push_bytes(digits)?;
        return output.into_js_string();
    }

    let integer_digits = usize::try_from(scientific_exponent + 1).unwrap_or(usize::MAX);
    if integer_digits >= digits.len() {
        output.push_bytes(digits)?;
        output.push_repeated(b'0', integer_digits - digits.len())?;
    } else {
        output.push_bytes(&digits[..integer_digits])?;
        output.push_byte(b'.')?;
        output.push_bytes(&digits[integer_digits..])?;
    }
    output.into_js_string()
}

fn parse_decimal_exponent(value: &[u8]) -> i32 {
    let (negative, digits) = value
        .strip_prefix(b"-")
        .map_or((false, value), |digits| (true, digits));
    let digits = digits.strip_prefix(b"+").unwrap_or(digits);
    let exponent = digits.iter().fold(0_i32, |exponent, digit| {
        exponent
            .saturating_mul(10)
            .saturating_add(i32::from(digit.saturating_sub(b'0')))
    });
    if negative { -exponent } else { exponent }
}

struct FallibleAsciiBuffer<const INLINE_BYTES: usize> {
    inline: [u8; INLINE_BYTES],
    inline_len: usize,
    heap: Option<Vec<u8>>,
    format_error: Option<JsStringError>,
}

impl<const INLINE_BYTES: usize> FallibleAsciiBuffer<INLINE_BYTES> {
    const fn new() -> Self {
        Self {
            inline: [0; INLINE_BYTES],
            inline_len: 0,
            heap: None,
            format_error: None,
        }
    }

    fn len(&self) -> usize {
        self.heap.as_ref().map_or(self.inline_len, Vec::len)
    }

    fn as_slice(&self) -> &[u8] {
        self.heap
            .as_deref()
            .unwrap_or(&self.inline[..self.inline_len])
    }

    fn push_byte(&mut self, byte: u8) -> Result<(), JsStringError> {
        self.push_bytes(&[byte])
    }

    fn push_repeated(&mut self, byte: u8, count: usize) -> Result<(), JsStringError> {
        for _ in 0..count {
            self.push_byte(byte)?;
        }
        Ok(())
    }

    fn push_bytes(&mut self, bytes: &[u8]) -> Result<(), JsStringError> {
        debug_assert!(bytes.is_ascii(), "number formatting emits ASCII");
        let requested = self
            .len()
            .checked_add(bytes.len())
            .ok_or(JsStringError::TooLong {
                requested: u64::MAX,
                maximum: MAX_STRING_CODE_UNITS,
            })?;
        if requested > usize::try_from(MAX_STRING_CODE_UNITS).unwrap_or(usize::MAX) {
            return Err(JsStringError::TooLong {
                requested: u64::try_from(requested).unwrap_or(u64::MAX),
                maximum: MAX_STRING_CODE_UNITS,
            });
        }

        if let Some(heap) = self.heap.as_mut() {
            if heap.capacity() - heap.len() < bytes.len() {
                heap.try_reserve(bytes.len())
                    .map_err(|_| JsStringError::AllocationFailed {
                        additional: bytes.len(),
                    })?;
            }
            heap.extend_from_slice(bytes);
            return Ok(());
        }

        if requested <= INLINE_BYTES {
            self.inline[self.inline_len..requested].copy_from_slice(bytes);
            self.inline_len = requested;
            return Ok(());
        }

        let mut heap = Vec::new();
        heap.try_reserve_exact(requested)
            .map_err(|_| JsStringError::AllocationFailed {
                additional: requested,
            })?;
        heap.extend_from_slice(&self.inline[..self.inline_len]);
        heap.extend_from_slice(bytes);
        self.heap = Some(heap);
        Ok(())
    }

    fn write_arguments(&mut self, arguments: fmt::Arguments<'_>) -> Result<(), JsStringError> {
        if fmt::write(self, arguments).is_ok() {
            return Ok(());
        }
        Err(self
            .format_error
            .take()
            .unwrap_or(JsStringError::AllocationFailed { additional: 0 }))
    }

    fn into_js_string(self) -> Result<JsString, JsStringError> {
        JsString::from_latin1(self.as_slice())
    }
}

impl<const INLINE_BYTES: usize> fmt::Write for FallibleAsciiBuffer<INLINE_BYTES> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.push_bytes(value.as_bytes()).map_err(|error| {
            self.format_error = Some(error);
            fmt::Error
        })
    }
}

impl From<i32> for JsNumber {
    fn from(value: i32) -> Self {
        Self::from_i32(value)
    }
}

impl From<u32> for JsNumber {
    fn from(value: u32) -> Self {
        Self::from_u32(value)
    }
}

impl From<f64> for JsNumber {
    fn from(value: f64) -> Self {
        Self::from_f64(value)
    }
}

impl From<JsNumber> for f64 {
    fn from(value: JsNumber) -> Self {
        value.as_f64()
    }
}

#[cfg(test)]
mod tests {
    use super::JsNumber;
    use fusor_bytecode::Binary64Constant;

    #[test]
    fn exact_i32_values_use_the_integer_fast_path_without_losing_negative_zero() {
        for value in [i32::MIN, -1, 0, 1, i32::MAX] {
            let number = JsNumber::from_f64(f64::from(value));
            assert!(number.is_int32_optimized(), "{value}");
            assert_number_bits(number, f64::from(value));
        }

        let negative_zero = JsNumber::from_f64(-0.0);
        assert!(!negative_zero.is_int32_optimized());
        assert_eq!(negative_zero.as_f64().to_bits(), (-0.0_f64).to_bits());
    }

    #[test]
    fn values_adjacent_to_i32_range_remain_binary64() {
        let below = JsNumber::from_f64(f64::from(i32::MIN) - 1.0);
        let above = JsNumber::from_f64(f64::from(i32::MAX) + 1.0);

        assert!(!below.is_int32_optimized());
        assert!(!above.is_int32_optimized());
        assert_number_bits(below, -2_147_483_649.0);
        assert_number_bits(above, 2_147_483_648.0);
    }

    #[test]
    fn integer_addition_stays_fast_until_i32_overflow() {
        let answer = JsNumber::from_i32(40).add_numeric(JsNumber::from_i32(2));
        assert!(answer.is_int32_optimized());
        assert_number_bits(answer, 42.0);

        let overflow = JsNumber::from_i32(i32::MAX).add_numeric(JsNumber::from_i32(1));
        assert!(!overflow.is_int32_optimized());
        assert_number_bits(overflow, 2_147_483_648.0);
    }

    #[test]
    fn a_float_operand_keeps_the_float_result_representation() {
        let result = JsNumber::from_f64(0.5).add_numeric(JsNumber::from_f64(0.5));
        assert!(!result.is_int32_optimized());
        assert_number_bits(result, 1.0);
    }

    #[test]
    fn numeric_addition_preserves_binary64_edge_cases() {
        let negative_overflow = JsNumber::from_i32(i32::MIN).add_numeric(JsNumber::from_i32(-1));
        assert!(!negative_overflow.is_int32_optimized());
        assert_number_bits(negative_overflow, -2_147_483_649.0);

        let signed_zero = JsNumber::from_f64(-0.0).add_numeric(JsNumber::from_f64(-0.0));
        assert_number_bits(signed_zero, -0.0);

        let nan =
            JsNumber::from_f64(f64::INFINITY).add_numeric(JsNumber::from_f64(f64::NEG_INFINITY));
        assert!(nan.as_f64().is_nan());
    }

    #[test]
    fn numeric_equality_modes_distinguish_nan_and_signed_zero() {
        let nan = JsNumber::from_f64(f64::NAN);
        let other_nan = JsNumber::from_f64(f64::from_bits(0x7ff8_0000_0000_0001));
        let positive_zero = JsNumber::from_f64(0.0);
        let negative_zero = JsNumber::from_f64(-0.0);

        assert!(!nan.strict_equals(other_nan));
        assert!(nan.same_value(other_nan));
        assert!(nan.same_value_zero(other_nan));

        assert!(positive_zero.strict_equals(negative_zero));
        assert!(!positive_zero.same_value(negative_zero));
        assert!(positive_zero.same_value_zero(negative_zero));
    }

    #[test]
    fn numeric_equality_does_not_expose_the_integer_fast_path() {
        let integer = JsNumber::from_i32(1);
        let binary64 = JsNumber::from_f64(0.5).add_numeric(JsNumber::from_f64(0.5));

        assert!(integer.is_int32_optimized());
        assert!(!binary64.is_int32_optimized());
        assert!(integer.strict_equals(binary64));
        assert!(integer.same_value(binary64));
        assert!(integer.same_value_zero(binary64));
    }

    #[test]
    fn source_string_format_matches_quickjs_decimal_thresholds() {
        for (value, expected) in [
            (0.0, "0"),
            (-0.0, "0"),
            (1.5, "1.5"),
            (1.0e-7, "1e-7"),
            (1.0e-6, "0.000001"),
            (1.0e20, "100000000000000000000"),
            (1.0e21, "1e+21"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
            (0.000_001_234_567_890_123, "0.000001234567890123"),
        ] {
            let actual = JsNumber::from_f64(value)
                .to_javascript_string()
                .expect("number string");
            assert_eq!(
                actual.to_utf8_lossy().expect("ASCII number string"),
                expected
            );
        }
    }

    #[test]
    fn source_string_format_covers_binary64_and_integer_buffer_boundaries() {
        for (number, expected) in [
            (JsNumber::from_i32(i32::MIN), "-2147483648"),
            (JsNumber::from_i32(i32::MAX), "2147483647"),
            (JsNumber::from_f64(f64::MAX), "1.7976931348623157e+308"),
            (JsNumber::from_f64(f64::from_bits(1)), "5e-324"),
            (JsNumber::from_f64(-f64::from_bits(1)), "-5e-324"),
            (
                JsNumber::from_f64(f64::MIN_POSITIVE),
                "2.2250738585072014e-308",
            ),
            (
                JsNumber::from_f64(1.234_567_890_123_456_7e200),
                "1.2345678901234567e+200",
            ),
        ] {
            assert_eq!(
                number
                    .to_javascript_string()
                    .expect("number string")
                    .to_utf8_lossy()
                    .expect("ASCII number string"),
                expected
            );
        }
    }

    #[test]
    fn fallible_source_format_matches_project_spelling_across_binary64_space() {
        let mut state = 0x6a09_e667_f3bc_c909_u64;
        for index in 0..100_000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let value = f64::from_bits(state);
            let expected = Binary64Constant::from_f64(value).to_javascript_string();
            let actual = JsNumber::from_f64(value)
                .to_javascript_string()
                .expect("fallible number string")
                .to_utf8_lossy()
                .expect("ASCII number string");
            assert_eq!(actual, expected, "case {index}, bits {state:016x}");
        }
    }

    #[test]
    fn infinities_and_large_host_integers_round_trip_as_numbers() {
        assert_number_bits(JsNumber::from_f64(f64::INFINITY), f64::INFINITY);
        assert_number_bits(JsNumber::from_f64(f64::NEG_INFINITY), f64::NEG_INFINITY);
        assert_number_bits(JsNumber::from_i64(i64::MAX), 9_223_372_036_854_775_808.0);
        assert_number_bits(JsNumber::from_u32(u32::MAX), f64::from(u32::MAX));
    }

    fn assert_number_bits(actual: JsNumber, expected: f64) {
        assert_eq!(actual.as_f64().to_bits(), expected.to_bits());
    }
}
