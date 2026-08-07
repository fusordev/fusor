/*
 * JavaScript String.prototype.replace semantics derived from QuickJS.
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

//! Resumable `String.prototype.replace`/`replaceAll` protocol and plain-string
//! fallback.
//!
//! The protocol lookup deliberately precedes every fallback `ToString`.
//! `replaceAll` additionally performs `IsRegExp`, including observable
//! `@@match`, `flags`, and flags-string conversion, before `@@replace`.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::enum_variant_names,
    reason = "the Await prefix makes every observable replace boundary explicit"
)]
enum StringReplaceStage {
    AwaitMatchProperty,
    AwaitFlagsProperty,
    AwaitFlagsConversion,
    AwaitReplaceMethod,
    AwaitSubjectConversion,
    AwaitSearchConversion,
    AwaitReplacementConversion,
    AwaitFunctionalReplacement,
    AwaitFunctionalResultConversion,
}

/// One suspended `String.prototype.replace` execution.
pub(super) struct StringReplaceContinuation {
    replace_all: bool,
    receiver: StoredValue,
    search_value: StoredValue,
    replace_value: StoredValue,
    subject: Option<JsString>,
    search_string: Option<JsString>,
    replacement_string: Option<JsString>,
    position: Option<u32>,
    end_of_last_match: u32,
    result: JsString,
    realm: RealmId,
    stage: StringReplaceStage,
    origin: JsStackFrame,
}

impl StringReplaceContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        3_u64
            .saturating_add(u64::from(self.subject.is_some()))
            .saturating_add(u64::from(self.search_string.is_some()))
            .saturating_add(u64::from(self.replacement_string.is_some()))
            .saturating_add(1)
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.receiver, mark);
        trace_stored_value_root(&self.search_value, mark);
        trace_stored_value_root(&self.replace_value, mark);
    }
}

/// Starts the `@@replace` protocol lookup or the plain-string fallback.
#[expect(
    clippy::too_many_arguments,
    reason = "the shared replace entry point carries its method identity alongside the same receiver, arguments, and resumption context every native dispatch takes"
)]
pub(super) fn begin_string_replace(
    runtime: &mut Runtime,
    replace_all: bool,
    realm: RealmId,
    receiver: StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(receiver, StoredValue::Undefined | StoredValue::Null) {
        return replace_type_error(realm, origin, "null or undefined are forbidden");
    }

    let state = StringReplaceContinuation {
        replace_all,
        receiver,
        search_value: arguments.take_first_or_undefined(),
        replace_value: arguments.take_first_or_undefined(),
        subject: None,
        search_string: None,
        replacement_string: None,
        position: None,
        end_of_last_match: 0,
        result: JsString::empty(),
        realm,
        stage: if replace_all {
            StringReplaceStage::AwaitMatchProperty
        } else {
            StringReplaceStage::AwaitReplaceMethod
        },
        origin,
    };

    if matches!(
        state.search_value,
        StoredValue::Undefined | StoredValue::Null
    ) {
        return begin_replace_fallback(runtime, state, return_to, execution_budget);
    }
    if replace_all
        && matches!(
            state.search_value,
            StoredValue::Function(_) | StoredValue::Object(_)
        )
    {
        return read_match_property(runtime, state, return_to, execution_budget);
    }
    read_replace_method(runtime, state, return_to, execution_budget)
}

/// Resumes a protocol getter, fallback conversion, replacer call, or callback
/// result conversion.
pub(super) fn advance_string_replace(
    runtime: &mut Runtime,
    mut state: StringReplaceContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        StringReplaceStage::AwaitMatchProperty => {
            decide_regexp_like(runtime, state, &completion, return_to, execution_budget)
        }
        StringReplaceStage::AwaitFlagsProperty => {
            begin_flags_conversion(runtime, state, completion, return_to, execution_budget)
        }
        StringReplaceStage::AwaitFlagsConversion => {
            let flags = operator_primitive_to_string(completion, state.realm, &state.origin)?;
            if !(0..flags.len()).any(|index| flags.code_unit_at(index) == Some(u16::from(b'g'))) {
                return replace_type_error(
                    state.realm,
                    state.origin,
                    "regexp must have the 'g' flag",
                );
            }
            read_replace_method(runtime, state, return_to, execution_budget)
        }
        StringReplaceStage::AwaitReplaceMethod => {
            decide_replace_method(runtime, state, &completion, return_to, execution_budget)
        }
        StringReplaceStage::AwaitSubjectConversion => {
            state.subject = Some(operator_primitive_to_string(
                completion,
                state.realm,
                &state.origin,
            )?);
            state.stage = StringReplaceStage::AwaitSearchConversion;
            let value = state.search_value.duplicate();
            convert_replace_value(runtime, state, value, return_to, execution_budget)
        }
        StringReplaceStage::AwaitSearchConversion => {
            state.search_string = Some(operator_primitive_to_string(
                completion,
                state.realm,
                &state.origin,
            )?);
            if matches!(state.replace_value, StoredValue::Function(_)) {
                prepare_functional_replacement(state, return_to, execution_budget)
            } else {
                state.stage = StringReplaceStage::AwaitReplacementConversion;
                let value = state.replace_value.duplicate();
                convert_replace_value(runtime, state, value, return_to, execution_budget)
            }
        }
        StringReplaceStage::AwaitReplacementConversion => {
            state.replacement_string = Some(operator_primitive_to_string(
                completion,
                state.realm,
                &state.origin,
            )?);
            prepare_template_replacement(state, execution_budget)
        }
        StringReplaceStage::AwaitFunctionalReplacement => {
            state.stage = StringReplaceStage::AwaitFunctionalResultConversion;
            convert_replace_value(runtime, state, completion, return_to, execution_budget)
        }
        StringReplaceStage::AwaitFunctionalResultConversion => {
            let replacement = operator_primitive_to_string(completion, state.realm, &state.origin)?;
            append_replacement(&mut state, &replacement, execution_budget)?;
            continue_functional_replacement(state, return_to, execution_budget)
        }
    }
}

fn read_match_property(
    runtime: &mut Runtime,
    mut state: StringReplaceContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = StringReplaceStage::AwaitMatchProperty;
    charge_replace_property_lookup(runtime, state.realm, &state.search_value, execution_budget)?;
    let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolMatch);
    begin_string_replace_get(
        runtime,
        state,
        key,
        "Symbol.match",
        return_to,
        execution_budget,
    )
}

fn decide_regexp_like(
    runtime: &mut Runtime,
    state: StringReplaceContinuation,
    matcher: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let branded = match state.search_value {
        StoredValue::Object(object) => runtime.regexp_state(object)?.is_some(),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Function(_) => false,
    };
    if !branded && (matches!(matcher, StoredValue::Undefined) || !matcher.is_truthy()) {
        return read_replace_method(runtime, state, return_to, execution_budget);
    }
    read_flags_property(runtime, state, return_to, execution_budget)
}

fn read_flags_property(
    runtime: &mut Runtime,
    mut state: StringReplaceContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = StringReplaceStage::AwaitFlagsProperty;
    charge_replace_property_lookup(runtime, state.realm, &state.search_value, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::Flags);
    begin_string_replace_get(runtime, state, key, "flags", return_to, execution_budget)
}

fn begin_flags_conversion(
    runtime: &mut Runtime,
    mut state: StringReplaceContinuation,
    flags: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(flags, StoredValue::Undefined | StoredValue::Null) {
        return replace_type_error(state.realm, state.origin, "cannot convert to object");
    }
    state.stage = StringReplaceStage::AwaitFlagsConversion;
    convert_replace_value(runtime, state, flags, return_to, execution_budget)
}

fn read_replace_method(
    runtime: &mut Runtime,
    mut state: StringReplaceContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = StringReplaceStage::AwaitReplaceMethod;
    charge_replace_property_lookup(runtime, state.realm, &state.search_value, execution_budget)?;
    let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolReplace);
    begin_string_replace_get(
        runtime,
        state,
        key,
        "Symbol.replace",
        return_to,
        execution_budget,
    )
}

fn begin_string_replace_get(
    runtime: &mut Runtime,
    state: StringReplaceContinuation,
    key: PropertyKey,
    diagnostic_name: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let name = JsString::from_utf8(diagnostic_name)?;
    let dispatch = begin_value_get(
        runtime,
        &state.search_value,
        key,
        Some(&name),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        string_replace_continuation,
        |state, value| advance_string_replace(runtime, state, value, return_to, execution_budget),
        "String replace Get produced a structured result",
    )
}

fn string_replace_continuation(state: StringReplaceContinuation) -> NativeContinuation {
    NativeContinuation::StringReplace(Box::new(state))
}

fn decide_replace_method(
    runtime: &mut Runtime,
    state: StringReplaceContinuation,
    method: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        StoredValue::Undefined | StoredValue::Null => {
            begin_replace_fallback(runtime, state, return_to, execution_budget)
        }
        StoredValue::Function(function) => {
            let receiver = state.search_value.duplicate();
            let mut values = Vec::new();
            values
                .try_reserve_exact(2)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 2,
                })?;
            values.push(state.receiver.duplicate());
            values.push(state.replace_value.duplicate());
            let origin = state.origin.clone();
            call_replace_function(
                *function,
                receiver,
                CallArguments::from_values(values),
                origin,
                None,
                return_to,
            )
        }
        StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Object(_) => replace_type_error(state.realm, state.origin, "not a function"),
    }
}

fn charge_replace_property_lookup(
    runtime: &Runtime,
    realm: RealmId,
    base: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    let prototype = match base {
        StoredValue::Boolean(_) => Some(runtime.realm_boolean_prototype(realm)?),
        StoredValue::Number(_) => Some(runtime.realm_number_prototype(realm)?),
        StoredValue::BigInt(_) => Some(runtime.realm_bigint_prototype(realm)?),
        StoredValue::String(_) => Some(runtime.realm_string_prototype(realm)?),
        StoredValue::Symbol(_) => Some(runtime.realm_symbol_prototype(realm)?),
        StoredValue::Function(_) | StoredValue::Object(_) => None,
        StoredValue::Undefined | StoredValue::Null => {
            return Err(EngineFault::RuntimeInvariant {
                message: "String replacement property lookup received a nullish base",
            }
            .into());
        }
    };
    if let Some(prototype) = prototype {
        charge_heap_property_lookup(runtime, &StoredValue::Object(prototype), execution_budget)
    } else {
        charge_heap_property_lookup(runtime, base, execution_budget)
    }
}

fn begin_replace_fallback(
    runtime: &mut Runtime,
    mut state: StringReplaceContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = StringReplaceStage::AwaitSubjectConversion;
    let value = state.receiver.duplicate();
    convert_replace_value(runtime, state, value, return_to, execution_budget)
}

fn convert_replace_value(
    runtime: &mut Runtime,
    state: StringReplaceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::String,
        OperatorPrimitiveTarget::StringReplaceValue(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn prepare_functional_replacement(
    mut state: StringReplaceContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let position = locate_replacement(&state, execution_budget)?;
    let Some(position) = position else {
        return unchanged_subject(&state);
    };
    state.position = Some(position);
    call_functional_replacer(state, return_to)
}

fn call_functional_replacer(
    mut state: StringReplaceContinuation,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let position = state.position.ok_or(EngineFault::RuntimeInvariant {
        message: "functional String replacement lost its match position",
    })?;

    let StoredValue::Function(function) = &state.replace_value else {
        return Err(EngineFault::RuntimeInvariant {
            message: "functional String replacement lost its callable",
        }
        .into());
    };
    let function = *function;
    let subject = required_subject(&state)?.clone();
    let search = required_search(&state)?.clone();
    let mut values = Vec::new();
    values
        .try_reserve_exact(3)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 3,
        })?;
    values.push(StoredValue::String(search));
    values.push(StoredValue::Number(JsNumber::from_f64(f64::from(position))));
    values.push(StoredValue::String(subject));
    state.stage = StringReplaceStage::AwaitFunctionalReplacement;
    let origin = state.origin.clone();
    call_replace_function(
        function,
        StoredValue::Undefined,
        CallArguments::from_values(values),
        origin,
        Some(state),
        return_to,
    )
}

fn prepare_template_replacement(
    mut state: StringReplaceContinuation,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let position = locate_replacement(&state, execution_budget)?;
    let Some(position) = position else {
        return unchanged_subject(&state);
    };
    state.position = Some(position);
    let template = state
        .replacement_string
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "String replacement completed without replacement text",
        })?
        .clone();
    loop {
        let position = state.position.ok_or(EngineFault::RuntimeInvariant {
            message: "String replacement completed without a match position",
        })?;
        let replacement = get_string_substitution(
            required_subject(&state)?,
            required_search(&state)?,
            position,
            &template,
            execution_budget,
        )?;
        append_replacement(&mut state, &replacement, execution_budget)?;
        let Some(next) = next_replacement_position(&state)? else {
            break;
        };
        state.position = Some(next);
    }
    finish_replacement_sequence(&state, execution_budget)
}

fn locate_replacement(
    state: &StringReplaceContinuation,
    execution_budget: &mut ExecutionBudget,
) -> Result<Option<u32>, NativeFailure> {
    let subject = required_subject(state)?;
    let search = required_search(state)?;
    execution_budget.charge_instructions(
        u64::from(subject.len())
            .saturating_mul(u64::from(search.len()).max(1))
            .saturating_add(1),
    )?;
    Ok(find_forward(subject, search, 0))
}

fn unchanged_subject(state: &StringReplaceContinuation) -> Result<NativeDispatch, NativeFailure> {
    Ok(NativeDispatch::Immediate(StoredValue::String(
        required_subject(state)?.clone(),
    )))
}

fn continue_functional_replacement(
    mut state: StringReplaceContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(next) = next_replacement_position(&state)? else {
        return finish_replacement_sequence(&state, execution_budget);
    };
    state.position = Some(next);
    call_functional_replacer(state, return_to)
}

fn next_replacement_position(
    state: &StringReplaceContinuation,
) -> Result<Option<u32>, EngineFault> {
    if !state.replace_all {
        return Ok(None);
    }
    let position = state.position.ok_or(EngineFault::RuntimeInvariant {
        message: "String replacement sequence lost its match position",
    })?;
    let search = required_search(state)?;
    let advance = search.len().max(1);
    let Some(start) = position.checked_add(advance) else {
        return Ok(None);
    };
    Ok(find_forward(required_subject(state)?, search, start))
}

fn append_replacement(
    state: &mut StringReplaceContinuation,
    replacement: &JsString,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    let position = state.position.ok_or(EngineFault::RuntimeInvariant {
        message: "String replacement completed without a match position",
    })?;
    let (preserved, endpoint) = {
        let subject = required_subject(state)?;
        let search = required_search(state)?;
        let endpoint = position
            .checked_add(search.len())
            .ok_or(EngineFault::RuntimeInvariant {
                message: "String replacement match endpoint overflowed",
            })?;
        (subject.slice(state.end_of_last_match..position)?, endpoint)
    };
    execution_budget.charge_instructions(
        u64::from(preserved.len())
            .saturating_add(u64::from(replacement.len()))
            .saturating_add(1),
    )?;
    state.result = state.result.concat(&preserved)?.concat(replacement)?;
    state.end_of_last_match = endpoint;
    Ok(())
}

fn finish_replacement_sequence(
    state: &StringReplaceContinuation,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let subject = required_subject(state)?;
    let following = subject.slice(state.end_of_last_match..subject.len())?;
    execution_budget.charge_instructions(u64::from(following.len()).saturating_add(1))?;
    let result = state.result.concat(&following)?;
    Ok(NativeDispatch::Immediate(StoredValue::String(result)))
}

/// Implements `GetSubstitution` for the plain-string path, whose captures list
/// is empty and whose named captures value is `undefined`.
fn get_string_substitution(
    subject: &JsString,
    matched: &JsString,
    position: u32,
    template: &JsString,
    execution_budget: &mut ExecutionBudget,
) -> Result<JsString, NativeFailure> {
    execution_budget.charge_instructions(u64::from(template.len()).saturating_add(1))?;
    let mut result = JsString::empty();
    let mut index = 0;
    while index < template.len() {
        let current = template
            .code_unit_at(index)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "replacement template traversal read past its bound",
            })?;
        if current == u16::from(b'$')
            && let Some(next) = template.code_unit_at(index.saturating_add(1))
        {
            let substitution = match next {
                unit if unit == u16::from(b'$') => Some(template.slice(index..index + 1)?),
                unit if unit == u16::from(b'`') => Some(subject.slice(0..position)?),
                unit if unit == u16::from(b'&') => Some(matched.clone()),
                unit if unit == u16::from(b'\'') => {
                    let tail = position.checked_add(matched.len()).ok_or(
                        EngineFault::RuntimeInvariant {
                            message: "replacement substitution endpoint overflowed",
                        },
                    )?;
                    Some(subject.slice(tail.min(subject.len())..subject.len())?)
                }
                _ => None,
            };
            if let Some(substitution) = substitution {
                execution_budget
                    .charge_instructions(u64::from(substitution.len()).saturating_add(1))?;
                result = result.concat(&substitution)?;
                index = index.saturating_add(2);
                continue;
            }
        }
        execution_budget.charge_instructions(1)?;
        result = result.concat(&template.slice(index..index + 1)?)?;
        index = index.saturating_add(1);
    }
    Ok(result)
}

fn required_subject(state: &StringReplaceContinuation) -> Result<&JsString, EngineFault> {
    state.subject.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "String replacement lost its converted subject",
    })
}

fn required_search(state: &StringReplaceContinuation) -> Result<&JsString, EngineFault> {
    state
        .search_string
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "String replacement lost its converted search string",
        })
}

fn call_replace_function(
    function: FunctionId,
    receiver: StoredValue,
    arguments: CallArguments,
    origin: JsStackFrame,
    continuation: Option<StringReplaceContinuation>,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let mut continuations = Vec::new();
    if let Some(continuation) = continuation {
        continuations
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::Frames,
                additional: 1,
            })?;
        continuations.push(NativeContinuation::StringReplace(Box::new(continuation)));
    }
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments,
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

fn replace_type_error(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}
