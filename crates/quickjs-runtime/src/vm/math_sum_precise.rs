/*
 * JavaScript Math.sumPrecise semantics derived from QuickJS.
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

//! Resumable ECMA-262 `Math.sumPrecise` iterator and exact accumulator.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const LIMB_BITS: u32 = 56;
const ROUNDING_BITS: u32 = LIMB_BITS - 53;
const ACCUMULATOR_LIMBS: usize = 39;
const RENORMALIZE_INTERVAL: u32 = 250;
const LIMB_MASK: u64 = (1_u64 << LIMB_BITS) - 1;
const BINARY64_FRACTION_MASK: u64 = (1_u64 << 52) - 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MathSumPreciseStage {
    IteratorMethod,
    Iterator,
    NextMethod,
    NextResult,
    Done,
    Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExactSumState {
    Finite,
    PositiveInfinity,
    NegativeInfinity,
    NotANumber,
}

/// Fixed-width superaccumulator covering every possible binary64 exponent.
struct ExactSum {
    state: ExactSumState,
    counter: u32,
    limb_count: usize,
    limbs: [i64; ACCUMULATOR_LIMBS],
}

impl ExactSum {
    const fn new() -> Self {
        Self {
            state: ExactSumState::Finite,
            counter: RENORMALIZE_INTERVAL,
            limb_count: 0,
            limbs: [0; ACCUMULATOR_LIMBS],
        }
    }

    fn add(&mut self, number: JsNumber) {
        let bits = number.as_f64().to_bits();
        let negative = bits >> 63 != 0;
        let exponent = (bits >> 52) & 0x7ff;
        let mut mantissa = bits & BINARY64_FRACTION_MASK;

        if exponent == 0x7ff {
            if mantissa != 0 {
                self.state = ExactSumState::NotANumber;
            } else {
                self.state = match (self.state, negative) {
                    (ExactSumState::NotANumber, _)
                    | (ExactSumState::PositiveInfinity, true)
                    | (ExactSumState::NegativeInfinity, false) => ExactSumState::NotANumber,
                    (_, false) => ExactSumState::PositiveInfinity,
                    (_, true) => ExactSumState::NegativeInfinity,
                };
            }
            return;
        }

        let (position, shift) = if exponent == 0 {
            if mantissa == 0 {
                // The initial empty state and an all-negative-zero input both
                // produce -0. Any +0 changes that finite result to +0.
                if self.limb_count == 0 && !negative {
                    self.limb_count = 1;
                }
                return;
            }
            (0, 0)
        } else {
            mantissa |= 1_u64 << 52;
            let bit_position = u32::try_from(exponent - 1).expect("binary64 exponent fits u32");
            (
                usize::try_from(bit_position / LIMB_BITS).expect("limb position fits usize"),
                bit_position % LIMB_BITS,
            )
        };

        let low = (mantissa << shift) & LIMB_MASK;
        let high = mantissa >> (LIMB_BITS - shift);
        let low = i64::try_from(low).expect("56-bit limb fits i64");
        let high = i64::try_from(high).expect("53-bit mantissa fragment fits i64");
        if negative {
            self.limbs[position] -= low;
            self.limbs[position + 1] -= high;
        } else {
            self.limbs[position] += low;
            self.limbs[position + 1] += high;
        }
        self.limb_count = self.limb_count.max(position + 2);

        self.counter -= 1;
        if self.counter == 0 {
            self.counter = RENORMALIZE_INTERVAL;
            self.renormalize();
        }
    }

    fn result(mut self) -> f64 {
        match self.state {
            ExactSumState::PositiveInfinity => return f64::INFINITY,
            ExactSumState::NegativeInfinity => return f64::NEG_INFINITY,
            ExactSumState::NotANumber => return f64::NAN,
            ExactSumState::Finite => {}
        }

        self.renormalize();
        let mut count = self.limb_count;
        if count == 0 {
            return -0.0;
        }
        while count > 0 && self.limbs[count - 1] == 0 {
            count -= 1;
        }
        if count == 0 {
            return 0.0;
        }

        let negative = self.limbs[count - 1] < 0;
        if negative {
            let mut carry = 1_u64;
            for limb in &mut self.limbs[..count - 1] {
                let digit = u64::try_from(*limb).expect("normalized lower limb is non-negative");
                let value = LIMB_MASK - digit + carry;
                carry = value >> LIMB_BITS;
                *limb = i64::try_from(value & LIMB_MASK).expect("56-bit limb fits i64");
            }
            self.limbs[count - 1] =
                -self.limbs[count - 1] + i64::try_from(carry).expect("one-bit carry fits i64") - 1;
            while count > 1 && self.limbs[count - 1] == 0 {
                count -= 1;
            }
        }

        let sign = u64::from(negative) << 63;
        if count == 1 && self.limbs[0] < (1_i64 << 52) {
            let fraction = u64::try_from(self.limbs[0]).expect("subnormal sum is non-negative");
            return f64::from_bits(sign | fraction);
        }

        let mut exponent =
            i32::try_from(count * usize::try_from(LIMB_BITS).expect("u32 fits usize"))
                .expect("accumulator exponent fits i32");
        let mut position = count - 1;
        let mut mantissa =
            u64::try_from(self.limbs[position]).expect("absolute top limb is non-negative");
        let shift = mantissa.leading_zeros() - (u64::BITS - LIMB_BITS);
        exponent -= i32::try_from(shift).expect("normalization shift fits i32") + 52;

        if shift != 0 {
            mantissa <<= shift;
            if position > 0 {
                position -= 1;
                let lower_width = LIMB_BITS - shift;
                let lower = u64::try_from(self.limbs[position])
                    .expect("normalized lower limb is non-negative");
                let discarded = lower & low_mask(lower_width);
                mantissa |= lower >> lower_width;
                mantissa |= u64::from(discarded != 0);
            }
        }

        let rounding_mask = low_mask(ROUNDING_BITS);
        if mantissa & rounding_mask == 1_u64 << (ROUNDING_BITS - 1) {
            while position > 0 {
                position -= 1;
                if self.limbs[position] != 0 {
                    mantissa |= 1;
                    break;
                }
            }
        }

        let addend = (1_u64 << (ROUNDING_BITS - 1)) - 1 + ((mantissa >> ROUNDING_BITS) & 1);
        mantissa = (mantissa + addend) >> ROUNDING_BITS;
        if mantissa == 1_u64 << 53 {
            exponent += 1;
        }
        if exponent >= 0x7ff {
            return f64::from_bits(sign | (0x7ff_u64 << 52));
        }
        mantissa &= BINARY64_FRACTION_MASK;
        f64::from_bits(
            sign | (u64::try_from(exponent).expect("normal exponent is positive") << 52) | mantissa,
        )
    }

    fn renormalize(&mut self) {
        let mut carry = 0_i64;
        for limb in &mut self.limbs[..self.limb_count] {
            let value = *limb + carry;
            let bits = u64::from_ne_bytes(value.to_ne_bytes());
            *limb = i64::try_from(bits & LIMB_MASK).expect("56-bit limb fits i64");
            carry = value >> LIMB_BITS;
        }
        if carry != 0 && self.limb_count < ACCUMULATOR_LIMBS {
            self.limbs[self.limb_count] = carry;
            self.limb_count += 1;
        }
    }
}

const fn low_mask(bits: u32) -> u64 {
    (1_u64 << bits) - 1
}

/// One suspended `Math.sumPrecise` iterator traversal.
pub(super) struct MathSumPreciseContinuation {
    items: StoredValue,
    iterator: Option<StoredValue>,
    next: Option<StoredValue>,
    result: Option<StoredValue>,
    sum: ExactSum,
    count: u64,
    realm: RealmId,
    stage: MathSumPreciseStage,
    origin: JsStackFrame,
}

impl MathSumPreciseContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64
            .saturating_add(u64::from(self.iterator.is_some()))
            .saturating_add(u64::from(self.next.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.items, mark);
        for value in [
            self.iterator.as_ref(),
            self.next.as_ref(),
            self.result.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            trace_stored_value_root(value, mark);
        }
    }
}

pub(super) fn begin_math_sum_precise(
    runtime: &mut Runtime,
    items: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(items, StoredValue::Undefined | StoredValue::Null) {
        return abrupt_sum_precise_type_error(realm, origin, "cannot convert to object");
    }
    let state = MathSumPreciseContinuation {
        items,
        iterator: None,
        next: None,
        result: None,
        sum: ExactSum::new(),
        count: 0,
        realm,
        stage: MathSumPreciseStage::IteratorMethod,
        origin,
    };
    read_sum_precise_property(
        runtime,
        state,
        &runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        return_to,
        execution_budget,
    )
}

pub(super) fn advance_math_sum_precise(
    runtime: &mut Runtime,
    mut state: MathSumPreciseContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        MathSumPreciseStage::IteratorMethod => {
            let StoredValue::Function(method) = completion else {
                return abrupt_sum_precise_type_error(
                    state.realm,
                    state.origin,
                    "value is not iterable",
                );
            };
            let receiver = state.items.duplicate();
            state.stage = MathSumPreciseStage::Iterator;
            call_sum_precise_function(method, receiver, state, return_to)
        }
        MathSumPreciseStage::Iterator => {
            if completion.heap_reference().is_none() {
                return abrupt_sum_precise_type_error(state.realm, state.origin, "not an object");
            }
            state.iterator = Some(completion);
            state.stage = MathSumPreciseStage::NextMethod;
            read_sum_precise_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        MathSumPreciseStage::NextMethod => {
            state.next = Some(completion);
            call_sum_precise_next(state, return_to, execution_budget)
        }
        MathSumPreciseStage::NextResult => {
            if completion.heap_reference().is_none() {
                return abrupt_sum_precise_type_error(
                    state.realm,
                    state.origin,
                    "iterator must return an object",
                );
            }
            state.result = Some(completion);
            state.stage = MathSumPreciseStage::Done;
            read_sum_precise_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        MathSumPreciseStage::Done => {
            if completion.is_truthy() {
                execution_budget.charge_instructions(
                    u64::try_from(ACCUMULATOR_LIMBS).expect("limb count fits u64"),
                )?;
                return Ok(NativeDispatch::Immediate(StoredValue::Number(
                    JsNumber::from_f64(state.sum.result()),
                )));
            }
            state.stage = MathSumPreciseStage::Value;
            read_sum_precise_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        MathSumPreciseStage::Value => {
            if state.count >= MAX_SAFE_INTEGER {
                return close_sum_precise_with_type_error(
                    runtime,
                    state,
                    "too many items",
                    return_to,
                    execution_budget,
                );
            }
            let StoredValue::Number(number) = completion else {
                return close_sum_precise_with_type_error(
                    runtime,
                    state,
                    "not a number",
                    return_to,
                    execution_budget,
                );
            };
            execution_budget.charge_instructions(1)?;
            state.sum.add(number);
            state.count = state.count.saturating_add(1);
            state.result = None;
            call_sum_precise_next(state, return_to, execution_budget)
        }
    }
}

fn read_sum_precise_property(
    runtime: &mut Runtime,
    state: MathSumPreciseContinuation,
    key: &PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (base, property_name) = match state.stage {
        MathSumPreciseStage::IteratorMethod => (&state.items, "Symbol.iterator"),
        MathSumPreciseStage::NextMethod => (
            state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Math.sumPrecise next lookup has no iterator",
                })?,
            "next",
        ),
        MathSumPreciseStage::Done => (
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "Math.sumPrecise done lookup has no iterator result",
            })?,
            "done",
        ),
        MathSumPreciseStage::Value => (
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "Math.sumPrecise value lookup has no iterator result",
            })?,
            "value",
        ),
        MathSumPreciseStage::Iterator | MathSumPreciseStage::NextResult => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Math.sumPrecise call stage attempted a property read",
            }
            .into());
        }
    };
    charge_iterator_property_lookup(runtime, base, execution_budget)?;
    match read_static_property(runtime, state.realm, base, key)? {
        PropertyReadOutcome::Value(value) => {
            advance_math_sum_precise(runtime, state, value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            call_sum_precise_function(function, receiver, state, return_to)
        }
        PropertyReadOutcome::Failed(failure) => {
            let name = JsString::from_utf8(property_name)?;
            let pending =
                property_exception_at(state.realm, state.origin.clone(), Some(&name), failure)?;
            Err(NativeFailure::Abrupt(pending))
        }
    }
}

fn call_sum_precise_next(
    mut state: MathSumPreciseContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let next = state.next.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "Math.sumPrecise iterator advance has no retained next method",
    })?;
    let StoredValue::Function(next) = next else {
        return abrupt_sum_precise_type_error(state.realm, state.origin, "not a function");
    };
    execution_budget.charge_instructions(1)?;
    let receiver = state
        .iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Math.sumPrecise iterator advance has no retained iterator",
        })?
        .duplicate();
    state.stage = MathSumPreciseStage::NextResult;
    call_sum_precise_function(*next, receiver, state, return_to)
}

fn call_sum_precise_function(
    function: FunctionId,
    receiver: StoredValue,
    state: MathSumPreciseContinuation,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::MathSumPrecise(Box::new(state)));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::from_values(Vec::new()),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

fn abrupt_sum_precise_type_error(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(sum_precise_exception(
        realm, origin, message,
    )?))
}

fn close_sum_precise_with_type_error(
    runtime: &mut Runtime,
    state: MathSumPreciseContinuation,
    message: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let pending = sum_precise_exception(state.realm, state.origin.clone(), message)?;
    let iterator = state.iterator.ok_or(EngineFault::RuntimeInvariant {
        message: "Math.sumPrecise IteratorClose started before iterator acquisition",
    })?;
    begin_exceptional_iterator_close(runtime, iterator, pending, return_to, execution_budget)
}

fn sum_precise_exception(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn precise(values: &[f64]) -> f64 {
        let mut sum = ExactSum::new();
        for value in values {
            sum.add(JsNumber::from_f64(*value));
        }
        sum.result()
    }

    #[test]
    fn exact_accumulator_preserves_zero_state_and_non_finite_precedence() {
        assert_eq!(precise(&[]).to_bits(), (-0.0_f64).to_bits());
        assert_eq!(precise(&[-0.0]).to_bits(), (-0.0_f64).to_bits());
        assert_eq!(precise(&[-0.0, 0.0]).to_bits(), 0.0_f64.to_bits());
        assert!(precise(&[f64::INFINITY, f64::NEG_INFINITY]).is_nan());
        assert!(precise(&[f64::NAN, f64::INFINITY]).is_nan());
    }

    #[test]
    fn exact_accumulator_rounds_once_after_cancellation() {
        assert_eq!(
            precise(&[1.0e30, 1.0, -1.0e30]).to_bits(),
            1.0_f64.to_bits()
        );
        assert_eq!(precise(&[0.1, 0.2, 0.3]).to_bits(), 0.6_f64.to_bits());
        assert_eq!(
            precise(&[f64::MAX, f64::MAX]).to_bits(),
            f64::INFINITY.to_bits()
        );
        assert_eq!(
            precise(&[f64::from_bits(1), f64::from_bits(1)]).to_bits(),
            2
        );
    }
}
