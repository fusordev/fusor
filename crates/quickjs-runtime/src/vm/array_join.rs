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
//! `toString` first performs its own observable `Get(array, "join")`, calls a
//! callable result with no arguments, and otherwise invokes the intrinsic
//! `%Object.prototype.toString%` even if that property was deleted. The join
//! loop remains separately resumable because every element read and conversion
//! can run user code.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// The boxed receiver retained while `Array.prototype.toString` awaits its
/// observable `join` property read.
pub(super) struct ArrayToStringContinuation {
    target: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

impl ArrayToStringContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
    }
}

/// Performs the `Get(array, "join")` half of `Array.prototype.toString`.
pub(super) fn begin_array_to_string(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = match to_object_value(runtime, realm, receiver, origin.clone())? {
        Ok(target) => target,
        Err(exception) => return Err(NativeFailure::Abrupt(exception)),
    };
    let state = ArrayToStringContinuation {
        target,
        realm,
        origin,
    };
    let join_key = runtime.predefined_property_key(PredefinedAtom::Join);
    charge_heap_property_lookup(runtime, &state.target, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        &state.target,
        join_key,
        None,
        realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        array_to_string_continuation,
        |state, value| finish_array_to_string(runtime, state, &value, return_to, execution_budget),
        "Array.prototype.toString join Get produced a structured result",
    )
}

/// Calls a callable `join`, or the unforgeable intrinsic Object fallback.
pub(super) fn finish_array_to_string(
    runtime: &mut Runtime,
    state: ArrayToStringContinuation,
    join: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let ArrayToStringContinuation {
        target,
        realm,
        origin,
    } = state;
    let StoredValue::Function(function) = join else {
        return begin_object_prototype_to_string(
            runtime,
            realm,
            target,
            return_to,
            Some(origin),
            execution_budget,
        );
    };
    Ok(NativeDispatch::Call(NativeCall {
        function: *function,
        receiver: target,
        arguments: CallArguments::empty(),
        return_to,
        origin,
        continuations: Vec::new(),
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

fn array_to_string_continuation(state: ArrayToStringContinuation) -> NativeContinuation {
    NativeContinuation::ArrayToString(state)
}

/// Which stage of the join loop a continuation resumes into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ArrayJoinStage {
    /// Ready to select the default separator or start its `ToString`.
    PrepareSeparator,
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
    /// The unconverted separator retained until after the length snapshot.
    separator_argument: Option<StoredValue>,
    /// The separator, once converted. `None` until the conversion completes.
    separator: Option<JsString>,
    /// The accumulated result.
    accumulated: JsString,
    /// The element count from the single `ToLength` length read.
    length: u64,
    /// The next element index to read.
    next: u64,
    realm: RealmId,
    stage: ArrayJoinStage,
    origin: JsStackFrame,
}

impl ArrayJoinContinuation {
    /// The receiver, pending separator, and accumulated string.
    ///
    /// The count is constant because the continuation never grows: the
    /// accumulated string replaces itself on every element.
    pub(super) const fn retained_values() -> u64 {
        3
    }

    /// Reports the receiver and pending separator to cycle collection.
    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        if let Some(separator) = &self.separator_argument {
            trace_stored_value_root(separator, mark);
        }
    }
}

/// Starts `Array.prototype.join`.
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
    // the separator is converted. It must retain the resulting wrapper so
    // indexed access to primitive receivers uses its ordinary object state.
    let target = match to_object_value(runtime, realm, receiver, origin.clone())? {
        Ok(target) => target,
        Err(exception) => return Err(NativeFailure::Abrupt(exception)),
    };
    let state = ArrayJoinContinuation {
        target,
        separator_argument: separator,
        separator: None,
        accumulated: JsString::empty(),
        length: 0,
        next: 0,
        realm,
        stage: ArrayJoinStage::AwaitLength,
        origin,
    };
    advance_array_join(runtime, state, None, return_to, execution_budget)
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
        separator_argument: separator,
        separator: None,
        accumulated: JsString::empty(),
        length,
        next: 0,
        realm,
        stage: ArrayJoinStage::PrepareSeparator,
        origin,
    };
    advance_array_join(runtime, state, None, return_to, execution_budget)
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
            ArrayJoinStage::PrepareSeparator => match state.separator_argument.take() {
                None | Some(StoredValue::Undefined) => {
                    state.separator = Some(JsString::from_utf8(",")?);
                    state.stage = ArrayJoinStage::NextElement;
                }
                Some(value) => {
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    state.stage = ArrayJoinStage::AwaitSeparator;
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::String,
                        OperatorPrimitiveTarget::ArrayJoinSeparator(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
            },
            ArrayJoinStage::AwaitSeparator => {
                let value = take_completion(&mut completion)?;
                state.separator = Some(operator_primitive_to_string(
                    value,
                    state.realm,
                    &state.origin,
                )?);
                state.stage = ArrayJoinStage::NextElement;
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
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::Number,
                        OperatorPrimitiveTarget::ArrayJoinElement(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                let number = operator_to_number(value, state.realm, &state.origin)?;
                state.length = number_to_length(number);
                state.stage = ArrayJoinStage::PrepareSeparator;
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
                    value @ (StoredValue::Function(_) | StoredValue::Object(_)) => {
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        state.stage = ArrayJoinStage::AwaitElementString;
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::String,
                            OperatorPrimitiveTarget::ArrayJoinElement(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    value => {
                        let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
                        state.accumulated = state.accumulated.concat(&text)?;
                        state.stage = ArrayJoinStage::NextElement;
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
