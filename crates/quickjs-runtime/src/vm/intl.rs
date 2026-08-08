//! Resumable ECMA-402 locale-list canonicalization.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

use quickjs_intl::canonicalize_locale;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlLocaleListStage {
    AwaitLength,
    AwaitLengthConversion,
    Next,
    AwaitHas,
    AwaitElement,
    AwaitElementString,
}

/// One suspended `CanonicalizeLocaleList` operation.
pub(super) struct IntlLocaleListContinuation {
    source: StoredValue,
    seen: Vec<StoredValue>,
    index: u64,
    length: u64,
    realm: RealmId,
    stage: IntlLocaleListStage,
    origin: JsStackFrame,
}

impl IntlLocaleListContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(usize_to_u64(self.seen.len()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.source, mark);
        for value in &self.seen {
            trace_stored_value_root(value, mark);
        }
    }
}

pub(super) fn begin_intl_get_canonical_locales(
    runtime: &mut Runtime,
    locales: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match locales {
        StoredValue::Undefined => finish_intl_locale_list(runtime, realm, Vec::new()),
        StoredValue::String(locale) => {
            let canonical = canonicalize_js_locale(&locale, realm, &origin, execution_budget)?;
            finish_intl_locale_list(
                runtime,
                realm,
                intl_one_value(StoredValue::String(canonical))?,
            )
        }
        value => {
            let source = match to_object_value(runtime, realm, value, origin.clone())? {
                Ok(source) => source,
                Err(exception) => return Err(NativeFailure::Abrupt(exception)),
            };
            let state = IntlLocaleListContinuation {
                source,
                seen: Vec::new(),
                index: 0,
                length: 0,
                realm,
                stage: IntlLocaleListStage::AwaitLength,
                origin,
            };
            read_intl_locale_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Length),
                "length",
                return_to,
                execution_budget,
            )
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the typed resumable CanonicalizeLocaleList state transitions stay together for ordering audits"
)]
pub(super) fn advance_intl_locale_list(
    runtime: &mut Runtime,
    mut state: IntlLocaleListContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            IntlLocaleListStage::AwaitLength => {
                let value = take_intl_completion(&mut completion)?;
                state.stage = IntlLocaleListStage::AwaitLengthConversion;
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::Number,
                        OperatorPrimitiveTarget::IntlLocaleListLength(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                return finish_intl_locale_list_length(
                    runtime,
                    state,
                    value,
                    return_to,
                    execution_budget,
                );
            }
            IntlLocaleListStage::AwaitLengthConversion => {
                return finish_intl_locale_list_length(
                    runtime,
                    state,
                    take_intl_completion(&mut completion)?,
                    return_to,
                    execution_budget,
                );
            }
            IntlLocaleListStage::Next => {
                if state.index >= state.length {
                    return finish_intl_locale_list(runtime, state.realm, state.seen);
                }
                execution_budget.charge_instructions(1)?;
                let key = array_static_index_key(runtime, state.index)?;
                state.stage = IntlLocaleListStage::AwaitHas;
                return has_intl_locale_property(runtime, state, key, return_to, execution_budget);
            }
            IntlLocaleListStage::AwaitHas => {
                let StoredValue::Boolean(present) = take_intl_completion(&mut completion)? else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "CanonicalizeLocaleList HasProperty returned a non-Boolean",
                    }
                    .into());
                };
                if !present {
                    state.index = state.index.saturating_add(1);
                    state.stage = IntlLocaleListStage::Next;
                    continue;
                }
                let key = array_static_index_key(runtime, state.index)?;
                state.stage = IntlLocaleListStage::AwaitElement;
                return read_intl_locale_property(
                    runtime,
                    state,
                    key,
                    "locale",
                    return_to,
                    execution_budget,
                );
            }
            IntlLocaleListStage::AwaitElement => {
                let value = take_intl_completion(&mut completion)?;
                match value {
                    StoredValue::String(locale) => {
                        return finish_intl_locale_list_element(
                            runtime,
                            state,
                            &locale,
                            return_to,
                            execution_budget,
                        );
                    }
                    value @ (StoredValue::Function(_) | StoredValue::Object(_)) => {
                        state.stage = IntlLocaleListStage::AwaitElementString;
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::String,
                            OperatorPrimitiveTarget::IntlLocaleListElement(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    StoredValue::Undefined
                    | StoredValue::Null
                    | StoredValue::Boolean(_)
                    | StoredValue::Number(_)
                    | StoredValue::BigInt(_)
                    | StoredValue::Symbol(_) => {
                        return intl_locale_list_error(
                            state.realm,
                            state.origin,
                            ExceptionKind::TypeError,
                            "locale list element is not a string or object",
                        );
                    }
                }
            }
            IntlLocaleListStage::AwaitElementString => {
                let value = take_intl_completion(&mut completion)?;
                let locale = operator_primitive_to_string(value, state.realm, &state.origin)?;
                return finish_intl_locale_list_element(
                    runtime,
                    state,
                    &locale,
                    return_to,
                    execution_budget,
                );
            }
        }
    }
}

