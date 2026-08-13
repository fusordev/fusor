/*
 * Binary64 radix formatting derived from QuickJS dtoa.c.
 *
 * Copyright (c) 2024 Fabrice Bellard
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

use crate::string::{JsString, JsStringError};

use super::{FallibleAsciiBuffer, JAVASCRIPT_NUMBER_INLINE_BYTES};

const RADIX_DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const LIMB_BITS: usize = 32;
const RADIX_SCRATCH_LIMBS: usize = 52;
const MAX_SIGNIFICANT_DIGITS: usize = 54;

const DIGITS_PER_LIMB: [u8; 35] = [
    32, 20, 16, 13, 12, 11, 10, 10, 9, 9, 8, 8, 8, 8, 8, 7, 7, 7, 7, 7, 7, 7, 6, 6, 6, 6, 6, 6, 6,
    6, 6, 6, 6, 6, 6,
];

const MAX_FREE_DIGITS: [u8; 35] = [
    54, 35, 28, 24, 22, 20, 19, 18, 17, 17, 16, 16, 15, 15, 15, 14, 14, 14, 14, 14, 13, 13, 13, 13,
    13, 13, 13, 12, 12, 12, 12, 12, 12, 12, 12,
];

const MUL_LOG2_RADIX: [u32; 35] = [
    0x0000_0000,
    0x00a1_849d,
    0x0000_0000,
    0x006e_40d2,
    0x0063_08c9,
    0x005b_3065,
    0x0000_0000,
    0x0050_c24e,
    0x004d_104d,
    0x004a_0027,
    0x0047_68ce,
    0x0045_2e54,
    0x0043_3d00,
    0x0041_8677,
    0x0000_0000,
    0x003e_a16b,
    0x003d_645a,
    0x003c_43c2,
    0x003b_3b9a,
    0x003a_4899,
    0x0039_680b,
    0x0038_97b3,
    0x0037_d5af,
    0x0037_2069,
    0x0036_7686,
    0x0035_d6df,
    0x0035_4072,
    0x0034_b261,
    0x0034_2bea,
    0x0033_ac62,
    0x0000_0000,
    0x0032_bfd9,
    0x0032_51dd,
    0x0031_e8d6,
    0x0031_8465,
];

pub(super) fn format_i32_radix(value: i32, radix: u32) -> Result<JsString, JsStringError> {
    let mut output = FallibleAsciiBuffer::<JAVASCRIPT_NUMBER_INLINE_BYTES>::new();
    let magnitude = if value < 0 {
        output.push_byte(b'-')?;
        u64::from(value.unsigned_abs())
    } else {
        u64::try_from(value).expect("a nonnegative i32 fits u64")
    };
    push_minimal_digits(&mut output, magnitude, radix)?;
    output.into_js_string()
}

pub(super) fn format_binary64_radix(value: f64, radix: u32) -> Result<JsString, JsStringError> {
    debug_assert!((2..=36).contains(&radix) && radix != 10);
    let bits = value.to_bits();
    let absolute_bits = bits & 0x7fff_ffff_ffff_ffff;
    if let Some(canonical) = canonical_special_value(bits, absolute_bits) {
        return JsString::from_latin1(canonical);
    }

    let mut exponent = i32::from(
        u16::try_from((absolute_bits >> 52) & 0x7ff).expect("binary64 exponent fits u16"),
    );
    let mut mantissa = absolute_bits & ((1_u64 << 52) - 1);
    if exponent == 0 {
        let normalization = i32::try_from(mantissa.leading_zeros()).expect("u32 fits i32") - 11;
        exponent -= normalization - 1;
        mantissa <<= u32::try_from(normalization).expect("normalization is nonnegative");
    } else {
        mantissa |= 1_u64 << 52;
    }
    exponent -= 1022;

    let mut output = FallibleAsciiBuffer::<JAVASCRIPT_NUMBER_INLINE_BYTES>::new();
    if bits >> 63 != 0 {
        output.push_byte(b'-')?;
    }

    if (1..=53).contains(&exponent) {
        let fractional_bits =
            u32::try_from(53 - exponent).expect("fast integer shift is nonnegative");
        let fractional_mask = if fractional_bits == 0 {
            0
        } else {
            (1_u64 << fractional_bits) - 1
        };
        if mantissa & fractional_mask == 0 {
            push_minimal_digits(&mut output, mantissa >> fractional_bits, radix)?;
            return output.into_js_string();
        }
    }

    let radix_shift = i32::try_from(radix.trailing_zeros()).expect("radix trailing zeros fit i32");
    let odd_radix = radix >> u32::try_from(radix_shift).expect("radix shift is nonnegative");
    let initial_digit_exponent = 1 + floor_binary_exponent_in_radix(exponent - 1, radix);
    let mut digit_count = i32::from(MAX_FREE_DIGITS[radix_index(radix)]);
    let mut found_digit_count = 0_i32;
    let mut found_digit_exponent = 0_i32;
    let mut found_mantissa = 0_u64;

    loop {
        let maximum_mantissa = pow_u64(
            u64::from(radix),
            u32::try_from(digit_count).expect("digit count is positive"),
        );
        let mut digit_exponent = initial_digit_exponent;
        let candidate = loop {
            let candidate = RadixBig::rounded_product(
                mantissa,
                exponent - 53,
                odd_radix,
                radix_shift,
                digit_count - digit_exponent,
            )
            .to_u64();
            if candidate < maximum_mantissa {
                break candidate;
            }
            digit_exponent += 1;
        };

        let mut candidate = candidate;
        while candidate % u64::from(radix) == 0 {
            candidate /= u64::from(radix);
            digit_count -= 1;
        }

        if found_digit_count != 0 {
            let (round_trip_mantissa, round_trip_exponent) = RadixBig::from_u64(candidate)
                .rounded_to_binary64(odd_radix, radix_shift, digit_exponent - digit_count);
            if round_trip_mantissa != mantissa || round_trip_exponent != exponent {
                break;
            }
        }

        found_digit_count = digit_count;
        found_digit_exponent = digit_exponent;
        found_mantissa = candidate;
        if digit_count == 1 {
            break;
        }
        digit_count -= 1;
    }

    write_fixed_free_format(
        &mut output,
        found_mantissa,
        radix,
        found_digit_count,
        found_digit_exponent,
    )?;
    output.into_js_string()
}

fn canonical_special_value(bits: u64, absolute_bits: u64) -> Option<&'static [u8]> {
    if absolute_bits > 0x7ff0_0000_0000_0000 {
        Some(b"NaN")
    } else if absolute_bits == 0x7ff0_0000_0000_0000 {
        if bits >> 63 == 0 {
            Some(b"Infinity")
        } else {
            Some(b"-Infinity")
        }
    } else if absolute_bits == 0 {
        Some(b"0")
    } else {
        None
    }
}

fn write_fixed_free_format<const INLINE_BYTES: usize>(
    output: &mut FallibleAsciiBuffer<INLINE_BYTES>,
    mantissa: u64,
    radix: u32,
    digit_count: i32,
    digit_exponent: i32,
) -> Result<(), JsStringError> {
    let digit_count = usize::try_from(digit_count).expect("digit count is positive");
    let digit_storage = padded_digits(mantissa, radix, digit_count);
    let digits = &digit_storage[..digit_count];
    if digit_exponent <= 0 {
        output.push_bytes(b"0.")?;
        output.push_repeated(
            b'0',
            usize::try_from(-digit_exponent).expect("negative exponent magnitude fits usize"),
        )?;
        return output.push_bytes(digits);
    }

    let digit_exponent =
        usize::try_from(digit_exponent).expect("positive digit exponent fits usize");
    if digit_exponent < digit_count {
        output.push_bytes(&digits[..digit_exponent])?;
        output.push_byte(b'.')?;
        output.push_bytes(&digits[digit_exponent..])
    } else {
        output.push_bytes(digits)?;
        output.push_repeated(b'0', digit_exponent - digit_count)
    }
}

fn push_minimal_digits<const INLINE_BYTES: usize>(
    output: &mut FallibleAsciiBuffer<INLINE_BYTES>,
    mut value: u64,
    radix: u32,
) -> Result<(), JsStringError> {
    let mut storage = [b'0'; 64];
    let mut start = storage.len();
    loop {
        start -= 1;
        let digit = usize::try_from(value % u64::from(radix)).expect("digit fits usize");
        storage[start] = RADIX_DIGITS[digit];
        value /= u64::from(radix);
        if value == 0 {
            return output.push_bytes(&storage[start..]);
        }
    }
}

fn padded_digits(mantissa: u64, radix: u32, width: usize) -> [u8; MAX_SIGNIFICANT_DIGITS] {
    debug_assert!((1..=MAX_SIGNIFICANT_DIGITS).contains(&width));
    let mut storage = [b'0'; MAX_SIGNIFICANT_DIGITS];
    let mut value = mantissa;
    for position in (MAX_SIGNIFICANT_DIGITS - width..MAX_SIGNIFICANT_DIGITS).rev() {
        let digit = usize::try_from(value % u64::from(radix)).expect("digit fits usize");
        storage[position] = RADIX_DIGITS[digit];
        value /= u64::from(radix);
    }
    debug_assert_eq!(value, 0);
    storage.rotate_left(MAX_SIGNIFICANT_DIGITS - width);
    storage
}

fn floor_binary_exponent_in_radix(mut exponent: i32, radix: u32) -> i32 {
    if radix.is_power_of_two() {
        let radix_bits = i32::try_from(radix.ilog2()).expect("radix width fits i32");
        if exponent < 0 {
            exponent -= radix_bits - 1;
        }
        exponent / radix_bits
    } else {
        let multiplier = i64::from(MUL_LOG2_RADIX[radix_index(radix)]);
        i32::try_from((i64::from(exponent) * multiplier) >> 24)
            .expect("radix exponent estimate fits i32")
    }
}

fn radix_index(radix: u32) -> usize {
    usize::try_from(radix - 2).expect("validated radix index fits usize")
}

fn pow_u64(mut base: u64, mut exponent: u32) -> u64 {
    let mut result = 1_u64;
    while exponent != 0 {
        if exponent & 1 != 0 {
            result = result
                .checked_mul(base)
                .expect("free-format mantissa bound fits u64");
        }
        exponent >>= 1;
        if exponent != 0 {
            base = base
                .checked_mul(base)
                .expect("free-format mantissa bound fits u64");
        }
    }
    result
}

fn pow_small(base: u32, exponent: i32) -> u32 {
    u32::try_from(pow_u64(
        u64::from(base),
        u32::try_from(exponent).expect("small power exponent is nonnegative"),
    ))
    .expect("digits-per-limb table keeps the power within u32")
}

#[derive(Clone, Copy)]
enum RoundingMode {
    NearestEven,
    TowardZero,
}

struct RadixBig {
    limbs: [u32; RADIX_SCRATCH_LIMBS],
    len: usize,
}

impl RadixBig {
    fn from_u64(value: u64) -> Self {
        let mut result = Self {
            limbs: [0; RADIX_SCRATCH_LIMBS],
            len: 1,
        };
        result.limbs[0] = u32::try_from(value & u64::from(u32::MAX)).expect("low half fits u32");
        let high = u32::try_from(value >> LIMB_BITS).expect("high half fits u32");
        if high != 0 {
            result.limbs[1] = high;
            result.len = 2;
        }
        result
    }

    fn to_u64(&self) -> u64 {
        assert!(self.len <= 2, "rounded mantissa must fit u64");
        let high = if self.len == 2 { self.limbs[1] } else { 0 };
        u64::from(self.limbs[0]) | (u64::from(high) << LIMB_BITS)
    }

    fn rounded_product(
        mantissa: u64,
        binary_exponent: i32,
        odd_radix: u32,
        radix_shift: i32,
        radix_exponent: i32,
    ) -> Self {
        let mut result = Self::from_u64(mantissa);
        let exponent_offset = result.multiply_radix_power(
            odd_radix,
            radix_shift,
            radix_exponent,
            true,
            binary_exponent,
        );
        result.shift_round(
            -binary_exponent + exponent_offset,
            RoundingMode::NearestEven,
        );
        result
    }

    fn rounded_to_binary64(
        mut self,
        odd_radix: u32,
        radix_shift: i32,
        radix_exponent: i32,
    ) -> (u64, i32) {
        let exponent_offset =
            self.multiply_radix_power(odd_radix, radix_shift, radix_exponent, false, 55);
        self.round_to_binary64(exponent_offset)
    }

    fn multiply_radix_power(
        &mut self,
        odd_radix: u32,
        radix_shift: i32,
        radix_exponent: i32,
        is_integer: bool,
        required_bits: i32,
    ) -> i32 {
        let mut exponent_offset = -radix_exponent * radix_shift;
        if odd_radix == 1 {
            return exponent_offset;
        }

        let digits_per_limb = i32::from(DIGITS_PER_LIMB[radix_index(odd_radix)]);
        if radix_exponent >= 0 {
            let mut remaining = radix_exponent;
            while remaining != 0 {
                let digits = remaining.min(digits_per_limb);
                self.multiply_small(pow_small(odd_radix, digits));
                remaining -= digits;
            }
            return exponent_offset;
        }

        let mut remaining = -radix_exponent;
        let division_limbs = (remaining + digits_per_limb - 1) / digits_per_limb;
        exponent_offset += division_limbs * i32::try_from(LIMB_BITS).expect("limb width fits i32");
        let extra_bits = if is_integer {
            (2 + required_bits - exponent_offset).max(0)
        } else {
            (required_bits - self.floor_log2()).max(0)
        };
        exponent_offset += extra_bits;
        self.shift_round(
            -(division_limbs * i32::try_from(LIMB_BITS).expect("limb width fits i32") + extra_bits),
            RoundingMode::TowardZero,
        );

        let mut has_remainder = false;
        while remaining != 0 {
            let digits = remaining.min(digits_per_limb);
            has_remainder |= self.divide_small(pow_small(odd_radix, digits)) != 0;
            remaining -= digits;
        }
        if has_remainder {
            self.limbs[0] |= 1;
        }
        exponent_offset
    }

    fn round_to_binary64(&mut self, exponent_offset: i32) -> (u64, i32) {
        if self.is_zero() {
            return (0, 0);
        }
        let mut exponent = self.floor_log2() + 1 - exponent_offset;
        if exponent < -1074 {
            return (0, 0);
        }
        let precision = if exponent < -1021 {
            53 - (-1021 - exponent)
        } else {
            53
        };
        debug_assert!((0..=53).contains(&precision));
        self.shift_round(
            exponent + exponent_offset - precision,
            RoundingMode::NearestEven,
        );
        let mut mantissa = self.to_u64();
        mantissa <<= u32::try_from(53 - precision).expect("precision is at most 53");
        if mantissa >= 1_u64 << 53 {
            mantissa >>= 1;
            exponent += 1;
        }
        (mantissa, exponent)
    }

    fn multiply_small(&mut self, multiplier: u32) {
        let mut carry = 0_u64;
        for limb in &mut self.limbs[..self.len] {
            let product = u64::from(*limb) * u64::from(multiplier) + carry;
            *limb = u32::try_from(product & u64::from(u32::MAX)).expect("low half fits u32");
            carry = product >> LIMB_BITS;
        }
        if carry != 0 {
            self.push_limb(u32::try_from(carry).expect("limb carry fits u32"));
        }
    }

    fn divide_small(&mut self, divisor: u32) -> u32 {
        let mut remainder = 0_u64;
        for index in (0..self.len).rev() {
            let dividend = (remainder << LIMB_BITS) | u64::from(self.limbs[index]);
            self.limbs[index] =
                u32::try_from(dividend / u64::from(divisor)).expect("quotient limb fits u32");
            remainder = dividend % u64::from(divisor);
        }
        self.renormalize();
        u32::try_from(remainder).expect("remainder fits u32")
    }

    fn shift_round(&mut self, shift: i32, mode: RoundingMode) {
        if shift == 0 {
            return;
        }
        if shift < 0 {
            self.shift_left(
                usize::try_from(shift.unsigned_abs()).expect("shift magnitude fits usize"),
            );
            return;
        }

        let shift = usize::try_from(shift).expect("positive shift fits usize");
        let halfway = self.bit(shift - 1);
        let add_one = halfway
            && matches!(mode, RoundingMode::NearestEven)
            && (self.any_bits_below(shift - 1) || self.bit(shift));
        self.shift_right(shift);
        if add_one {
            self.add_one();
        }
    }

    fn shift_left(&mut self, shift: usize) {
        let whole_limbs = shift / LIMB_BITS;
        let partial = shift % LIMB_BITS;
        if partial != 0 {
            let mut carry = 0_u64;
            for limb in &mut self.limbs[..self.len] {
                let shifted = (u64::from(*limb) << partial) | carry;
                *limb = u32::try_from(shifted & u64::from(u32::MAX)).expect("low half fits u32");
                carry = shifted >> LIMB_BITS;
            }
            if carry != 0 {
                self.push_limb(u32::try_from(carry).expect("limb carry fits u32"));
            }
        }
        if whole_limbs != 0 {
            assert!(
                self.len + whole_limbs <= RADIX_SCRATCH_LIMBS,
                "QuickJS radix scratch bound must cover every binary64: len={}, whole_limbs={}, shift={shift}",
                self.len,
                whole_limbs
            );
            self.limbs.copy_within(0..self.len, whole_limbs);
            self.limbs[..whole_limbs].fill(0);
            self.len += whole_limbs;
        }
    }

    fn shift_right(&mut self, shift: usize) {
        let whole_limbs = shift / LIMB_BITS;
        let partial = shift % LIMB_BITS;
        if whole_limbs >= self.len {
            self.len = 1;
            self.limbs[0] = 0;
            return;
        }
        if whole_limbs != 0 {
            self.limbs.copy_within(whole_limbs..self.len, 0);
            self.len -= whole_limbs;
        }
        if partial != 0 {
            let mut high = 0_u32;
            for index in (0..self.len).rev() {
                let current = self.limbs[index];
                self.limbs[index] = (current >> partial) | (high << (LIMB_BITS - partial));
                high = current;
            }
        }
        self.renormalize();
    }

    fn add_one(&mut self) {
        for index in 0..self.len {
            let (value, carry) = self.limbs[index].overflowing_add(1);
            self.limbs[index] = value;
            if !carry {
                return;
            }
        }
        self.push_limb(1);
    }

    fn bit(&self, index: usize) -> bool {
        let limb = index / LIMB_BITS;
        let bit = index % LIMB_BITS;
        self.limbs
            .get(limb)
            .is_some_and(|value| value & (1_u32 << bit) != 0)
    }

    fn any_bits_below(&self, exclusive: usize) -> bool {
        let whole_limbs = exclusive / LIMB_BITS;
        if self.limbs[..whole_limbs.min(self.len)]
            .iter()
            .any(|limb| *limb != 0)
        {
            return true;
        }
        let partial = exclusive % LIMB_BITS;
        partial != 0
            && self
                .limbs
                .get(whole_limbs)
                .is_some_and(|limb| limb & ((1_u32 << partial) - 1) != 0)
    }

    fn floor_log2(&self) -> i32 {
        if self.is_zero() {
            return -1;
        }
        let high = self.limbs[self.len - 1];
        i32::try_from(self.len * LIMB_BITS - 1).expect("scratch bit count fits i32")
            - i32::try_from(high.leading_zeros()).expect("leading zeros fit i32")
    }

    fn is_zero(&self) -> bool {
        self.len == 1 && self.limbs[0] == 0
    }

    fn renormalize(&mut self) {
        while self.len > 1 && self.limbs[self.len - 1] == 0 {
            self.len -= 1;
        }
    }

    fn push_limb(&mut self, value: u32) {
        let slot = self
            .limbs
            .get_mut(self.len)
            .expect("QuickJS radix scratch bound must cover every binary64");
        *slot = value;
        self.len += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{format_binary64_radix, format_i32_radix};

    #[test]
    fn integer_radix_formatting_uses_lowercase_digits_and_preserves_sign() {
        for (value, radix, expected) in [
            (0, 2, "0"),
            (255, 2, "11111111"),
            (255, 8, "377"),
            (255, 16, "ff"),
            (255, 36, "73"),
            (i32::MIN, 16, "-80000000"),
        ] {
            assert_eq!(
                format_i32_radix(value, radix)
                    .expect("integer radix string")
                    .to_utf8_lossy()
                    .expect("ASCII"),
                expected
            );
        }
    }

    #[test]
    fn free_format_matches_pinned_quickjs_rounding_vectors() {
        for (value, radix, expected) in [
            (
                0.1,
                2,
                "0.0001100110011001100110011001100110011001100110011001101",
            ),
            (0.1, 16, "0.1999999999999a"),
            (0.1, 36, "0.3lllllllllm"),
            (1.0 / 3.0, 3, "0.1"),
            (1.0 / 3.0, 16, "0.55555555555554"),
            (1.0 - 2_f64.powi(-53), 12, "0.bbbbbbbbbbbbbba"),
            (1.3, 7, "1.2046204620462046205"),
            (1.3, 35, "1.ahhhhhhhhhm"),
            (2_147_483_648.0, 36, "zik0zk"),
            (0.999_999_999_999_999_9, 16, "0.fffffffffffff8"),
            (1.000_000_000_000_000_2, 16, "1.0000000000001"),
            (9_007_199_254_740_991.0, 16, "1fffffffffffff"),
        ] {
            assert_eq!(
                format_binary64_radix(value, radix)
                    .expect("binary64 radix string")
                    .to_utf8_lossy()
                    .expect("ASCII"),
                expected,
                "{value:?} base {radix}"
            );
        }
    }

    #[test]
    fn free_format_covers_subnormal_and_maximum_output_bounds() {
        let minimum = format_binary64_radix(f64::from_bits(1), 2)
            .expect("minimum binary64")
            .to_utf8_lossy()
            .expect("ASCII");
        assert_eq!(minimum.len(), 1_076);
        assert!(minimum.starts_with("0."));
        assert!(
            minimum[2..minimum.len() - 1]
                .bytes()
                .all(|byte| byte == b'0')
        );
        assert!(minimum.ends_with('1'));

        let odd_radix_minimum = format_binary64_radix(f64::from_bits(1), 3)
            .expect("minimum binary64 in odd radix")
            .to_utf8_lossy()
            .expect("ASCII");
        assert_eq!(odd_radix_minimum.len(), 680);
        assert!(odd_radix_minimum.starts_with("0."));
        assert!(
            odd_radix_minimum[2..odd_radix_minimum.len() - 1]
                .bytes()
                .all(|byte| byte == b'0')
        );
        assert!(odd_radix_minimum.ends_with('2'));

        let maximum = format_binary64_radix(f64::MAX, 2)
            .expect("maximum binary64")
            .to_utf8_lossy()
            .expect("ASCII");
        assert_eq!(maximum.len(), 1_024);
        assert!(maximum[..53].bytes().all(|byte| byte == b'1'));
        assert!(maximum[53..].bytes().all(|byte| byte == b'0'));
    }

    #[test]
    fn free_format_canonicalizes_special_values_and_negative_zero() {
        for (value, radix, expected) in [
            (-0.0, 2, "0"),
            (f64::NAN, 36, "NaN"),
            (f64::INFINITY, 2, "Infinity"),
            (f64::NEG_INFINITY, 36, "-Infinity"),
        ] {
            assert_eq!(
                format_binary64_radix(value, radix)
                    .expect("special radix string")
                    .to_utf8_lossy()
                    .expect("ASCII"),
                expected
            );
        }
    }
}
