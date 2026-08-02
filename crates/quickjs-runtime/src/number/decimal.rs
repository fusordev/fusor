/*
 * JavaScript Number.prototype decimal formatting derived from QuickJS.
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

//! Exact fixed-point and exponential rendering for `Number.prototype`.
//!
//! `toFixed`, `toExponential`, and `toPrecision` all round the *exact* value the
//! binary64 holds, not its shortest decimal spelling. That distinction is
//! observable and is the reason this module exists rather than delegating to
//! Rust's formatter: `(1.005).toFixed(2)` is `"1.00"`, because the stored value
//! is slightly below 1.005, while `(1.55).toFixed(1)` is `"1.6"` because that one
//! is slightly above. Formatting the shortest spelling would round both up.
//!
//! Every binary64 is exactly `significand * 2^exponent`, so the exact decimal
//! digits are obtained with integer arithmetic alone: scale by a power of ten,
//! divide by a power of two (or multiply, when the exponent is positive), and
//! round the integer quotient half away from zero. [`JsBigInt`] supplies that
//! arithmetic, so no step introduces rounding of its own.

use crate::bigint::{BigIntError, JsBigInt};

/// The decimal digits of an exactly-rendered value, plus its sign.
pub(crate) struct DecimalDigits {
    /// Whether the source value was negative, including negative zero.
    pub(crate) negative: bool,
    /// The significant decimal digits, most significant first, with no leading
    /// zero unless the value is zero.
    pub(crate) digits: String,
    /// The base-ten exponent of the first digit.
    ///
    /// The rendered value is `0.<digits> * 10^(exponent + 1)`, so a `digits` of
    /// `"123"` with an `exponent` of `2` is `123`.
    pub(crate) exponent: i32,
}

/// Decomposes a finite binary64 into its exact sign, significand, and exponent.
///
/// The returned significand is an integer and the value is exactly
/// `significand * 2^exponent`.
fn decompose(value: f64) -> (bool, u64, i32) {
    let bits = value.to_bits();
    let negative = bits >> 63 == 1;
    let biased_exponent = ((bits >> 52) & 0x7ff) as u32;
    let fraction = bits & ((1_u64 << 52) - 1);
    if biased_exponent == 0 {
        // A subnormal has no implicit leading one and a fixed exponent.
        (negative, fraction, -1074)
    } else {
        let significand = fraction | (1_u64 << 52);
        // The exponent field is 11 bits, so the cast cannot lose information.
        let exponent = biased_exponent.cast_signed() - 1075;
        (negative, significand, exponent)
    }
}

/// Renders `value` with exactly `fraction_digits` digits after the point.
///
/// The rounding is half away from zero on the exact stored value, matching the
/// pinned oracle: `(1.5).toFixed(0)` is `"2"` and `(2.5).toFixed(0)` is `"3"`,
/// while `(1.005).toFixed(2)` is `"1.00"` because the stored value is below the
/// halfway point.
///
/// # Errors
///
/// Returns an error only when the exact intermediate exceeds the `BigInt` limb
/// cap, which no admitted input can reach.
pub(crate) fn exact_fixed(value: f64, fraction_digits: u32) -> Result<String, BigIntError> {
    let (negative, significand, exponent) = decompose(value);
    // The scaled integer is `significand * 2^exponent * 10^fraction_digits`,
    // rounded half away from zero. Both factors are exact.
    let scaled = scale_and_round(significand, exponent, i64::from(fraction_digits))?;
    let mut digits = scaled.to_string_radix(10)?;
    let fraction_digits = fraction_digits as usize;
    if digits.len() <= fraction_digits {
        // Pad so the decimal point has enough digits to its right.
        let padding = fraction_digits + 1 - digits.len();
        let mut padded = String::new();
        padded.try_reserve_exact(padding + digits.len())?;
        for _ in 0..padding {
            padded.push('0');
        }
        padded.push_str(&digits);
        digits = padded;
    }
    let mut rendered = String::new();
    rendered.try_reserve_exact(digits.len() + 2)?;
    // A negative zero renders without its sign, which `(-0).toFixed(2)` shows as
    // `"0.00"`.
    if negative && scaled.is_zero() {
        // Nothing to add.
    } else if negative {
        rendered.push('-');
    }
    let split = digits.len() - fraction_digits;
    rendered.push_str(&digits[..split]);
    if fraction_digits > 0 {
        rendered.push('.');
        rendered.push_str(&digits[split..]);
    }
    Ok(rendered)
}

/// Returns the exact significant digits of `value`, rounded to `precision`.
///
/// # Errors
///
/// Returns an error only when the exact intermediate exceeds the `BigInt` limb
/// cap.
pub(crate) fn exact_significant(value: f64, precision: u32) -> Result<DecimalDigits, BigIntError> {
    let (negative, significand, exponent) = decompose(value);
    if significand == 0 {
        let mut digits = String::new();
        digits.try_reserve_exact(precision as usize)?;
        for _ in 0..precision {
            digits.push('0');
        }
        return Ok(DecimalDigits {
            negative,
            digits,
            exponent: 0,
        });
    }

    // Estimate the decimal exponent, then correct it after rounding, since the
    // rounding itself can carry into a new leading digit.
    let mut decimal_exponent = estimate_decimal_exponent(significand, exponent);
    for _ in 0..4 {
        let scale = i64::from(precision) - 1 - i64::from(decimal_exponent);
        let scaled = scale_and_round(significand, exponent, scale)?;
        let digits = scaled.to_string_radix(10)?;
        let length = i32::try_from(digits.len()).map_err(|_| BigIntError::ResultTooLarge)?;
        // The digit count reveals whether the estimate was right; rounding may
        // have added a digit, so this loop corrects rather than guesses.
        if length == i32::try_from(precision).map_err(|_| BigIntError::ResultTooLarge)? {
            return Ok(DecimalDigits {
                negative,
                digits,
                exponent: decimal_exponent,
            });
        }
        decimal_exponent += length - i32::try_from(precision).unwrap_or(0);
    }
    Err(BigIntError::ResultTooLarge)
}

/// Estimates the base-ten exponent of `significand * 2^exponent`.
///
/// The estimate can be off by one, which [`exact_significant`] corrects.
fn estimate_decimal_exponent(significand: u64, exponent: i32) -> i32 {
    // log10(m * 2^e) = log10(m) + e * log10(2). Both terms use f64, so the
    // result is only an estimate; correctness comes from the caller's check.
    #[expect(
        clippy::cast_precision_loss,
        reason = "the logarithm is only an estimate that the caller verifies and corrects"
    )]
    let magnitude = (significand as f64).log10() + f64::from(exponent) * core::f64::consts::LOG10_2;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the estimate is bounded by the binary64 exponent range"
    )]
    let estimate = magnitude.floor() as i32;
    estimate
}

/// Computes `round(significand * 2^exponent * 10^scale)` exactly.
///
/// Rounding is half away from zero. Every operation is integer arithmetic, so
/// the result reflects the stored value rather than any decimal approximation.
fn scale_and_round(significand: u64, exponent: i32, scale: i64) -> Result<JsBigInt, BigIntError> {
    let mut numerator = JsBigInt::from_u64(significand);
    let mut denominator = JsBigInt::from_i32(1);
    let ten = JsBigInt::from_i32(10);

    // A positive scale multiplies by a power of ten; a negative one divides.
    match u64::try_from(scale.abs()) {
        Ok(magnitude) => {
            let power = ten.pow(&JsBigInt::from_u64(magnitude))?;
            if scale >= 0 {
                numerator = numerator.mul(&power)?;
            } else {
                denominator = denominator.mul(&power)?;
            }
        }
        Err(_) => return Err(BigIntError::ResultTooLarge),
    }

    // A positive binary exponent multiplies; a negative one divides.
    let shift = u64::try_from(exponent.abs()).map_err(|_| BigIntError::ResultTooLarge)?;
    if exponent >= 0 {
        numerator = numerator.shl(shift)?;
    } else {
        denominator = denominator.shl(shift)?;
    }

    let (quotient, remainder) = numerator.div_rem(&denominator)?;
    // Round half away from zero: double the remainder and compare against the
    // denominator, which avoids any fractional arithmetic.
    let doubled = remainder.shl(1)?;
    if doubled.compare(&denominator) != core::cmp::Ordering::Less {
        return quotient.add(&JsBigInt::from_i32(1));
    }
    Ok(quotient)
}

#[cfg(test)]
mod tests {
    use super::{exact_fixed, exact_significant};

    /// `toFixed` rounds the exact stored value, not its shortest spelling.
    ///
    /// This is the whole reason the module uses exact integer arithmetic. The
    /// pinned oracle:
    ///
    /// ```console
    /// $ /private/tmp/quickjs-2026-06-04/qjs -e 'console.log((1.005).toFixed(2),\
    ///     (1.55).toFixed(1), (8.575).toFixed(2), (9.995).toFixed(2));'
    /// 1.00 1.6 8.57 9.99
    /// ```
    #[test]
    fn fixed_rounding_follows_the_exact_stored_value() {
        for (value, digits, expected) in [
            // Stored just below 1.005, so it rounds down.
            (1.005_f64, 2, "1.00"),
            // Stored just above 1.55, so it rounds up.
            (1.55, 1, "1.6"),
            (1.45, 1, "1.4"),
            (8.575, 2, "8.57"),
            (9.995, 2, "9.99"),
            (-9.995, 2, "-9.99"),
            // An exact tie rounds away from zero.
            (1.5, 0, "2"),
            (2.5, 0, "3"),
            (-1.5, 0, "-2"),
            (0.5, 0, "1"),
            (1.25, 1, "1.3"),
            (1.35, 1, "1.4"),
            (0.0, 2, "0.00"),
            // Negative zero renders without a sign.
            (-0.0, 2, "0.00"),
            (1e-7, 2, "0.00"),
            (0.000_001, 2, "0.00"),
            (1.999, 2, "2.00"),
            (123.456, 0, "123"),
            (1.0, 0, "1"),
        ] {
            assert_eq!(
                exact_fixed(value, digits).expect("exact fixed rendering"),
                expected,
                "({value}).toFixed({digits})"
            );
        }
    }

    /// The maximum digit count renders without loss.
    #[test]
    fn fixed_rendering_supports_one_hundred_digits() {
        let rendered = exact_fixed(1.0, 100).expect("exact fixed rendering");
        assert_eq!(rendered.len(), 102, "one digit, a point, and 100 zeroes");
        assert!(rendered.starts_with("1."));
        assert!(rendered[2..].bytes().all(|byte| byte == b'0'));
    }

    /// The significant digits and exponent match the oracle's spellings.
    ///
    /// ```console
    /// $ /private/tmp/quickjs-2026-06-04/qjs -e 'console.log((123.456).toExponential(2),\
    ///     (123.456).toPrecision(4), (12345).toPrecision(2));'
    /// 1.23e+2 123.5 1.2e+4
    /// ```
    #[test]
    fn significant_digits_match_the_oracle() {
        for (value, precision, digits, exponent) in [
            (123.456_f64, 3, "123", 2),
            (123.456, 4, "1235", 2),
            (123.456, 6, "123456", 2),
            (1.0, 1, "1", 0),
            (12345.0, 2, "12", 4),
            (0.000_001, 2, "10", -6),
            (1e21, 3, "100", 21),
            (1e-7, 4, "1000", -7),
            // A tie rounds away from zero here too.
            (1.5, 1, "2", 0),
            (2.5, 1, "3", 0),
            (-1.5, 1, "2", 0),
        ] {
            let rendered = exact_significant(value, precision).expect("exact significant digits");
            assert_eq!(
                (rendered.digits.as_str(), rendered.exponent),
                (digits, exponent),
                "({value}) with precision {precision}"
            );
        }
    }

    /// Zero reports its precision as zeroes with a zero exponent.
    #[test]
    fn significant_digits_of_zero_are_zeroes() {
        let rendered = exact_significant(0.0, 3).expect("exact significant digits");
        assert_eq!(rendered.digits, "000");
        assert_eq!(rendered.exponent, 0);
        assert!(!rendered.negative);

        let negative = exact_significant(-0.0, 1).expect("exact significant digits");
        assert_eq!(negative.digits, "0");
        assert!(negative.negative, "the sign is reported and dropped later");
    }
}
