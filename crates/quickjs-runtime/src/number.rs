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