pub(super) fn finish_intl_locale_list_length(
    runtime: &mut Runtime,
    mut state: IntlLocaleListContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.length = number_to_length(operator_to_number(value, state.realm, &state.origin)?);
    state.stage = IntlLocaleListStage::Next;
    advance_intl_locale_list(runtime, state, None, return_to, execution_budget)
}

pub(super) fn finish_intl_locale_list_element(
    runtime: &mut Runtime,
    mut state: IntlLocaleListContinuation,
    locale: &JsString,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let canonical = canonicalize_js_locale(locale, state.realm, &state.origin, execution_budget)?;
    if !state
        .seen
        .iter()
        .any(|candidate| matches!(candidate, StoredValue::String(value) if value == &canonical))
    {
        state
            .seen
            .try_reserve(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::Frames,
                additional: 1,
            })?;
        state.seen.push(StoredValue::String(canonical));
    }
    state.index = state.index.saturating_add(1);
    state.stage = IntlLocaleListStage::Next;
    advance_intl_locale_list(runtime, state, None, return_to, execution_budget)
}

fn canonicalize_js_locale(
    locale: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<JsString, NativeFailure> {
    execution_budget.charge_instructions(u64::from(locale.len()).saturating_add(1))?;
    let input = locale.to_utf8_lossy()?;
    let canonical = canonicalize_locale(&input).map_err(|_| {
        NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("invalid language tag")
                    .expect("static Intl error message is valid"),
            },
            origin: origin.clone(),
        })
    })?;
    Ok(JsString::from_utf8(&canonical)?)
}

fn finish_intl_locale_list(
    runtime: &mut Runtime,
    realm: RealmId,
    locales: Vec<StoredValue>,
) -> Result<NativeDispatch, NativeFailure> {
    Ok(NativeDispatch::Immediate(StoredValue::Object(
        runtime.allocate_array(realm, locales)?,
    )))
}

fn read_intl_locale_property(
    runtime: &mut Runtime,
    state: IntlLocaleListContinuation,
    key: PropertyKey,
    diagnostic_name: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = state.source.duplicate();
    charge_heap_property_lookup(runtime, &base, execution_budget)?;
    let name = JsString::from_utf8(diagnostic_name)?;
    let dispatch = begin_value_get(
        runtime,
        &base,
        key,
        Some(&name),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_intl_locale_after(dispatch, state, runtime, return_to, execution_budget)
}

fn has_intl_locale_property(
    runtime: &mut Runtime,
    state: IntlLocaleListContinuation,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = state.source.duplicate();
    charge_heap_property_lookup(runtime, &base, execution_budget)?;
    let dispatch = begin_value_has(
        runtime,
        &base,
        key,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_intl_locale_after(dispatch, state, runtime, return_to, execution_budget)
}

fn continue_intl_locale_after(
    dispatch: NativeDispatch,
    state: IntlLocaleListContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        intl_locale_list_continuation,
        |state, value| {
            advance_intl_locale_list(runtime, state, Some(value), return_to, execution_budget)
        },
        "CanonicalizeLocaleList property operation produced a structured result",
    )
}

fn intl_locale_list_continuation(state: IntlLocaleListContinuation) -> NativeContinuation {
    NativeContinuation::IntlLocaleList(Box::new(state))
}

fn take_intl_completion(completion: &mut Option<StoredValue>) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "CanonicalizeLocaleList resumed without a completion",
    })
}

fn intl_one_value(value: StoredValue) -> Result<Vec<StoredValue>, NativeFailure> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    values.push(value);
    Ok(values)
}

fn intl_locale_list_error<T>(
    realm: RealmId,
    origin: JsStackFrame,
    kind: ExceptionKind,
    message: &str,
) -> Result<T, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}
