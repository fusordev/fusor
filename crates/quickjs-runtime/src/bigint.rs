/*
 * JavaScript BigInt arithmetic derived from QuickJS.
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

//! Arbitrary-precision integers for the ECMAScript `BigInt` domain.
//!
//! The representation mirrors the pinned `JSBigInt` (`quickjs.c:490-495`): a
//! two's-complement value held in 32-bit limbs, least significant first, always
//! normalized so the limb count is the minimum that preserves the sign. The
//! most significant limb's top bit is therefore the sign bit, and `0n` is the
//! single limb `0`.
//!
//! Keeping two's complement rather than sign-and-magnitude is deliberate: the
//! bitwise operators, `asIntN`, and `asUintN` are all defined on the infinite
//! two's-complement expansion, so this representation makes them limb-local
//! instead of requiring a sign fixup pass.
//!
//! The module is self-contained so it can be tested without a runtime; the
//! caller maps [`BigIntError`] onto the pinned exception messages.

use std::{cmp::Ordering, collections::TryReserveError};

/// Bits per limb, matching `JS_LIMB_BITS` for the 32-bit configuration.
const LIMB_BITS: u32 = 32;

/// Maximum limb count, matching `JS_BIGINT_MAX_SIZE` (`quickjs.c:11266`):
/// one mebibit of value.
const MAX_LIMBS: usize = (1024 * 1024) / LIMB_BITS as usize;

/// A failure while producing a `BigInt`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BigIntError {
    /// The result needs more limbs than the pinned engine permits.
    ///
    /// The caller reports `RangeError: BigInt is too large to allocate`
    /// (`quickjs.c:11595` and `quickjs.c:12472`).
    TooLarge,
    /// A shift or exponent would produce a value beyond the limb cap.
    ///
    /// The caller reports `RangeError: BigInt is too large`
    /// (`quickjs.c:12184`).
    ResultTooLarge,
    /// Backing storage could not be reserved.
    AllocationFailed,
    /// A conversion source was not an integer.
    ///
    /// The caller reports
    /// `RangeError: cannot convert to BigInt: not an integer`.
    NotAnInteger,
    /// A conversion source was `NaN` or an infinity.
    ///
    /// The caller reports
    /// `RangeError: cannot convert NaN or Infinity to BigInt`.
    NotFinite,
    /// A string was not a valid `BigInt` literal.
    ///
    /// The caller reports `SyntaxError: invalid bigint literal`.
    InvalidLiteral,
    /// A division or remainder had a zero divisor.
    ///
    /// The caller reports `RangeError: division by zero`.
    DivisionByZero,
    /// An exponentiation had a negative exponent.
    ///
    /// The caller reports `RangeError: exponent must be non-negative`.
    NegativeExponent,
    /// A radix fell outside `2..=36`.
    InvalidRadix,
}

impl From<TryReserveError> for BigIntError {
    fn from(_: TryReserveError) -> Self {
        Self::AllocationFailed
    }
}

/// An arbitrary-precision integer in the ECMAScript `BigInt` domain.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct JsBigInt {
    /// Two's-complement limbs, least significant first, always normalized.
    limbs: Vec<u32>,
}

impl JsBigInt {
    /// Returns `0n`.
    #[must_use]
    pub(crate) fn zero() -> Self {
        Self { limbs: vec![0] }
    }

    /// Returns whether the value is zero.
    #[must_use]
    pub(crate) fn is_zero(&self) -> bool {
        self.limbs.len() == 1 && self.limbs[0] == 0
    }

    /// Returns whether the value is negative.
    ///
    /// The sign lives in the top bit of the most significant limb, which the
    /// normalization invariant keeps meaningful.
    #[must_use]
    pub(crate) fn is_negative(&self) -> bool {
        self.top_limb() >> (LIMB_BITS - 1) == 1
    }

    /// Returns the most significant limb.
    fn top_limb(&self) -> u32 {
        *self
            .limbs
            .last()
            .expect("a normalized BigInt always has at least one limb")
    }

    /// Returns the limb used to extend the value leftwards indefinitely.
    ///
    /// Sign extension repeats `0xffff_ffff` for a negative value and `0` for a
    /// non-negative one, which is what makes the limb operations below valid on
    /// operands of different lengths.
    fn sign_extension(&self) -> u32 {
        if self.is_negative() { u32::MAX } else { 0 }
    }

    /// Returns limb `index`, sign-extending past the end.
    fn limb(&self, index: usize) -> u32 {
        self.limbs
            .get(index)
            .copied()
            .unwrap_or_else(|| self.sign_extension())
    }

    /// Builds a value from raw limbs, normalizing them.
    fn from_limbs(mut limbs: Vec<u32>) -> Result<Self, BigIntError> {
        if limbs.is_empty() {
            limbs.push(0);
        }
        normalize(&mut limbs);
        if limbs.len() > MAX_LIMBS {
            return Err(BigIntError::TooLarge);
        }
        Ok(Self { limbs })
    }

    /// Creates a value from a signed 32-bit integer.
    #[must_use]
    pub(crate) fn from_i32(value: i32) -> Self {
        Self {
            limbs: vec![value.cast_unsigned()],
        }
    }

    /// Creates a value from a signed 64-bit integer.
    #[must_use]
    pub(crate) fn from_i64(value: i64) -> Self {
        let bits = value.cast_unsigned();
        let low = u32::try_from(bits & u64::from(u32::MAX)).expect("masked to 32 bits");
        let high = u32::try_from(bits >> LIMB_BITS).expect("shifted to 32 bits");
        let mut limbs = vec![low, high];
        normalize(&mut limbs);
        Self { limbs }
    }

    /// Creates a value from an unsigned 64-bit integer.
    #[must_use]
    pub(crate) fn from_u64(value: u64) -> Self {
        let low = u32::try_from(value & u64::from(u32::MAX)).expect("masked to 32 bits");
        let high = u32::try_from(value >> LIMB_BITS).expect("shifted to 32 bits");
        // A third zero limb keeps a value with the high bit set non-negative.
        let mut limbs = vec![low, high, 0];
        normalize(&mut limbs);
        Self { limbs }
    }

    /// Returns the value as an `i64` when it fits.
    #[must_use]
    pub(crate) fn to_i64(&self) -> Option<i64> {
        if self.limbs.len() > 2 {
            return None;
        }
        let low = u64::from(self.limb(0));
        let high = u64::from(self.limb(1));
        let bits = low | (high << LIMB_BITS);
        let value = bits.cast_signed();
        // Round-tripping proves the two's-complement value survived the width.
        (Self::from_i64(value) == *self).then_some(value)
    }

    /// Returns the value as a `u64` when it fits and is non-negative.
    #[must_use]
    pub(crate) fn to_u64(&self) -> Option<u64> {
        if self.is_negative() || self.limbs.len() > 3 {
            return None;
        }
        if self.limbs.len() == 3 && self.limb(2) != 0 {
            return None;
        }
        let low = u64::from(self.limb(0));
        let high = u64::from(self.limb(1));
        Some(low | (high << LIMB_BITS))
    }

    /// Returns the two's-complement negation.
    pub(crate) fn neg(&self) -> Result<Self, BigIntError> {
        // Negation is `!value + 1`, computed with one extra limb so the most
        // negative value of a width still widens correctly.
        let mut limbs = Vec::new();
        limbs.try_reserve_exact(self.limbs.len() + 1)?;
        let mut carry = 1_u64;
        for index in 0..=self.limbs.len() {
            let inverted = u64::from(!self.limb(index));
            let sum = inverted + carry;
            limbs.push(u32::try_from(sum & u64::from(u32::MAX)).expect("masked to 32 bits"));
            carry = sum >> LIMB_BITS;
        }
        Self::from_limbs(limbs)
    }

    /// Returns the absolute value.
    pub(crate) fn abs(&self) -> Result<Self, BigIntError> {
        if self.is_negative() {
            self.neg()
        } else {
            Ok(self.clone())
        }
    }

    /// Returns the sum.
    pub(crate) fn add(&self, other: &Self) -> Result<Self, BigIntError> {
        self.add_with_sign(other, false)
    }

    /// Returns the difference.
    pub(crate) fn sub(&self, other: &Self) -> Result<Self, BigIntError> {
        self.add_with_sign(other, true)
    }

    /// Adds, optionally negating the right operand first.
    ///
    /// Subtraction is folded in here because two's-complement addition and
    /// subtraction differ only by inverting the addend and seeding the carry,
    /// so one loop serves both and cannot drift apart.
    fn add_with_sign(&self, other: &Self, subtract: bool) -> Result<Self, BigIntError> {
        let width = self.limbs.len().max(other.limbs.len()) + 1;
        let mut limbs = Vec::new();
        limbs.try_reserve_exact(width)?;
        let mut carry = u64::from(subtract);
        for index in 0..width {
            let left = u64::from(self.limb(index));
            let right = u64::from(if subtract {
                !other.limb(index)
            } else {
                other.limb(index)
            });
            let sum = left + right + carry;
            limbs.push(u32::try_from(sum & u64::from(u32::MAX)).expect("masked to 32 bits"));
            carry = sum >> LIMB_BITS;
        }
        Self::from_limbs(limbs)
    }

    /// Returns the product.
    pub(crate) fn mul(&self, other: &Self) -> Result<Self, BigIntError> {
        if self.is_zero() || other.is_zero() {
            return Ok(Self::zero());
        }
        // Multiply magnitudes, then apply the sign, so the inner loop never has
        // to reason about two's-complement borrow.
        let negative = self.is_negative() != other.is_negative();
        let left = self.magnitude()?;
        let right = other.magnitude()?;
        let mut product = vec![0_u32; left.len() + right.len() + 1];
        for (left_index, &left_limb) in left.iter().enumerate() {
            if left_limb == 0 {
                continue;
            }
            let mut carry = 0_u64;
            for (right_index, &right_limb) in right.iter().enumerate() {
                let position = left_index + right_index;
                let current = u64::from(product[position]);
                let sum = current + u64::from(left_limb) * u64::from(right_limb) + carry;
                product[position] =
                    u32::try_from(sum & u64::from(u32::MAX)).expect("masked to 32 bits");
                carry = sum >> LIMB_BITS;
            }
            let mut position = left_index + right.len();
            while carry != 0 {
                let sum = u64::from(product[position]) + carry;
                product[position] =
                    u32::try_from(sum & u64::from(u32::MAX)).expect("masked to 32 bits");
                carry = sum >> LIMB_BITS;
                position += 1;
            }
        }
        let value = Self::from_limbs(product)?;
        if negative { value.neg() } else { Ok(value) }
    }

    /// Returns the non-negative magnitude limbs, without a sign limb.
    fn magnitude(&self) -> Result<Vec<u32>, BigIntError> {
        let value = self.abs()?;
        Ok(value.limbs)
    }

    /// Returns the truncating quotient and the remainder.
    ///
    /// ECMAScript `BigInt` division truncates toward zero and the remainder takes
    /// the dividend's sign, which the pinned oracle confirms: `7n/2n` is `3n`,
    /// `(-7n)/2n` is `-3n`, and `(-7n)%2n` is `-1n`.
    pub(crate) fn div_rem(&self, other: &Self) -> Result<(Self, Self), BigIntError> {
        if other.is_zero() {
            return Err(BigIntError::DivisionByZero);
        }
        if self.is_zero() {
            return Ok((Self::zero(), Self::zero()));
        }
        let dividend_negative = self.is_negative();
        let divisor_negative = other.is_negative();
        let dividend = self.magnitude()?;
        let divisor = other.magnitude()?;
        let (quotient, remainder) = divide_magnitudes(&dividend, &divisor)?;
        let mut quotient = Self::from_limbs(quotient)?;
        let mut remainder = Self::from_limbs(remainder)?;
        if dividend_negative != divisor_negative {
            quotient = quotient.neg()?;
        }
        if dividend_negative {
            remainder = remainder.neg()?;
        }
        Ok((quotient, remainder))
    }

    /// Returns `self` raised to `exponent`.
    pub(crate) fn pow(&self, exponent: &Self) -> Result<Self, BigIntError> {
        if exponent.is_negative() {
            return Err(BigIntError::NegativeExponent);
        }
        let Some(mut remaining) = exponent.to_u64() else {
            return Err(BigIntError::ResultTooLarge);
        };
        // `0n ** 0n` is `1n`, which the oracle confirms.
        let mut result = Self::from_i32(1);
        let mut base = self.clone();
        while remaining > 0 {
            if remaining & 1 == 1 {
                result = result.mul(&base).map_err(promote_size_error)?;
            }
            remaining >>= 1;
            if remaining > 0 {
                base = base.mul(&base).map_err(promote_size_error)?;
            }
        }
        Ok(result)
    }

    /// Returns the bitwise AND.
    pub(crate) fn bitand(&self, other: &Self) -> Result<Self, BigIntError> {
        self.bitwise(other, |left, right| left & right)
    }

    /// Returns the bitwise OR.
    pub(crate) fn bitor(&self, other: &Self) -> Result<Self, BigIntError> {
        self.bitwise(other, |left, right| left | right)
    }

    /// Returns the bitwise XOR.
    pub(crate) fn bitxor(&self, other: &Self) -> Result<Self, BigIntError> {
        self.bitwise(other, |left, right| left ^ right)
    }

    /// Applies one limb-wise bitwise operation over the sign-extended operands.
    fn bitwise(
        &self,
        other: &Self,
        operation: impl Fn(u32, u32) -> u32,
    ) -> Result<Self, BigIntError> {
        let width = self.limbs.len().max(other.limbs.len());
        let mut limbs = Vec::new();
        limbs.try_reserve_exact(width)?;
        for index in 0..width {
            limbs.push(operation(self.limb(index), other.limb(index)));
        }
        Self::from_limbs(limbs)
    }

    /// Returns the bitwise complement.
    ///
    /// `~value` is `-value - 1`, so the oracle reports `~1n` as `-2n`.
    pub(crate) fn not(&self) -> Result<Self, BigIntError> {
        let mut limbs = Vec::new();
        limbs.try_reserve_exact(self.limbs.len())?;
        for &limb in &self.limbs {
            limbs.push(!limb);
        }
        Self::from_limbs(limbs)
    }

    /// Returns the value shifted left by `count` bits.
    pub(crate) fn shl(&self, count: u64) -> Result<Self, BigIntError> {
        if self.is_zero() {
            return Ok(Self::zero());
        }
        let limb_shift = usize::try_from(count / u64::from(LIMB_BITS))
            .map_err(|_| BigIntError::ResultTooLarge)?;
        let bit_shift = u32::try_from(count % u64::from(LIMB_BITS)).expect("modulo 32 fits u32");
        if limb_shift > MAX_LIMBS {
            return Err(BigIntError::ResultTooLarge);
        }
        let mut limbs = Vec::new();
        limbs.try_reserve_exact(self.limbs.len() + limb_shift + 1)?;
        limbs.resize(limb_shift, 0);
        let mut carry = 0_u32;
        for &limb in &self.limbs {
            if bit_shift == 0 {
                limbs.push(limb);
            } else {
                limbs.push((limb << bit_shift) | carry);
                carry = limb >> (LIMB_BITS - bit_shift);
            }
        }
        // The final limb carries the sign, so extend with it rather than zero.
        let extension = self.sign_extension();
        if bit_shift == 0 {
            limbs.push(extension);
        } else {
            limbs.push((extension << bit_shift) | carry);
        }
        Self::from_limbs(limbs).map_err(promote_size_error)
    }

    /// Returns the value shifted right by `count` bits.
    ///
    /// The shift is arithmetic, so the sign is preserved and `(-1n) >> 1n`
    /// stays `-1n`.
    pub(crate) fn shr(&self, count: u64) -> Result<Self, BigIntError> {
        let extension = self.sign_extension();
        // Shifting further than the value's width leaves only the sign.
        let Ok(limb_shift) = usize::try_from(count / u64::from(LIMB_BITS)) else {
            return Self::from_limbs(vec![extension]);
        };
        if limb_shift >= self.limbs.len() {
            return Self::from_limbs(vec![extension]);
        }
        let bit_shift = u32::try_from(count % u64::from(LIMB_BITS)).expect("modulo 32 fits u32");
        let remaining = self.limbs.len() - limb_shift;
        let mut limbs = Vec::new();
        limbs.try_reserve_exact(remaining)?;
        for index in 0..remaining {
            let current = self.limb(limb_shift + index);
            if bit_shift == 0 {
                limbs.push(current);
            } else {
                let next = self.limb(limb_shift + index + 1);
                limbs.push((current >> bit_shift) | (next << (LIMB_BITS - bit_shift)));
            }
        }
        Self::from_limbs(limbs)
    }

    /// Compares two values.
    #[must_use]
    pub(crate) fn cmp(&self, other: &Self) -> Ordering {
        match (self.is_negative(), other.is_negative()) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            (true, true) | (false, false) => {}
        }
        // Same sign: the longer two's-complement value is further from zero, and
        // for negatives that means smaller.
        let negative = self.is_negative();
        match self.limbs.len().cmp(&other.limbs.len()) {
            Ordering::Equal => {}
            ordering => {
                return if negative {
                    ordering.reverse()
                } else {
                    ordering
                };
            }
        }
        for index in (0..self.limbs.len()).rev() {
            match self.limbs[index].cmp(&other.limbs[index]) {
                Ordering::Equal => {}
                ordering => return ordering,
            }
        }
        Ordering::Equal
    }

    /// Renders the value in `radix`, matching `BigInt.prototype.toString`.
    pub(crate) fn to_string_radix(&self, radix: u32) -> Result<String, BigIntError> {
        if !(2..=36).contains(&radix) {
            return Err(BigIntError::InvalidRadix);
        }
        if self.is_zero() {
            return Ok("0".to_owned());
        }
        let negative = self.is_negative();
        let mut magnitude = self.magnitude()?;
        normalize_magnitude(&mut magnitude);
        let mut digits = Vec::new();
        while !magnitude.iter().all(|&limb| limb == 0) {
            let remainder = divide_magnitude_by_small(&mut magnitude, radix);
            let digit = u32::try_from(remainder).expect("a radix remainder is below 36");
            digits.push(digit_char(digit));
            normalize_magnitude(&mut magnitude);
        }
        let mut text = String::new();
        text.try_reserve(digits.len() + usize::from(negative))
            .map_err(BigIntError::from)?;
        if negative {
            text.push('-');
        }
        text.extend(digits.iter().rev());
        Ok(text)
    }

    /// Parses `text` in `radix`.
    ///
    /// Leading and trailing ECMAScript whitespace is accepted, an empty or
    /// whitespace-only string is `0n`, and a `0x`/`0o`/`0b` prefix selects its
    /// radix when `radix` is 10. The oracle confirms `BigInt("0x10")` is `16n`
    /// and `BigInt("")` is `0n`.
    pub(crate) fn from_str_radix(text: &str, radix: u32) -> Result<Self, BigIntError> {
        if !(2..=36).contains(&radix) {
            return Err(BigIntError::InvalidRadix);
        }
        let trimmed = text.trim_matches(is_ecmascript_whitespace);
        if trimmed.is_empty() {
            return Ok(Self::zero());
        }
        let (negative, unsigned) = match trimmed.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
        };
        let (radix, digits) = if radix == 10 {
            detect_radix_prefix(unsigned)
        } else {
            (radix, unsigned)
        };
        if digits.is_empty() {
            return Err(BigIntError::InvalidLiteral);
        }
        let mut value = Self::zero();
        let multiplier =
            Self::from_i32(i32::try_from(radix).expect("a radix between 2 and 36 fits an i32"));
        for character in digits.chars() {
            let digit = character
                .to_digit(radix)
                .ok_or(BigIntError::InvalidLiteral)?;
            value = value.mul(&multiplier)?;
            value = value.add(&Self::from_i32(
                i32::try_from(digit).expect("a radix digit fits an i32"),
            ))?;
        }
        if negative { value.neg() } else { Ok(value) }
    }

    /// Returns the low `bits` bits interpreted as a signed value, matching
    /// `BigInt.asIntN`.
    pub(crate) fn as_int_n(&self, bits: u64) -> Result<Self, BigIntError> {
        if bits == 0 {
            return Ok(Self::zero());
        }
        let truncated = self.truncate_to_bits(bits)?;
        // If the sign bit of the requested width is set, the value is negative,
        // so subtract 2^bits. The oracle reports `BigInt.asIntN(8, 255n)` as
        // `-1n`.
        if truncated.bit(bits - 1) {
            let modulus = Self::from_i32(1).shl(bits)?;
            return truncated.sub(&modulus);
        }
        Ok(truncated)
    }

    /// Returns the low `bits` bits interpreted as an unsigned value, which is
    /// ECMAScript `BigInt.asUintN`.
    ///
    /// This deliberately diverges from the pinned engine. `js_bigint_asUintN`
    /// (`quickjs.c:56092` and `quickjs.c:56075`) returns its argument unchanged
    /// whenever `bits` covers the whole value, so the pinned `qjs` reports
    /// `BigInt.asUintN(64, -1n)` as `-1n`. The specification requires the
    /// result to be non-negative, and V8 reports `18446744073709551615n`. The
    /// divergence is recorded as `QJS-BIGINT-001` in `PORTING.md`, and this
    /// implementation follows ECMAScript.
    pub(crate) fn as_uint_n(&self, bits: u64) -> Result<Self, BigIntError> {
        if bits == 0 {
            return Ok(Self::zero());
        }
        self.truncate_to_bits(bits)
    }

    /// Returns the low `bits` bits as a non-negative value.
    fn truncate_to_bits(&self, bits: u64) -> Result<Self, BigIntError> {
        let limb_count = usize::try_from(bits.div_ceil(u64::from(LIMB_BITS)))
            .map_err(|_| BigIntError::TooLarge)?;
        if limb_count > MAX_LIMBS {
            return Err(BigIntError::TooLarge);
        }
        let mut limbs = Vec::new();
        // One extra limb keeps the truncated value non-negative.
        limbs.try_reserve_exact(limb_count + 1)?;
        for index in 0..limb_count {
            limbs.push(self.limb(index));
        }
        let used = u32::try_from(bits % u64::from(LIMB_BITS)).expect("modulo 32 fits u32");
        if used != 0 {
            let last = limbs.len() - 1;
            limbs[last] &= u32::MAX >> (LIMB_BITS - used);
        }
        limbs.push(0);
        Self::from_limbs(limbs)
    }

    /// Returns whether bit `index` of the two's-complement value is set.
    fn bit(&self, index: u64) -> bool {
        let limb_index = usize::try_from(index / u64::from(LIMB_BITS)).unwrap_or(usize::MAX);
        let bit_index = u32::try_from(index % u64::from(LIMB_BITS)).expect("modulo 32 fits u32");
        (self.limb(limb_index) >> bit_index) & 1 == 1
    }
}

/// Promotes an allocation-size failure into the shift/exponent spelling.
///
/// `quickjs.c:12184` reports `BigInt is too large` for a result that grew past
/// the cap, which is a different message from an allocation refusal.
const fn promote_size_error(error: BigIntError) -> BigIntError {
    match error {
        BigIntError::TooLarge => BigIntError::ResultTooLarge,
        other => other,
    }
}

/// Trims redundant sign limbs so the representation stays canonical.
fn normalize(limbs: &mut Vec<u32>) {
    while limbs.len() > 1 {
        let top = limbs[limbs.len() - 1];
        let next = limbs[limbs.len() - 2];
        // A leading limb is redundant only when it merely repeats the sign bit
        // that the next limb already carries.
        let redundant = (top == 0 && next >> (LIMB_BITS - 1) == 0)
            || (top == u32::MAX && next >> (LIMB_BITS - 1) == 1);
        if !redundant {
            break;
        }
        limbs.pop();
    }
}

/// Trims leading zero limbs from a magnitude, keeping at least one.
fn normalize_magnitude(limbs: &mut Vec<u32>) {
    while limbs.len() > 1 && limbs[limbs.len() - 1] == 0 {
        limbs.pop();
    }
}

/// Divides a magnitude in place by a small divisor, returning the remainder.
fn divide_magnitude_by_small(limbs: &mut [u32], divisor: u32) -> u64 {
    let mut remainder = 0_u64;
    for limb in limbs.iter_mut().rev() {
        let current = (remainder << LIMB_BITS) | u64::from(*limb);
        *limb = u32::try_from(current / u64::from(divisor)).expect("a quotient limb fits u32");
        remainder = current % u64::from(divisor);
    }
    remainder
}

/// Divides two non-negative magnitudes, returning the quotient and remainder.
///
/// This is the schoolbook long division of `js_bigint_divrem`
/// (`quickjs.c:11880`), performed one bit at a time so it needs no limb-level
/// quotient estimation and cannot mis-estimate.
fn divide_magnitudes(
    dividend: &[u32],
    divisor: &[u32],
) -> Result<(Vec<u32>, Vec<u32>), BigIntError> {
    let mut quotient = Vec::new();
    quotient.try_reserve_exact(dividend.len() + 1)?;
    quotient.resize(dividend.len() + 1, 0);
    let mut remainder = Vec::new();
    remainder.try_reserve_exact(divisor.len() + dividend.len() + 1)?;
    remainder.resize(divisor.len() + dividend.len() + 1, 0);

    for bit in (0..dividend.len() * LIMB_BITS as usize).rev() {
        shift_magnitude_left_one(&mut remainder);
        let limb = bit / LIMB_BITS as usize;
        let offset = u32::try_from(bit % LIMB_BITS as usize).expect("modulo 32 fits u32");
        if (dividend[limb] >> offset) & 1 == 1 {
            remainder[0] |= 1;
        }
        if compare_magnitudes(&remainder, divisor) != Ordering::Less {
            subtract_magnitude(&mut remainder, divisor);
            quotient[limb] |= 1 << offset;
        }
    }
    // Both results need a zero sign limb so `from_limbs` reads them as
    // non-negative.
    quotient.push(0);
    remainder.push(0);
    Ok((quotient, remainder))
}

/// Shifts a magnitude left by one bit in place.
fn shift_magnitude_left_one(limbs: &mut [u32]) {
    let mut carry = 0_u32;
    for limb in limbs.iter_mut() {
        let next_carry = *limb >> (LIMB_BITS - 1);
        *limb = (*limb << 1) | carry;
        carry = next_carry;
    }
}

/// Compares two non-negative magnitudes of possibly different lengths.
fn compare_magnitudes(left: &[u32], right: &[u32]) -> Ordering {
    let width = left.len().max(right.len());
    for index in (0..width).rev() {
        let left_limb = left.get(index).copied().unwrap_or(0);
        let right_limb = right.get(index).copied().unwrap_or(0);
        match left_limb.cmp(&right_limb) {
            Ordering::Equal => {}
            ordering => return ordering,
        }
    }
    Ordering::Equal
}

/// Subtracts `right` from `left` in place, assuming `left >= right`.
fn subtract_magnitude(left: &mut [u32], right: &[u32]) {
    let mut borrow = 0_i64;
    for (index, limb) in left.iter_mut().enumerate() {
        let right_limb = i64::from(right.get(index).copied().unwrap_or(0));
        let difference = i64::from(*limb) - right_limb - borrow;
        if difference < 0 {
            *limb = u32::try_from(difference + (1_i64 << LIMB_BITS))
                .expect("a borrowed difference fits u32");
            borrow = 1;
        } else {
            *limb = u32::try_from(difference).expect("a difference fits u32");
            borrow = 0;
        }
    }
}

/// Returns the lowercase digit character for `digit`.
fn digit_char(digit: u32) -> char {
    char::from_digit(digit, 36).expect("a digit below 36 has a representation")
}

/// Returns whether `character` is ECMAScript whitespace or a line terminator.
fn is_ecmascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{feff}'
    ) || character.is_whitespace()
}

/// Splits an explicit radix prefix from a decimal literal.
fn detect_radix_prefix(text: &str) -> (u32, &str) {
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return (radix, rest);
        }
    }
    (10, text)
}

#[cfg(test)]
mod tests {
    use super::{BigIntError, JsBigInt, LIMB_BITS, MAX_LIMBS};
    use std::cmp::Ordering;

    /// Asserts the canonical-form invariant: no redundant leading sign limb.
    fn assert_normalized(value: &JsBigInt) {
        let limbs = &value.limbs;
        assert!(!limbs.is_empty(), "a BigInt always has at least one limb");
        if limbs.len() > 1 {
            let top = limbs[limbs.len() - 1];
            let next = limbs[limbs.len() - 2];
            let redundant = (top == 0 && next >> (LIMB_BITS - 1) == 0)
                || (top == u32::MAX && next >> (LIMB_BITS - 1) == 1);
            assert!(
                !redundant,
                "leading limb {top:#x} is redundant over {next:#x}"
            );
        }
    }

    fn parse(text: &str) -> JsBigInt {
        let value = JsBigInt::from_str_radix(text, 10).expect("decimal literal");
        assert_normalized(&value);
        value
    }

    fn decimal(value: &JsBigInt) -> String {
        assert_normalized(value);
        value.to_string_radix(10).expect("decimal rendering")
    }

    #[test]
    fn small_integers_round_trip_through_every_constructor() {
        for value in [0_i32, 1, -1, 2, -2, i32::MAX, i32::MIN] {
            let big = JsBigInt::from_i32(value);
            assert_normalized(&big);
            assert_eq!(big.to_i64(), Some(i64::from(value)), "i32 {value}");
            assert_eq!(decimal(&big), value.to_string());
        }
        for value in [0_i64, 1, -1, i64::MAX, i64::MIN, i64::from(i32::MIN) - 1] {
            let big = JsBigInt::from_i64(value);
            assert_normalized(&big);
            assert_eq!(big.to_i64(), Some(value), "i64 {value}");
            assert_eq!(decimal(&big), value.to_string());
        }
        for value in [0_u64, 1, u64::from(u32::MAX), u64::MAX, 1_u64 << 63] {
            let big = JsBigInt::from_u64(value);
            assert_normalized(&big);
            assert_eq!(big.to_u64(), Some(value), "u64 {value}");
            assert_eq!(decimal(&big), value.to_string());
        }
    }

    #[test]
    fn zero_is_canonical_and_unsigned() {
        let zero = JsBigInt::zero();
        assert!(zero.is_zero());
        assert!(!zero.is_negative());
        // Negating zero yields zero, so `-0n === 0n`.
        let negated = zero.neg().expect("negate zero");
        assert_normalized(&negated);
        assert!(negated.is_zero());
        assert_eq!(negated, zero);
        assert_eq!(decimal(&zero), "0");
    }

    /// Oracle: `qjs -e 'print(4294967295n + 1n, 18446744073709551615n + 1n)'`
    /// prints `4294967296n 18446744073709551616n`.
    #[test]
    fn addition_carries_across_limb_boundaries() {
        let cases = [
            ("4294967295", "1", "4294967296"),
            ("18446744073709551615", "1", "18446744073709551616"),
            (
                "340282366920938463463374607431768211455",
                "1",
                "340282366920938463463374607431768211456",
            ),
            ("-1", "1", "0"),
            ("-4294967296", "1", "-4294967295"),
        ];
        for (left, right, expected) in cases {
            let sum = parse(left).add(&parse(right)).expect("addition");
            assert_eq!(decimal(&sum), expected, "{left} + {right}");
        }
    }

    /// Oracle: `qjs -e 'print(4294967296n - 1n, 0n - 18446744073709551616n)'`
    /// prints `4294967295n -18446744073709551616n`.
    #[test]
    fn subtraction_borrows_across_limb_boundaries() {
        let cases = [
            ("4294967296", "1", "4294967295"),
            ("0", "18446744073709551616", "-18446744073709551616"),
            ("1", "2", "-1"),
            ("-1", "-1", "0"),
        ];
        for (left, right, expected) in cases {
            let difference = parse(left).sub(&parse(right)).expect("subtraction");
            assert_eq!(decimal(&difference), expected, "{left} - {right}");
        }
    }

    /// Oracle: `qjs -e 'print(4294967296n * 4294967296n, (-3n) * 5n, 3n * -5n)'`
    /// prints `18446744073709551616n -15n -15n`.
    #[test]
    fn multiplication_handles_widths_and_signs() {
        let cases = [
            ("4294967296", "4294967296", "18446744073709551616"),
            ("-3", "5", "-15"),
            ("3", "-5", "-15"),
            ("-3", "-5", "15"),
            ("0", "12345678901234567890", "0"),
            (
                "123456789012345678901234567890",
                "987654321098765432109876543210",
                "121932631137021795226185032733622923332237463801111263526900",
            ),
        ];
        for (left, right, expected) in cases {
            let product = parse(left).mul(&parse(right)).expect("multiplication");
            assert_eq!(decimal(&product), expected, "{left} * {right}");
        }
    }

    /// Oracle: `qjs -e 'print(7n/2n, (-7n)/2n, 7n/(-2n), (-7n)/(-2n))'` prints
    /// `3n -3n -3n 3n`, and
    /// `qjs -e 'print(7n%2n, (-7n)%2n, 7n%(-2n), (-7n)%(-2n))'` prints
    /// `1n -1n 1n -1n`.
    #[test]
    fn division_truncates_toward_zero_for_every_sign_combination() {
        let cases = [
            ("7", "2", "3", "1"),
            ("-7", "2", "-3", "-1"),
            ("7", "-2", "-3", "1"),
            ("-7", "-2", "3", "-1"),
            ("0", "5", "0", "0"),
            ("18446744073709551616", "4294967296", "4294967296", "0"),
            ("100", "7", "14", "2"),
        ];
        for (left, right, quotient, remainder) in cases {
            let (actual_quotient, actual_remainder) =
                parse(left).div_rem(&parse(right)).expect("division");
            assert_eq!(decimal(&actual_quotient), quotient, "{left} / {right}");
            assert_eq!(decimal(&actual_remainder), remainder, "{left} % {right}");
        }
    }

    #[test]
    fn division_by_zero_is_an_error() {
        assert_eq!(
            parse("5").div_rem(&JsBigInt::zero()),
            Err(BigIntError::DivisionByZero)
        );
    }

    /// Oracle: `qjs -e 'print(2n**64n, 0n**0n, 3n**5n, (-2n)**3n)'` prints
    /// `18446744073709551616n 1n 243n -8n`.
    #[test]
    fn exponentiation_matches_the_specification() {
        let cases = [
            ("2", "64", "18446744073709551616"),
            ("0", "0", "1"),
            ("3", "5", "243"),
            ("-2", "3", "-8"),
            ("-2", "4", "16"),
            ("1", "1000", "1"),
        ];
        for (base, exponent, expected) in cases {
            let power = parse(base).pow(&parse(exponent)).expect("exponentiation");
            assert_eq!(decimal(&power), expected, "{base} ** {exponent}");
        }
    }

    #[test]
    fn a_negative_exponent_is_an_error() {
        assert_eq!(
            parse("2").pow(&parse("-1")),
            Err(BigIntError::NegativeExponent)
        );
    }

    /// Oracle: `qjs -e 'print(~1n, ~0n, ~(-1n))'` prints `-2n -1n 0n`;
    /// `qjs -e 'print((-1n)&255n, (-5n)|3n, (-1n)^(-1n), 12n&10n, 12n|10n, 12n^10n)'`
    /// prints `255n -5n 0n 8n 14n 6n`.
    #[test]
    fn bitwise_operations_follow_two_s_complement() {
        assert_eq!(decimal(&parse("1").not().expect("not")), "-2");
        assert_eq!(decimal(&parse("0").not().expect("not")), "-1");
        assert_eq!(decimal(&parse("-1").not().expect("not")), "0");

        let and_cases = [("-1", "255", "255"), ("12", "10", "8"), ("-4", "-3", "-4")];
        for (left, right, expected) in and_cases {
            let value = parse(left).bitand(&parse(right)).expect("and");
            assert_eq!(decimal(&value), expected, "{left} & {right}");
        }

        let or_cases = [("-5", "3", "-5"), ("12", "10", "14"), ("0", "-1", "-1")];
        for (left, right, expected) in or_cases {
            let value = parse(left).bitor(&parse(right)).expect("or");
            assert_eq!(decimal(&value), expected, "{left} | {right}");
        }

        let xor_cases = [("-1", "-1", "0"), ("12", "10", "6"), ("-1", "0", "-1")];
        for (left, right, expected) in xor_cases {
            let value = parse(left).bitxor(&parse(right)).expect("xor");
            assert_eq!(decimal(&value), expected, "{left} ^ {right}");
        }
    }

    /// Oracle: `qjs -e 'print(1n<<64n, (-1n)>>1n, (-8n)>>2n, 255n>>4n, 1n<<100n)'`
    /// prints
    /// `18446744073709551616n -1n -2n 15n 1267650600228229401496703205376n`.
    #[test]
    fn shifts_preserve_the_sign_and_cross_limb_boundaries() {
        let left_cases = [
            ("1", 64_u64, "18446744073709551616"),
            ("1", 100, "1267650600228229401496703205376"),
            ("1", 0, "1"),
            ("-1", 4, "-16"),
            ("0", 1000, "0"),
        ];
        for (value, count, expected) in left_cases {
            let shifted = parse(value).shl(count).expect("shift left");
            assert_eq!(decimal(&shifted), expected, "{value} << {count}");
        }

        let right_cases = [
            ("-1", 1_u64, "-1"),
            ("-8", 2, "-2"),
            ("255", 4, "15"),
            ("18446744073709551616", 64, "1"),
            ("1", 100, "0"),
            ("-1", 1000, "-1"),
        ];
        for (value, count, expected) in right_cases {
            let shifted = parse(value).shr(count).expect("shift right");
            assert_eq!(decimal(&shifted), expected, "{value} >> {count}");
        }
    }

    /// Oracle: `qjs -e 'print((255n).toString(16), (255n).toString(2))'` prints
    /// `ff 11111111`; `qjs -e 'print((-255n).toString(16))'` prints `-ff`;
    /// `qjs -e 'print((123n**20n).toString(7))'` prints
    /// `23054464414246102362010364064404555366332620161202`.
    #[test]
    fn radix_rendering_matches_the_specification() {
        assert_eq!(parse("255").to_string_radix(16).expect("hex"), "ff");
        assert_eq!(parse("255").to_string_radix(2).expect("binary"), "11111111");
        assert_eq!(parse("-255").to_string_radix(16).expect("hex"), "-ff");
        assert_eq!(parse("0").to_string_radix(36).expect("base 36"), "0");
        assert_eq!(parse("35").to_string_radix(36).expect("base 36"), "z");
        let large = parse("123").pow(&parse("20")).expect("power");
        assert_eq!(
            large.to_string_radix(7).expect("base 7"),
            "23054464414246102362010364064404555366332620161202"
        );
        assert_eq!(
            parse("1")
                .shl(100)
                .expect("shift")
                .to_string_radix(16)
                .expect("hex"),
            "10000000000000000000000000"
        );
    }

    #[test]
    fn every_supported_radix_round_trips() {
        let value = parse("123456789012345678901234567890");
        for radix in 2..=36 {
            let rendered = value.to_string_radix(radix).expect("rendering");
            let parsed = JsBigInt::from_str_radix(&rendered, radix).expect("round trip");
            assert_normalized(&parsed);
            assert_eq!(parsed, value, "radix {radix} round trip via {rendered}");
        }
        assert_eq!(value.to_string_radix(1), Err(BigIntError::InvalidRadix));
        assert_eq!(value.to_string_radix(37), Err(BigIntError::InvalidRadix));
    }

    /// Oracle: `qjs -e 'print(BigInt("0x10"), BigInt(""), BigInt("  12  "), BigInt("-7"))'`
    /// prints `16n 0n 12n -7n`.
    #[test]
    fn literal_parsing_matches_the_specification() {
        assert_eq!(decimal(&parse("0x10")), "16");
        assert_eq!(decimal(&parse("0b101")), "5");
        assert_eq!(decimal(&parse("0o17")), "15");
        assert_eq!(decimal(&parse("")), "0");
        assert_eq!(decimal(&parse("   ")), "0");
        assert_eq!(decimal(&parse("  12  ")), "12");
        assert_eq!(decimal(&parse("-7")), "-7");
        assert_eq!(decimal(&parse("+7")), "7");

        for malformed in ["1.5", "12x", "0x", "abc", "--1", "1 2"] {
            assert_eq!(
                JsBigInt::from_str_radix(malformed, 10),
                Err(BigIntError::InvalidLiteral),
                "{malformed}"
            );
        }
    }

    /// Oracle:
    /// `qjs -e 'print(BigInt.asIntN(8,255n), BigInt.asUintN(8,-1n), BigInt.asIntN(0,5n), BigInt.asIntN(1,1n))'`
    /// prints `-1n 255n 0n -1n`, and `BigInt.asUintN` for `bits < 64` agrees
    /// with V8. For `bits >= 64` the pinned engine has a known bug, so those
    /// cases follow the specification instead (`QJS-BIGINT-001`).
    #[test]
    fn as_int_n_and_as_uint_n_follow_the_specification() {
        let int_cases = [
            ("255", 8_u64, "-1"),
            ("5", 0, "0"),
            ("1", 1, "-1"),
            ("-1", 64, "-1"),
            ("127", 8, "127"),
            ("128", 8, "-128"),
            ("-1", 32, "-1"),
            ("4294967295", 32, "-1"),
            ("1", 65, "1"),
            ("1", 200, "1"),
        ];
        for (value, bits, expected) in int_cases {
            let result = parse(value).as_int_n(bits).expect("asIntN");
            assert_eq!(decimal(&result), expected, "asIntN({bits}, {value})");
        }

        let uint_cases = [
            ("-1", 8_u64, "255"),
            ("5", 0, "0"),
            ("1", 1, "1"),
            // The pinned engine reports `-1n` for these two because
            // `js_bigint_asUintN` returns its argument unchanged once `bits`
            // spans the value (`quickjs.c:56092`). ECMAScript and V8 require a
            // non-negative result, which is what this port produces; see
            // `QJS-BIGINT-001`.
            ("-1", 64, "18446744073709551615"),
            ("-1", 32, "4294967295"),
            ("256", 8, "0"),
            ("-1", 65, "36893488147419103231"),
        ];
        for (value, bits, expected) in uint_cases {
            let result = parse(value).as_uint_n(bits).expect("asUintN");
            assert_eq!(decimal(&result), expected, "asUintN({bits}, {value})");
        }
    }

    #[test]
    fn comparison_orders_across_signs_and_widths() {
        let ordered = [
            "-18446744073709551616",
            "-4294967296",
            "-2",
            "-1",
            "0",
            "1",
            "2",
            "4294967296",
            "18446744073709551616",
        ];
        for (left_index, left) in ordered.iter().enumerate() {
            for (right_index, right) in ordered.iter().enumerate() {
                let expected = left_index.cmp(&right_index);
                assert_eq!(
                    parse(left).cmp(&parse(right)),
                    expected,
                    "{left} vs {right}"
                );
            }
        }
        assert_eq!(parse("5").cmp(&parse("5")), Ordering::Equal);
    }

    #[test]
    fn oversized_results_report_the_size_error() {
        // One bit past the cap.
        let limit = u64::try_from(MAX_LIMBS).expect("limb cap fits u64") * u64::from(LIMB_BITS);
        assert_eq!(
            JsBigInt::from_i32(1).shl(limit + 1),
            Err(BigIntError::ResultTooLarge)
        );
        // A huge exponent is refused rather than attempted.
        assert_eq!(
            parse("2").pow(&parse("100000000000")),
            Err(BigIntError::ResultTooLarge)
        );
    }

    #[test]
    fn wide_values_stay_normalized_through_long_chains() {
        let mut value = parse("1");
        for _ in 0..40 {
            value = value.mul(&parse("4294967296")).expect("multiply");
            assert_normalized(&value);
        }
        // 2^(32*40) has 1281 bits, so the decimal rendering must round trip.
        let rendered = value.to_string_radix(10).expect("decimal");
        assert_eq!(JsBigInt::from_str_radix(&rendered, 10), Ok(value.clone()));

        let mut shrinking = value;
        for _ in 0..40 {
            let (quotient, remainder) = shrinking.div_rem(&parse("4294967296")).expect("divide");
            assert!(remainder.is_zero());
            assert_normalized(&quotient);
            shrinking = quotient;
        }
        assert_eq!(decimal(&shrinking), "1");
    }
}
