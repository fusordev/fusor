/*
 * JavaScript Math semantics derived from QuickJS.
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

//! Resumable ECMA-262 `%Math%` numeric coercion and algorithms.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// One suspended variadic `Math.min` or `Math.max` operation.
pub(crate) struct MathExtremaContinuation {
    method: MathMethod,
    arguments: Vec<StoredValue>,
    next: usize,
    result: JsNumber,
    realm: RealmId,
    origin: JsStackFrame,
}

/// One suspended variadic `Math.hypot` operation.
pub(crate) struct MathHypotContinuation {
    arguments: Vec<StoredValue>,
    next: usize,
    result: f64,
    saw_nan: bool,
    saw_infinity: bool,
    realm: RealmId,
    origin: JsStackFrame,
}

impl MathHypotContinuation {
    pub(crate) fn retained_values(&self) -> u64 {
        usize_to_u64(self.arguments.len())
    }

    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        for argument in &self.arguments {
            trace_stored_value_root(argument, mark);
        }
    }
}

impl MathExtremaContinuation {
    pub(crate) fn retained_values(&self) -> u64 {
        usize_to_u64(self.arguments.len())
    }

    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        for argument in &self.arguments {
            trace_stored_value_root(argument, mark);
        }
    }
}

/// Starts one method from the installed specification-order `%Math%` prefix.
pub(super) fn begin_math_method(
    runtime: &mut Runtime,
    method: MathMethod,
    realm: RealmId,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if method.is_extrema() {
        let initial = if method == MathMethod::Min {
            f64::INFINITY
        } else {
            f64::NEG_INFINITY
        };
        return advance_math_extrema(
            runtime,
            MathExtremaContinuation {
                method,
                arguments: arguments.into_remaining_values(),
                next: 0,
                result: JsNumber::from_f64(initial),
                realm,
                origin,
            },
            None,
            return_to,
            execution_budget,
        );
    }

    if method == MathMethod::Hypot {
        return advance_math_hypot(
            runtime,
            MathHypotContinuation {
                arguments: arguments.into_remaining_values(),
                next: 0,
                result: 0.0,
                saw_nan: false,
                saw_infinity: false,
                realm,
                origin,
            },
            None,
            return_to,
            execution_budget,
        );
    }

    if method == MathMethod::Random {
        return Ok(NativeDispatch::Immediate(StoredValue::Number(
            runtime.math_random_number(realm)?,
        )));
    }

    if method.is_binary() {
        let left = arguments.take_first_or_undefined();
        let right = arguments.take_first_or_undefined();
        return begin_operator_primitive_conversion(
            runtime,
            left,
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::MathBinaryRight { method, right },
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }

    begin_operator_primitive_conversion(
        runtime,
        arguments.take_first_or_undefined(),
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::MathUnary(method),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

/// Accepts one converted extrema argument and advances to the next conversion.
///
/// ECMA-262 first applies `ToNumber` to every argument and only then examines
/// the list. Retaining a `NaN` accumulator while continuing every conversion
/// preserves that observable rule: a later throwing `valueOf` still wins.
pub(super) fn advance_math_extrema(
    runtime: &mut Runtime,
    mut state: MathExtremaContinuation,
    completion: Option<JsNumber>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(number) = completion {
        update_math_extrema(&mut state, number)?;
    }

    loop {
        if state.next == state.arguments.len() {
            return Ok(NativeDispatch::Immediate(StoredValue::Number(state.result)));
        }

        execution_budget.charge_instructions(1)?;
        let argument = state
            .arguments
            .get_mut(state.next)
            .map(|argument| std::mem::replace(argument, StoredValue::Undefined))
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Math extrema argument cursor exceeded its retained values",
            })?;
        state.next = state.next.saturating_add(1);

        // Primitive conversions cannot call user code, so keep them on this
        // explicit loop. Only an Object or Function needs a suspended
        // continuation; this prevents a large primitive argument list from
        // recursively re-entering the conversion dispatcher.
        if argument.heap_reference().is_none() {
            let number = operator_to_number(argument, state.realm, &state.origin)?;
            update_math_extrema(&mut state, number)?;
            continue;
        }

        let realm = state.realm;
        let origin = state.origin.clone();
        return begin_operator_primitive_conversion(
            runtime,
            argument,
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::MathExtrema(Box::new(state)),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
}

fn update_math_extrema(
    state: &mut MathExtremaContinuation,
    number: JsNumber,
) -> Result<(), EngineFault> {
    let current = state.result.as_f64();
    let candidate = number.as_f64();
    let result = match state.method {
        MathMethod::Min => {
            if current.is_nan() || candidate.is_nan() {
                f64::NAN
            } else if candidate < current
                || (candidate == 0.0
                    && current == 0.0
                    && candidate.is_sign_negative()
                    && current.is_sign_positive())
            {
                candidate
            } else {
                current
            }
        }
        MathMethod::Max => {
            if current.is_nan() || candidate.is_nan() {
                f64::NAN
            } else if candidate > current
                || (candidate == 0.0
                    && current == 0.0
                    && candidate.is_sign_positive()
                    && current.is_sign_negative())
            {
                candidate
            } else {
                current
            }
        }
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "a non-extrema Math method entered the variadic continuation",
            });
        }
    };
    state.result = JsNumber::from_f64(result);
    Ok(())
}

/// Accepts one converted `hypot` argument and advances to the next conversion.
///
/// All conversions complete before the recorded boundary cases are examined,
/// so a later abrupt conversion wins and infinity wins over an earlier NaN.
pub(super) fn advance_math_hypot(
    runtime: &mut Runtime,
    mut state: MathHypotContinuation,
    completion: Option<JsNumber>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(number) = completion {
        update_math_hypot(&mut state, number);
    }

    loop {
        if state.next == state.arguments.len() {
            let result = if state.saw_infinity {
                f64::INFINITY
            } else if state.saw_nan {
                f64::NAN
            } else {
                state.result
            };
            return Ok(NativeDispatch::Immediate(StoredValue::Number(
                JsNumber::from_f64(result),
            )));
        }

        execution_budget.charge_instructions(1)?;
        let argument = state
            .arguments
            .get_mut(state.next)
            .map(|argument| std::mem::replace(argument, StoredValue::Undefined))
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Math.hypot argument cursor exceeded its retained values",
            })?;
        state.next = state.next.saturating_add(1);

        if argument.heap_reference().is_none() {
            let number = operator_to_number(argument, state.realm, &state.origin)?;
            update_math_hypot(&mut state, number);
            continue;
        }

        let realm = state.realm;
        let origin = state.origin.clone();
        return begin_operator_primitive_conversion(
            runtime,
            argument,
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::MathHypot(Box::new(state)),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
}

fn update_math_hypot(state: &mut MathHypotContinuation, number: JsNumber) {
    let value = number.as_f64();
    if value.is_infinite() {
        state.saw_infinity = true;
    } else if value.is_nan() {
        state.saw_nan = true;
    } else {
        state.result = state.result.hypot(value);
    }
}

/// Applies an installed one-argument `%Math%` algorithm after `ToNumber`.
pub(super) fn finish_math_unary(
    method: MathMethod,
    number: JsNumber,
) -> Result<NativeDispatch, NativeFailure> {
    let value = number.as_f64();
    let result = match method {
        MathMethod::Abs => value.abs(),
        MathMethod::Floor => value.floor(),
        MathMethod::Ceil => value.ceil(),
        MathMethod::Round => math_round(value),
        MathMethod::Sqrt => value.sqrt(),
        MathMethod::Acos => value.acos(),
        MathMethod::Asin => value.asin(),
        MathMethod::Atan => value.atan(),
        MathMethod::Cos => value.cos(),
        MathMethod::Exp => value.exp(),
        MathMethod::Log => value.ln(),
        MathMethod::Sin => value.sin(),
        MathMethod::Tan => value.tan(),
        MathMethod::Trunc => value.trunc(),
        MathMethod::Sign => {
            if value.is_nan() || value == 0.0 {
                value
            } else if value.is_sign_negative() {
                -1.0
            } else {
                1.0
            }
        }
        MathMethod::Cosh => value.cosh(),
        MathMethod::Sinh => value.sinh(),
        MathMethod::Tanh => value.tanh(),
        MathMethod::Acosh => value.acosh(),
        MathMethod::Asinh => value.asinh(),
        MathMethod::Atanh => value.atanh(),
        MathMethod::Expm1 => value.exp_m1(),
        MathMethod::Log1p => value.ln_1p(),
        MathMethod::Log2 => value.log2(),
        MathMethod::Log10 => value.log10(),
        MathMethod::Cbrt => value.cbrt(),
        MathMethod::F16Round => math_f16round(value),
        MathMethod::FRound => math_fround(value),
        MathMethod::Clz32 => f64::from(number_to_uint32(number).leading_zeros()),
        MathMethod::Min
        | MathMethod::Max
        | MathMethod::Atan2
        | MathMethod::Pow
        | MathMethod::Hypot
        | MathMethod::Random
        | MathMethod::Imul => {
            return Err(EngineFault::RuntimeInvariant {
                message: "a non-unary Math method entered the unary continuation",
            }
            .into());
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::Number(
        JsNumber::from_f64(result),
    )))
}

/// Applies one two-argument `%Math%` algorithm after both `ToNumber` steps.
pub(super) fn finish_math_binary(
    method: MathMethod,
    left: JsNumber,
    right: JsNumber,
) -> Result<NativeDispatch, NativeFailure> {
    let left = left.as_f64();
    let right = right.as_f64();
    let result = match method {
        // `atan2`'s argument names are `(y, x)`; Rust deliberately uses the
        // same receiver/argument order and preserves the specified zero signs.
        MathMethod::Atan2 => left.atan2(right),
        MathMethod::Pow => number_exponentiate(left, right),
        MathMethod::Imul => {
            let product = number_to_uint32(JsNumber::from_f64(left))
                .wrapping_mul(number_to_uint32(JsNumber::from_f64(right)));
            f64::from(i32::from_ne_bytes(product.to_ne_bytes()))
        }
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "a non-binary Math method entered the binary continuation",
            }
            .into());
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::Number(
        JsNumber::from_f64(result),
    )))
}

/// ECMA-262 rounds ties toward positive infinity, unlike Rust's `f64::round`,
/// and explicitly produces negative zero throughout `[-0.5, -0)`.
fn math_round(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 || value.fract() == 0.0 {
        return value;
    }
    if value > 0.0 && value < 0.5 {
        return 0.0;
    }
    if (-0.5..=0.0).contains(&value) {
        return -0.0;
    }

    let lower = value.floor();
    if value - lower < 0.5 {
        lower
    } else {
        lower + 1.0
    }
}

/// Converts one binary64 value directly to binary16 under roundTiesToEven and
/// widens the exact result back to binary64 without a binary32 intermediate.
fn math_f16round(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 {
        return value;
    }

    let magnitude = value.abs();
    let rounded = if magnitude < 2.0_f64.powi(-14) {
        magnitude.mul_add(2.0_f64.powi(24), 0.0).round_ties_even() * 2.0_f64.powi(-24)
    } else {
        let biased_exponent = ((magnitude.to_bits() >> 52) & 0x7ff) as i32;
        let exponent = biased_exponent - 1023;
        let quantum = 2.0_f64.powi(exponent - 10);
        (magnitude / quantum).round_ties_even() * quantum
    };
    let rounded = if rounded > 65_504.0 {
        f64::INFINITY
    } else {
        rounded
    };
    rounded.copysign(value)
}

/// Performs the deliberate IEEE binary64-to-binary32 narrowing required by
/// ECMA-262 and widens the rounded value back to an ECMAScript Number.
#[allow(
    clippy::cast_possible_truncation,
    reason = "Math.fround is specified as an intentional binary32 narrowing"
)]
fn math_fround(value: f64) -> f64 {
    f64::from(value as f32)
}
