/*
 * JavaScript locale-string semantics derived from ECMA-262 and QuickJS.
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

//! ECMA-262 and ECMA-402 `toLocaleString` methods.
//!
//! Number and `BigInt` delegate to the realm's intrinsic `NumberFormat`
//! semantics. The profile selects `","` as Array's implementation-defined list
//! separator. Object and Array remain
//! fully observable: both invoke a property dynamically, and Array additionally
//! suspends at every length conversion, element getter, locale method, and
//! conversion of that method's result.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocaleStringStage {
    AwaitLength,
    AwaitLengthConversion,
    NextElement,
    AwaitElement,
    ReadMethod,
    AwaitMethod,
    AwaitInvocation,
    AwaitElementString,
}

/// One in-progress generic Object or Array locale-string call.
pub(crate) struct LocaleStringContinuation {
    method: LocaleStringMethod,
    target: StoredValue,
    element: Option<StoredValue>,
    arguments: Vec<StoredValue>,
    separator: JsString,
    accumulated: JsString,
    length: u64,
    next: u64,
    realm: RealmId,
    stage: LocaleStringStage,
    origin: JsStackFrame,
}

impl LocaleStringContinuation {
    pub(crate) fn retained_values(&self) -> u64 {
        3_u64
            .saturating_add(u64::from(self.element.is_some()))
            .saturating_add(usize_to_u64(self.arguments.len()))
    }

    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        if let Some(element) = &self.element {
            trace_stored_value_root(element, mark);
        }
        for argument in &self.arguments {
            trace_stored_value_root(argument, mark);
        }
    }
}

/// Starts one locale-string method.
#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch keeps receiver, arguments, realm, return target, origin, and budget explicit"
)]
pub(super) fn begin_locale_string(
    runtime: &mut Runtime,
    method: LocaleStringMethod,
    realm: RealmId,
    receiver: StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        LocaleStringMethod::Number => {
            let value = number_receiver_value(runtime, realm, &receiver, Some(&origin))?;
            let value = to_intl_mathematical_value(StoredValue::Number(value), realm, &origin)?;
            return begin_intl_number_to_locale_string(
                runtime,
                arguments,
                value,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        LocaleStringMethod::BigInt => {
            let value = this_bigint_value(runtime, realm, &receiver, &origin)?;
            let value = to_intl_mathematical_value(StoredValue::BigInt(value), realm, &origin)?;
            return begin_intl_number_to_locale_string(
                runtime,
                arguments,
                value,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        LocaleStringMethod::Object => {
            let state = LocaleStringContinuation {
                method,
                target: receiver.duplicate(),
                element: Some(receiver),
                arguments: Vec::new(),
                separator: JsString::from_utf8(",")?,
                accumulated: JsString::empty(),
                length: 0,
                next: 0,
                realm,
                stage: LocaleStringStage::ReadMethod,
                origin,
            };
            return advance_locale_string(runtime, state, None, return_to, execution_budget);
        }
        LocaleStringMethod::Array => {}
    }

    let forwarded_arguments = vec![
        arguments.take_first_or_undefined(),
        arguments.take_first_or_undefined(),
    ];

    let target = match to_object_value(runtime, realm, receiver, origin.clone())? {
        Ok(target) => target,
        Err(exception) => return Err(NativeFailure::Abrupt(exception)),
    };
    let state = LocaleStringContinuation {
        method,
        target,
        element: None,
        arguments: forwarded_arguments,
        separator: JsString::from_utf8(",")?,
        accumulated: JsString::empty(),
        length: 0,
        next: 0,
        realm,
        stage: LocaleStringStage::AwaitLength,
        origin,
    };
    advance_locale_string(runtime, state, None, return_to, execution_budget)
}

/// Advances Object invocation or Array's locale-aware element loop.
#[allow(
    clippy::too_many_lines,
    clippy::needless_continue,
    reason = "the explicit stages preserve every getter, call, conversion, and separator boundary in specification order"
)]
pub(super) fn advance_locale_string(
    runtime: &mut Runtime,
    mut state: LocaleStringContinuation,
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
            LocaleStringStage::AwaitLength => {
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                let target = state.target.duplicate();
                await_get!(begin_locale_get(
                    runtime,
                    state,
                    &target,
                    key,
                    LocaleStringStage::AwaitLengthConversion,
                    return_to,
                    execution_budget,
                ));
            }
            LocaleStringStage::AwaitLengthConversion => {
                let value = take_locale_completion(&mut completion)?;
                if needs_locale_conversion(&value) {
                    return convert_locale_value(
                        runtime,
                        state,
                        value,
                        OperatorPrimitiveHint::Number,
                        return_to,
                        execution_budget,
                    );
                }
                state.length =
                    number_to_length(operator_to_number(value, state.realm, &state.origin)?);
                state.stage = LocaleStringStage::NextElement;
            }
            LocaleStringStage::NextElement => {
                if state.next >= state.length {
                    return Ok(NativeDispatch::Immediate(StoredValue::String(
                        state.accumulated,
                    )));
                }
                execution_budget.charge_instructions(1)?;
                if state.next > 0 {
                    state.accumulated = state.accumulated.concat(&state.separator)?;
                }
                let key = locale_element_key(runtime, state.next)?;
                state.next = state.next.saturating_add(1);
                let target = state.target.duplicate();
                await_get!(begin_locale_get(
                    runtime,
                    state,
                    &target,
                    key,
                    LocaleStringStage::AwaitElement,
                    return_to,
                    execution_budget,
                ));
            }
            LocaleStringStage::AwaitElement => {
                if state.element.is_none() {
                    state.element = Some(take_locale_completion(&mut completion)?);
                }
                if matches!(
                    state.element.as_ref(),
                    Some(StoredValue::Undefined | StoredValue::Null)
                ) {
                    state.element = None;
                    state.stage = LocaleStringStage::NextElement;
                } else {
                    state.stage = LocaleStringStage::ReadMethod;
                }
            }
            LocaleStringStage::ReadMethod => {
                let element = state
                    .element
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "locale-string invocation lost its receiver",
                    })?
                    .duplicate();
                let key = runtime.predefined_property_key(
                    if matches!(state.method, LocaleStringMethod::Object) {
                        PredefinedAtom::ToString
                    } else {
                        PredefinedAtom::ToLocaleString
                    },
                );
                await_get!(begin_locale_get(
                    runtime,
                    state,
                    &element,
                    key,
                    LocaleStringStage::AwaitMethod,
                    return_to,
                    execution_budget,
                ));
            }
            LocaleStringStage::AwaitMethod => {
                let method = take_locale_completion(&mut completion)?;
                let StoredValue::Function(function) = method else {
                    return Err(locale_type_error(
                        state.realm,
                        &state.origin,
                        "not a function",
                    ));
                };
                let receiver = state
                    .element
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "locale-string method lost its receiver",
                    })?
                    .duplicate();
                state.stage = LocaleStringStage::AwaitInvocation;
                return suspend_locale_string(state, function, receiver, return_to);
            }
            LocaleStringStage::AwaitInvocation => {
                let value = take_locale_completion(&mut completion)?;
                state.element = None;
                if matches!(state.method, LocaleStringMethod::Object) {
                    return Ok(NativeDispatch::Immediate(value));
                }
                if let StoredValue::String(text) = value {
                    state.accumulated = state.accumulated.concat(&text)?;
                    state.stage = LocaleStringStage::NextElement;
                } else {
                    state.stage = LocaleStringStage::AwaitElementString;
                    return convert_locale_value(
                        runtime,
                        state,
                        value,
                        OperatorPrimitiveHint::String,
                        return_to,
                        execution_budget,
                    );
                }
            }
            LocaleStringStage::AwaitElementString => {
                let value = take_locale_completion(&mut completion)?;
                let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
                state.accumulated = state.accumulated.concat(&text)?;
                state.stage = LocaleStringStage::NextElement;
            }
        }
    }
}

fn locale_string_continuation(state: LocaleStringContinuation) -> NativeContinuation {
    NativeContinuation::LocaleString(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the locale Get boundary keeps its receiver, next stage, caller continuation, and execution authority explicit"
)]
fn begin_locale_get(
    runtime: &mut Runtime,
    mut state: LocaleStringContinuation,
    base: &StoredValue,
    key: PropertyKey,
    next_stage: LocaleStringStage,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<GetContinuationDispatch<LocaleStringContinuation>, NativeFailure> {
    charge_locale_lookup(runtime, base, execution_budget)?;
    state.stage = next_stage;
    let dispatch = begin_value_get(
        runtime,
        base,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_state_after(
        dispatch,
        state,
        locale_string_continuation,
        "locale-string Get produced a structured result",
    )
}

fn convert_locale_value(
    runtime: &mut Runtime,
    state: LocaleStringContinuation,
    value: StoredValue,
    hint: OperatorPrimitiveHint,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        value,
        hint,
        OperatorPrimitiveTarget::LocaleStringValue(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn suspend_locale_string(
    state: LocaleStringContinuation,
    function: FunctionId,
    receiver: StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    let arguments = state.arguments.iter().map(StoredValue::duplicate).collect();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::LocaleString(Box::new(state)));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

fn locale_element_key(runtime: &mut Runtime, index: u64) -> Result<PropertyKey, NativeFailure> {
    if let Ok(index) = u32::try_from(index)
        && let Some(index) = ArrayIndex::new(index)
    {
        return Ok(PropertyKey::from_index(index));
    }
    let name = JsNumber::from_f64(index_as_f64(index)).to_javascript_string()?;
    Ok(runtime.property_key_from_string(&name)?)
}

fn charge_locale_lookup(
    runtime: &Runtime,
    base: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    if base.heap_reference().is_none() {
        execution_budget.charge_instructions(1)?;
        return Ok(());
    }
    charge_heap_property_lookup(runtime, base, execution_budget)
}

const fn needs_locale_conversion(value: &StoredValue) -> bool {
    matches!(value, StoredValue::Function(_) | StoredValue::Object(_))
}

#[expect(
    clippy::cast_precision_loss,
    reason = "ToLength bounds every index by 2^53 - 1, which binary64 represents exactly"
)]
fn index_as_f64(index: u64) -> f64 {
    index as f64
}

fn take_locale_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        EngineFault::RuntimeInvariant {
            message: "locale-string operation resumed without its awaited completion",
        }
        .into()
    })
}

fn locale_type_error(realm: RealmId, origin: &JsStackFrame, message: &str) -> NativeFailure {
    match JsString::from_utf8(message) {
        Ok(message) => NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message,
            },
            origin: origin.clone(),
        }),
        Err(error) => error.into(),
    }
}
