/*
 * JavaScript BigInt constructor semantics derived from QuickJS.
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

//! The `BigInt` constructor, its prototype methods, and `asIntN`/`asUintN`.
//!
//! `BigInt` is callable but not constructable (`quickjs.c:56005-56012`), so
//! `new BigInt(1)` is a `TypeError`. The prototype carries exactly `toString`,
//! `valueOf`, and `[Symbol.toStringTag]`; there is deliberately no
//! `toLocaleString`.

use std::cmp::Ordering;

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// Both operands of a same-domain `BigInt` operation.
type BigIntOperands = (Arc<JsBigInt>, Arc<JsBigInt>);

/// The outcome of comparing a value against a `BigInt`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BigIntComparison {
    /// Neither operand is a `BigInt`, so the Number path applies.
    NotApplicable,
    /// The operands are not ordered relative to each other, which is what a
    /// `NaN` operand produces. Every relational operator is then `false`.
    Unordered,
    /// The operands compare with this ordering.
    Ordered(Ordering),
}

/// Maps a [`BigIntError`] onto the pinned exception it reports.
pub(super) fn bigint_exception(
    realm: RealmId,
    origin: &JsStackFrame,
    error: BigIntError,
) -> Result<PendingException, NativeFailure> {
    let (kind, message) = match error {
        // The allocation cap and the shift/exponent overflow report different
        // messages upstream, so the distinction is preserved here.
        BigIntError::TooLarge | BigIntError::AllocationFailed => {
            (ExceptionKind::RangeError, "BigInt is too large to allocate")
        }
        BigIntError::ResultTooLarge => (ExceptionKind::RangeError, "BigInt is too large"),
        BigIntError::NotAnInteger => (
            ExceptionKind::RangeError,
            "cannot convert to BigInt: not an integer",
        ),
        BigIntError::NotFinite => (
            ExceptionKind::RangeError,
            "cannot convert NaN or Infinity to BigInt",
        ),
        BigIntError::InvalidLiteral => (ExceptionKind::SyntaxError, "invalid bigint literal"),
        BigIntError::DivisionByZero => (ExceptionKind::RangeError, "division by zero"),
        BigIntError::NegativeExponent => {
            (ExceptionKind::RangeError, "exponent must be non-negative")
        }
        BigIntError::InvalidRadix => (ExceptionKind::RangeError, "radix must be between 2 and 36"),
    };
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    })
}

/// Wraps a `BigInt` result into a stored value.
pub(super) fn bigint_value(value: JsBigInt) -> StoredValue {
    StoredValue::BigInt(Arc::new(value))
}

/// Applies ECMAScript `ToBigInt` to an already-primitive value.
///
/// A Number is rejected even when it is integral: only the explicit `BigInt()`
/// coercion accepts one, and it does so through `from_f64` rather than here
/// (`quickjs.c:14679`).
pub(super) fn to_bigint_from_primitive(
    value: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Arc<JsBigInt>, NativeFailure> {
    match value {
        StoredValue::BigInt(value) => Ok(Arc::clone(value)),
        StoredValue::Boolean(flag) => Ok(Arc::new(JsBigInt::from_i32(i32::from(*flag)))),
        StoredValue::String(text) => {
            let text = text.to_utf8_lossy()?;
            match JsBigInt::from_str_radix(&text, 10) {
                Ok(value) => Ok(Arc::new(value)),
                Err(error) => Err(NativeFailure::Abrupt(bigint_exception(
                    realm, origin, error,
                )?)),
            }
        }
        StoredValue::Number(_) => Err(NativeFailure::Abrupt(engine_type_error(
            realm,
            origin,
            "cannot convert to bigint",
        )?)),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Symbol(_)
        | StoredValue::Function(_)
        | StoredValue::Object(_) => Err(NativeFailure::Abrupt(engine_type_error(
            realm,
            origin,
            "cannot convert to BigInt",
        )?)),
    }
}

/// Builds an engine `TypeError`.
fn engine_type_error(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    })
}

/// `BigInt(value)`.
///
/// The coercion mirrors `JS_ToBigIntCtorFree` (`quickjs.c:55955-56002`): a
/// Number must be an exact integer, a String uses the literal grammar, and
/// `null`, `undefined`, and a Symbol are rejected.
pub(super) fn bigint_constructor(
    realm: RealmId,
    argument: Option<StoredValue>,
    new_target: Option<FunctionId>,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    if new_target.is_some() {
        return Err(NativeFailure::Abrupt(engine_type_error(
            realm,
            origin,
            "BigInt is not a constructor",
        )?));
    }
    let value = argument.unwrap_or(StoredValue::Undefined);
    // A Number converts when it is an exact integer, which is the one place
    // `ToBigInt` and the constructor's coercion differ.
    if let StoredValue::Number(number) = &value {
        let converted = match JsBigInt::from_f64(number.as_f64()) {
            Ok(converted) => converted,
            Err(error) => {
                return Err(NativeFailure::Abrupt(bigint_exception(
                    realm, origin, error,
                )?));
            }
        };
        return Ok(NativeDispatch::Immediate(bigint_value(converted)));
    }
    let converted = to_bigint_from_primitive(&value, realm, origin)?;
    Ok(NativeDispatch::Immediate(StoredValue::BigInt(converted)))
}

/// Resolves the `this` value of a `BigInt.prototype` method.
///
/// This is `js_thisBigIntValue` (`quickjs.c:56014-56027`): the receiver is
/// either a `BigInt` or an `Object(bigint)` wrapper, and anything else throws
/// `TypeError: not a BigInt`.
pub(super) fn this_bigint_value(
    runtime: &Runtime,
    realm: RealmId,
    receiver: &StoredValue,
    origin: &JsStackFrame,
) -> Result<Arc<JsBigInt>, NativeFailure> {
    let wrapped = match receiver {
        StoredValue::BigInt(value) => Some(Arc::clone(value)),
        StoredValue::Object(object) => runtime.boxed_bigint(*object)?,
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Function(_) => None,
    };
    match wrapped {
        Some(value) => Ok(value),
        None => Err(NativeFailure::Abrupt(engine_type_error(
            realm,
            origin,
            "not a BigInt",
        )?)),
    }
}

/// `BigInt.prototype.toString(radix)`.
pub(super) fn bigint_prototype_to_string(
    value: &JsBigInt,
    radix: u32,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let text = match value.to_string_radix(radix) {
        Ok(text) => text,
        Err(error) => {
            return Err(NativeFailure::Abrupt(bigint_exception(
                realm, origin, error,
            )?));
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::String(
        JsString::from_utf8(&text)?,
    )))
}

/// Which truncation `BigInt.asIntN`/`asUintN` applies.
#[derive(Clone, Copy)]
pub(super) enum BigIntTruncation {
    /// `BigInt.asIntN`: the result is signed.
    Signed,
    /// `BigInt.asUintN`: the result is non-negative.
    Unsigned,
}

/// `BigInt.asIntN(bits, value)` and `BigInt.asUintN(bits, value)`.
///
/// `bits` is converted with `ToIndex`, so a non-integral value truncates and an
/// out-of-range one reports `RangeError: invalid array index`.
pub(super) fn bigint_truncate(
    bits: &JsBigInt,
    value: &JsBigInt,
    truncation: BigIntTruncation,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(bits) = bits.to_u64() else {
        return Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("invalid array index")?,
            },
            origin: origin.clone(),
        }));
    };
    let truncated = match truncation {
        BigIntTruncation::Signed => value.as_int_n(bits),
        BigIntTruncation::Unsigned => value.as_uint_n(bits),
    };
    let truncated = match truncated {
        Ok(truncated) => truncated,
        Err(error) => {
            return Err(NativeFailure::Abrupt(bigint_exception(
                realm, origin, error,
            )?));
        }
    };
    Ok(NativeDispatch::Immediate(bigint_value(truncated)))
}

/// Applies a unary operator in the `BigInt` domain.
///
/// Unary `+` has no `BigInt` form: it is defined as `ToNumber`, which a `BigInt`
/// rejects, so it reports the pinned `bigint argument with unary +`
/// (`quickjs.c:14771`).
pub(super) fn apply_bigint_unary_operator(
    opcode: FinalOpcode,
    value: &Arc<JsBigInt>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    if opcode == FinalOpcode::Plus {
        return Err(NativeFailure::Abrupt(engine_type_error(
            realm,
            origin,
            "bigint argument with unary +",
        )?));
    }
    let one = JsBigInt::from_i32(1);
    let outcome = match opcode {
        FinalOpcode::Neg => value.neg(),
        FinalOpcode::Not => value.not(),
        FinalOpcode::Inc | FinalOpcode::PostInc => value.add(&one),
        FinalOpcode::Dec | FinalOpcode::PostDec => value.sub(&one),
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-unary opcode reached BigInt unary execution",
            }
            .into());
        }
    };
    let result = match outcome {
        Ok(result) => bigint_value(result),
        Err(error) => {
            return Err(NativeFailure::Abrupt(bigint_exception(
                realm, origin, error,
            )?));
        }
    };
    // The postfix forms yield the original value and leave the updated one for
    // the binding write.
    Ok(match opcode {
        FinalOpcode::PostInc | FinalOpcode::PostDec => {
            NativeDispatch::Pair(StoredValue::BigInt(Arc::clone(value)), result)
        }
        _ => NativeDispatch::Immediate(result),
    })
}

/// Applies an arithmetic operator when either operand is a `BigInt`.
///
/// Returns `Ok(None)` when neither operand is a `BigInt`, so the Number path
/// runs unchanged. Mixing the domains reports the pinned
/// `cannot convert bigint to number` (`quickjs.c:12959`).
pub(super) fn apply_bigint_arithmetic(
    opcode: FinalOpcode,
    left: &StoredValue,
    right: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Option<NativeDispatch>, NativeFailure> {
    let Some((left, right)) = bigint_operand_pair(left, right, realm, origin)? else {
        return Ok(None);
    };
    let outcome = match opcode {
        FinalOpcode::Mul => left.mul(&right),
        FinalOpcode::Div => left.div_rem(&right).map(|(quotient, _)| quotient),
        FinalOpcode::Mod => left.div_rem(&right).map(|(_, remainder)| remainder),
        FinalOpcode::Sub => left.sub(&right),
        FinalOpcode::Pow => left.pow(&right),
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-arithmetic opcode reached BigInt arithmetic",
            }
            .into());
        }
    };
    match outcome {
        Ok(result) => Ok(Some(NativeDispatch::Immediate(bigint_value(result)))),
        Err(error) => Err(NativeFailure::Abrupt(bigint_exception(
            realm, origin, error,
        )?)),
    }
}

/// Applies a bitwise or shift operator when either operand is a `BigInt`.
///
/// Unsigned right shift has no `BigInt` form, because the value has no fixed
/// width to fill from (`quickjs.c:15750`).
pub(super) fn apply_bigint_bitwise(
    opcode: FinalOpcode,
    left: &StoredValue,
    right: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Option<NativeDispatch>, NativeFailure> {
    let either_is_bigint =
        matches!(left, StoredValue::BigInt(_)) || matches!(right, StoredValue::BigInt(_));
    if either_is_bigint && opcode == FinalOpcode::Shr {
        return Err(NativeFailure::Abrupt(engine_type_error(
            realm,
            origin,
            "bigint operands are forbidden for >>>",
        )?));
    }
    let Some((left, right)) = bigint_operand_pair(left, right, realm, origin)? else {
        return Ok(None);
    };
    let outcome = match opcode {
        FinalOpcode::And => left.bitand(&right),
        FinalOpcode::Or => left.bitor(&right),
        FinalOpcode::Xor => left.bitxor(&right),
        FinalOpcode::Shl | FinalOpcode::Sar => {
            return bigint_shift(opcode, &left, &right, realm, origin).map(Some);
        }
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-bitwise opcode reached BigInt bitwise execution",
            }
            .into());
        }
    };
    match outcome {
        Ok(result) => Ok(Some(NativeDispatch::Immediate(bigint_value(result)))),
        Err(error) => Err(NativeFailure::Abrupt(bigint_exception(
            realm, origin, error,
        )?)),
    }
}

/// Applies a `BigInt` shift, where a negative count reverses the direction.
fn bigint_shift(
    opcode: FinalOpcode,
    left: &JsBigInt,
    right: &JsBigInt,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let negative_count = right.is_negative();
    let magnitude = match right.abs() {
        Ok(magnitude) => magnitude,
        Err(error) => {
            return Err(NativeFailure::Abrupt(bigint_exception(
                realm, origin, error,
            )?));
        }
    };
    // A count that does not fit `u64` cannot produce a representable left shift,
    // and saturates a right shift to the sign.
    let count = magnitude.to_u64();
    let shift_left = (opcode == FinalOpcode::Shl) != negative_count;
    let outcome = match count {
        Some(count) if shift_left => left.shl(count),
        Some(count) => left.shr(count),
        None if shift_left => Err(BigIntError::ResultTooLarge),
        None => left.shr(u64::MAX),
    };
    match outcome {
        Ok(result) => Ok(NativeDispatch::Immediate(bigint_value(result))),
        Err(error) => Err(NativeFailure::Abrupt(bigint_exception(
            realm, origin, error,
        )?)),
    }
}

/// Resolves both operands into the `BigInt` domain when either one is a
/// `BigInt`.
///
/// Returns `Ok(None)` when neither is, so the caller keeps the Number path. A
/// mixed pair is a `TypeError`: the domains never coerce into each other.
fn bigint_operand_pair(
    left: &StoredValue,
    right: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Option<BigIntOperands>, NativeFailure> {
    match (left, right) {
        (StoredValue::BigInt(left), StoredValue::BigInt(right)) => {
            Ok(Some((Arc::clone(left), Arc::clone(right))))
        }
        (StoredValue::BigInt(_), other) | (other, StoredValue::BigInt(_)) => {
            // A Boolean or String operand does not lift into the BigInt domain
            // for an operator, unlike the explicit `BigInt()` coercion.
            let _ = other;
            Err(NativeFailure::Abrupt(engine_type_error(
                realm,
                origin,
                "cannot convert bigint to number",
            )?))
        }
        _ => Ok(None),
    }
}

/// Applies `+` when either operand is a `BigInt`.
///
/// The caller has already handled the String case, so reaching here with a
/// `BigInt` means both operands must be `BigInt`s.
pub(super) fn apply_bigint_addition(
    left: &StoredValue,
    right: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Option<NativeDispatch>, NativeFailure> {
    let Some((left, right)) = bigint_operand_pair(left, right, realm, origin)? else {
        return Ok(None);
    };
    match left.add(&right) {
        Ok(result) => Ok(Some(NativeDispatch::Immediate(bigint_value(result)))),
        Err(error) => Err(NativeFailure::Abrupt(bigint_exception(
            realm, origin, error,
        )?)),
    }
}

/// Compares a `BigInt` against another value for a relational operator.
///
/// Returns `Ok(None)` when neither operand is a `BigInt`, so the Number path
/// runs unchanged. The inner `Option` is `None` for an unordered comparison,
/// which is what `NaN` produces: `1n < NaN` and `1n > NaN` are both `false`.
///
/// The comparison is mathematical rather than rounded, so a `BigInt` too large
/// for binary64 still compares correctly against a Number.
pub(super) fn bigint_relational_ordering(
    left: &StoredValue,
    right: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<BigIntComparison, NativeFailure> {
    let comparison = match (left, right) {
        (StoredValue::BigInt(left), StoredValue::BigInt(right)) => Some(left.compare(right)),
        (StoredValue::BigInt(left), other) => {
            compare_bigint_with_value(left, other, realm, origin)?
        }
        // Comparing from the other side reverses the ordering.
        (other, StoredValue::BigInt(right)) => {
            compare_bigint_with_value(right, other, realm, origin)?.map(Ordering::reverse)
        }
        _ => return Ok(BigIntComparison::NotApplicable),
    };
    Ok(match comparison {
        Some(ordering) => BigIntComparison::Ordered(ordering),
        None => BigIntComparison::Unordered,
    })
}

/// Compares `value` against a non-`BigInt` operand, from the `BigInt`'s side.
fn compare_bigint_with_value(
    value: &JsBigInt,
    other: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Option<Ordering>, NativeFailure> {
    match other {
        // A String operand is parsed as a `BigInt` literal; a malformed one is
        // unordered rather than an error, so `1n < "x"` is `false`.
        StoredValue::String(text) => {
            let text = text.to_utf8_lossy()?;
            Ok(JsBigInt::from_str_radix(&text, 10)
                .ok()
                .map(|parsed| value.compare(&parsed)))
        }
        StoredValue::Boolean(flag) => {
            Ok(Some(value.compare(&JsBigInt::from_i32(i32::from(*flag)))))
        }
        StoredValue::Number(number) => Ok(compare_bigint_with_number(value, number.as_f64())),
        // `undefined` becomes `NaN`, which is unordered against everything.
        StoredValue::Undefined => Ok(None),
        StoredValue::Null => Ok(Some(value.compare(&JsBigInt::zero()))),
        StoredValue::Symbol(_) => Err(NativeFailure::Abrupt(engine_type_error(
            realm,
            origin,
            "cannot convert symbol to number",
        )?)),
        StoredValue::BigInt(_) | StoredValue::Function(_) | StoredValue::Object(_) => {
            Err(EngineFault::RuntimeInvariant {
                message: "BigInt relational comparison received an unconverted operand",
            }
            .into())
        }
    }
}

/// Compares a `BigInt` against a Number exactly.
///
/// `NaN` is unordered. A non-integral Number is resolved by comparing against
/// its floor and ceiling, so no precision is lost in either direction.
fn compare_bigint_with_number(value: &JsBigInt, number: f64) -> Option<Ordering> {
    if number.is_nan() {
        return None;
    }
    if number.is_infinite() {
        // Every finite BigInt is below +Infinity and above -Infinity.
        return Some(if number.is_sign_positive() {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    let floor = number.floor();
    // Both bounds are exact integers, so each converts without rounding.
    let Ok(lower) = JsBigInt::from_f64(floor) else {
        return None;
    };
    match value.compare(&lower) {
        // Below the floor, so below the value itself.
        Ordering::Less => Some(Ordering::Less),
        // Above the floor: still below the Number only when the Number has a
        // fractional part and the BigInt equals the floor, which the equality
        // arm already excluded.
        Ordering::Greater => Some(Ordering::Greater),
        Ordering::Equal => {
            // A Number equal to its own floor is an exact integer, so the two
            // are equal; otherwise the Number sits strictly above the floor and
            // therefore above the BigInt. The comparison is exact by intent.
            #[expect(
                clippy::float_cmp,
                reason = "integrality is an exact property, so an epsilon comparison would be wrong"
            )]
            let integral = floor == number;
            Some(if integral {
                Ordering::Equal
            } else {
                Ordering::Less
            })
        }
    }
}
