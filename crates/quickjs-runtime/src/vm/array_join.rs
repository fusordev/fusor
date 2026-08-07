/*
 * JavaScript Array.prototype.join semantics derived from QuickJS.
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

//! `Array.prototype.join` and `Array.prototype.toString`.
//!
//! Both are one resumable element loop, because every element read can run a
//! getter and every element's `ToString` can run a user `toString` method. The
//! loop mirrors `js_array_join` (`quickjs.c:42505`): the length is read once
//! with `ToLength`, `null` and `undefined` elements contribute nothing, and the
//! separator defaults to `","` when it is absent or `undefined`.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// Which stage of the join loop a continuation resumes into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ArrayJoinStage {
    /// Awaiting `ToString` of the separator argument.
    AwaitSeparator,
    /// Awaiting the length property read.
    AwaitLength,
    /// Awaiting `ToLength` of the length value.
    AwaitLengthConversion,
    /// Ready to read the next element.
    NextElement,
    /// Awaiting an element read, which may have entered a getter.
    AwaitElement,
    /// Awaiting `ToString` of the element that was just read.
    AwaitElementString,
}

/// One in-progress `Array.prototype.join`.
pub(super) struct ArrayJoinContinuation {
    /// The coerced receiver whose elements are joined.
    target: StoredValue,
    /// The separator, once converted. `None` until the conversion completes.
    separator: Option<JsString>,
    /// The accumulated result.
    accumulated: JsString,
    /// The element count from the single `ToLength` length read.
    length: u64,
    /// Whether `length` comes from a prior `%TypedArray%` validation rather
    /// than this generic join's observable `length` property read.
    length_is_ready: bool,
    /// The next element index to read.
    next: u64,
    realm: RealmId,
    stage: ArrayJoinStage,
    origin: JsStackFrame,
}

impl ArrayJoinContinuation {
    /// The receiver plus the accumulated string.
    ///
    /// The count is constant because the continuation never grows: the
    /// accumulated string replaces itself on every element.
    pub(super) const fn retained_values() -> u64 {
        2
    }

    /// Reports the receiver so cycle collection can trace it.
    pub(super) const fn target(&self) -> &StoredValue {
        &self.target
    }
}

/// Starts `Array.prototype.join` or `Array.prototype.toString`.
///
/// `Array.prototype.toString` is defined as `join` with no separator once its
/// receiver's `join` property is not callable; the pinned engine reaches the
/// same observable result by dispatching straight to `js_array_join`
/// (`quickjs.c:44558`), and the profile's `Array.prototype.join` is
/// non-replaceable, so this shares one implementation.
pub(super) fn begin_array_join(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    separator: Option<StoredValue>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    // `join` begins with `ToObject(this)`, so a nullish receiver throws before
    // the separator is converted.
    let target = match receiver {
        StoredValue::Undefined | StoredValue::Null => {
            return Err(NativeFailure::Abrupt(PendingException {
                realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::TypeError,
                    message: JsString::from_utf8("cannot convert to object")?,
                },
                origin,
            }));
        }
        value => value,
    };
    let state = ArrayJoinContinuation {
        target,
        separator: None,
        accumulated: JsString::empty(),
        length: 0,
        length_is_ready: false,
        next: 0,
        realm,
        stage: ArrayJoinStage::AwaitSeparator,
        origin,
    };
    // An absent or `undefined` separator uses the default `","` without running
    // any conversion, which the oracle confirms: `[1,2].join(undefined)` is
    // `"1,2"`.
    match separator {
        None | Some(StoredValue::Undefined) => {
            let mut state = state;
            state.separator = Some(JsString::from_utf8(",")?);
            state.stage = ArrayJoinStage::AwaitLength;
            advance_array_join(runtime, state, None, return_to, execution_budget)
        }
        Some(value) => begin_operator_primitive_conversion(
            runtime,
            value,
            OperatorPrimitiveHint::String,
            OperatorPrimitiveTarget::ArrayJoinSeparator(Box::new(state)),
            realm,
            return_to,
            native_function_host_origin(),
            execution_budget,
        ),
    }
}

/// Starts `%TypedArray%.prototype.join` after `ValidateTypedArray` has
/// captured its required length witness.
///
/// Unlike generic `Array.prototype.join`, the typed-array method obtains its
/// length before converting `separator`. A separator's `toString` may resize
/// the backing buffer, but the subsequent indexed Gets still run against that
/// original iteration count.
#[expect(
    clippy::too_many_arguments,
    reason = "the native entry point preserves the validated typed-array witness and standard call context"
)]
pub(super) fn begin_typed_array_join(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    length: usize,
    separator: Option<StoredValue>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let length = u64::try_from(length).map_err(|_| EngineFault::RuntimeInvariant {
        message: "TypedArray join length did not fit u64",
    })?;
    let state = ArrayJoinContinuation {
        target: receiver,
        separator: None,
        accumulated: JsString::empty(),
        length,
        length_is_ready: true,
        next: 0,
        realm,
        stage: ArrayJoinStage::AwaitSeparator,
        origin,
    };
    match separator {
        None | Some(StoredValue::Undefined) => {
            let mut state = state;
            state.separator = Some(JsString::from_utf8(",")?);
            state.stage = ArrayJoinStage::NextElement;
            advance_array_join(runtime, state, None, return_to, execution_budget)
        }
        Some(value) => begin_operator_primitive_conversion(
            runtime,
            value,
            OperatorPrimitiveHint::String,
            OperatorPrimitiveTarget::ArrayJoinSeparator(Box::new(state)),
            realm,
            return_to,
            native_function_host_origin(),
            execution_budget,
        ),
    }
}

/// Resumes the join loop.
///
/// `completion` carries the value produced by whatever operation the previous
/// stage awaited.
#[allow(
    clippy::too_many_lines,
    clippy::needless_continue,
    reason = "the separator, length, element-read, and element-string stages form one traced continuation mirroring js_array_join"
)]
pub(super) fn advance_array_join(
    runtime: &mut Runtime,
    mut state: ArrayJoinContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    macro_rules! await_get {
        ($operation:expr) => {
            match $operation? {
                GetContinuationDispatch::Ready {
                    state: resumed,
                    value,
                } => {
                    state = resumed;
                    completion = Some(value);
                    continue;
                }
                GetContinuationDispatch::Suspended(dispatch) => return Ok(dispatch),
            }
        };
    }
    loop {
        match state.stage {
            ArrayJoinStage::AwaitSeparator => {
                let value = take_completion(&mut completion)?;
                state.separator = Some(operator_primitive_to_string(
                    value,
                    state.realm,
                    &state.origin,
                )?);
                state.stage = if state.length_is_ready {
                    ArrayJoinStage::NextElement
                } else {
                    ArrayJoinStage::AwaitLength
                };
            }
            ArrayJoinStage::AwaitLength => {
                let length_key = runtime.predefined_property_key(PredefinedAtom::Length);
                charge_heap_property_lookup(runtime, &state.target, execution_budget)?;
                state.stage = ArrayJoinStage::AwaitLengthConversion;
                let dispatch = begin_value_get(
                    runtime,
                    &state.target,
                    length_key,
                    None,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                await_get!(continue_get_state_after(
                    dispatch,
                    state,
                    array_join_continuation,
                    "Array join length Get produced a structured result",
                ));
            }
            ArrayJoinStage::AwaitLengthConversion => {
                let value = take_completion(&mut completion)?;
                // The length is read once, before any element, and every later
                // index derives from it. `js_get_length64` applies `ToLength`.
                let number = operator_to_number(value, state.realm, &state.origin)?;
                state.length = number_to_length(number);
                state.stage = ArrayJoinStage::NextElement;
            }
            ArrayJoinStage::NextElement => {
                if state.next >= state.length {
                    return Ok(NativeDispatch::Immediate(StoredValue::String(
                        state.accumulated,
                    )));
                }
                execution_budget.charge_instructions(1)?;
                if state.next > 0 {
                    let separator =
                        state
                            .separator
                            .as_ref()
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "array join reached its element loop without a separator",
                            })?;
                    state.accumulated = state.accumulated.concat(separator)?;
                }
                let key = element_key(state.next)?;
                charge_heap_property_lookup(runtime, &state.target, execution_budget)?;
                state.stage = ArrayJoinStage::AwaitElement;
                let dispatch = begin_value_get(
                    runtime,
                    &state.target,
                    key,
                    None,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                await_get!(continue_get_state_after(
                    dispatch,
                    state,
                    array_join_continuation,
                    "Array join element Get produced a structured result",
                ));
            }
            ArrayJoinStage::AwaitElement => {
                let value = take_completion(&mut completion)?;
                state.next = state.next.saturating_add(1);
                // `null` and `undefined` contribute nothing, so a hole joins as
                // an empty field: `[1,,3].join("-")` is `"1--3"`.
                match value {
                    StoredValue::Undefined | StoredValue::Null => {
                        state.stage = ArrayJoinStage::NextElement;
                    }
                    StoredValue::String(text) => {
                        state.accumulated = state.accumulated.concat(&text)?;
                        state.stage = ArrayJoinStage::NextElement;
                    }
                    value => {
                        let realm = state.realm;
                        state.stage = ArrayJoinStage::AwaitElementString;
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::String,
                            OperatorPrimitiveTarget::ArrayJoinElement(Box::new(state)),
                            realm,
                            return_to,
                            native_function_host_origin(),
                            execution_budget,
                        );
                    }
                }
            }
            ArrayJoinStage::AwaitElementString => {
                let value = take_completion(&mut completion)?;
                let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
                state.accumulated = state.accumulated.concat(&text)?;
                state.stage = ArrayJoinStage::NextElement;
            }
        }
    }
}

fn array_join_continuation(state: ArrayJoinContinuation) -> NativeContinuation {
    NativeContinuation::ArrayJoin(Box::new(state))
}

/// Returns the property key for one element index.
fn element_key(index: u64) -> Result<PropertyKey, NativeFailure> {
    let index = u32::try_from(index).map_err(|_| EngineFault::RuntimeInvariant {
        message: "array join index exceeded the array-index domain",
    })?;
    let index = ArrayIndex::new(index).ok_or(EngineFault::RuntimeInvariant {
        message: "array join index reached the non-index sentinel",
    })?;
    Ok(PropertyKey::from_index(index))
}

/// Extracts the awaited completion value.
fn take_completion(completion: &mut Option<StoredValue>) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        NativeFailure::Execution(
            EngineFault::RuntimeInvariant {
                message: "array join resumed without its awaited completion",
            }
            .into(),
        )
    })
}
