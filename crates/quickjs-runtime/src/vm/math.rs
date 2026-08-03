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
        MathMethod::Min | MathMethod::Max => {
            return Err(EngineFault::RuntimeInvariant {
                message: "a variadic Math method entered the unary continuation",
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
