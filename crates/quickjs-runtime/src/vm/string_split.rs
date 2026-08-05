/*
 * JavaScript String.prototype.split semantics derived from QuickJS.
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
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! Resumable `String.prototype.split` protocol and plain-string fallback.
//!
//! The implementation follows the ES2025 ordering contract: `@@split` is read
//! before fallback coercion, including from non-null primitive separators, and
//! the fallback converts receiver, limit, and separator in that order.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::enum_variant_names,
    reason = "the Await prefix makes every observable split boundary explicit"
)]
enum StringSplitStage {
    AwaitSplitMethod,
    AwaitSubjectConversion,
    AwaitLimitConversion,
    AwaitSeparatorConversion,
}

/// One suspended `String.prototype.split` execution.
pub(super) struct StringSplitContinuation {
    receiver: StoredValue,
    separator: StoredValue,
    limit: StoredValue,
    subject: Option<JsString>,
    converted_limit: Option<u32>,
    separator_string: Option<JsString>,
    realm: RealmId,
    stage: StringSplitStage,
    origin: JsStackFrame,
}

impl StringSplitContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        3_u64
            .saturating_add(u64::from(self.subject.is_some()))
            .saturating_add(u64::from(self.separator_string.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.receiver, mark);
        trace_stored_value_root(&self.separator, mark);
        trace_stored_value_root(&self.limit, mark);
    }
}

/// Starts the ES2025 `@@split` protocol lookup or the plain-string fallback.
pub(super) fn begin_string_split(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(receiver, StoredValue::Undefined | StoredValue::Null) {
        return split_type_error(realm, origin, "null or undefined are forbidden");
    }

    let state = StringSplitContinuation {
        receiver,
        separator: arguments.take_first_or_undefined(),
        limit: arguments.take_first_or_undefined(),
        subject: None,
        converted_limit: None,
        separator_string: None,
        realm,
        stage: StringSplitStage::AwaitSplitMethod,
        origin,
    };

    if matches!(state.separator, StoredValue::Undefined | StoredValue::Null) {
        begin_split_fallback(runtime, state, return_to, execution_budget)
    } else {
        read_split_method(runtime, state, return_to, execution_budget)
    }
}

/// Resumes a protocol getter or one of the fallback primitive conversions.
pub(super) fn advance_string_split(
    runtime: &mut Runtime,
    mut state: StringSplitContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        StringSplitStage::AwaitSplitMethod => {
            decide_split_method(runtime, state, &completion, return_to, execution_budget)
        }
        StringSplitStage::AwaitSubjectConversion => {
            state.subject = Some(operator_primitive_to_string(
                completion,
                state.realm,
                &state.origin,
            )?);
            begin_limit_conversion(runtime, state, return_to, execution_budget)
        }
        StringSplitStage::AwaitLimitConversion => {
            let number = operator_to_number(completion, state.realm, &state.origin)?;
            state.converted_limit = Some(number_to_uint32(number));
            begin_separator_conversion(runtime, state, return_to, execution_budget)
        }
        StringSplitStage::AwaitSeparatorConversion => {
            state.separator_string = Some(operator_primitive_to_string(
                completion,
                state.realm,
                &state.origin,
            )?);
            finish_plain_split(runtime, &state, execution_budget)
        }
    }
}

fn read_split_method(
    runtime: &mut Runtime,
    mut state: StringSplitContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = StringSplitStage::AwaitSplitMethod;
    charge_split_property_lookup(runtime, state.realm, &state.separator, execution_budget)?;
    let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolSplit);
    match read_static_property(runtime, state.realm, &state.separator, &key)? {
        PropertyReadOutcome::Value(value) => {
            decide_split_method(runtime, state, &value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => call_split_function(
            function,
            receiver,
            CallArguments::empty(),
            state.origin.clone(),
            Some(state),
            return_to,
        ),
        PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            state.realm,
            state.origin,
            Some(&JsString::from_utf8("Symbol.split")?),
            failure,
        )?)),
    }
}

fn decide_split_method(
    runtime: &mut Runtime,
    state: StringSplitContinuation,
    method: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        StoredValue::Undefined | StoredValue::Null => {
            begin_split_fallback(runtime, state, return_to, execution_budget)
        }
        StoredValue::Function(function) => {
            let receiver = state.separator.duplicate();
            let mut values = Vec::new();
            values
                .try_reserve_exact(2)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 2,
                })?;
            values.push(state.receiver.duplicate());
            values.push(state.limit.duplicate());
            call_split_function(
                *function,
                receiver,
                CallArguments::from_values(values),
                state.origin,
                None,
                return_to,
            )
        }
        StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Object(_) => split_type_error(state.realm, state.origin, "not a function"),
    }
}

fn charge_split_property_lookup(
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
                message: "String split property lookup received a nullish base",
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

fn begin_split_fallback(
    runtime: &mut Runtime,
    mut state: StringSplitContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = StringSplitStage::AwaitSubjectConversion;
    let value = state.receiver.duplicate();
    convert_split_value(
        runtime,
        state,
        value,
        OperatorPrimitiveHint::String,
        return_to,
        execution_budget,
    )
}

fn begin_limit_conversion(
    runtime: &mut Runtime,
    mut state: StringSplitContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.limit, StoredValue::Undefined) {
        state.converted_limit = Some(u32::MAX);
        return begin_separator_conversion(runtime, state, return_to, execution_budget);
    }
    state.stage = StringSplitStage::AwaitLimitConversion;
    let value = state.limit.duplicate();
    convert_split_value(
        runtime,
        state,
        value,
        OperatorPrimitiveHint::Number,
        return_to,
        execution_budget,
    )
}

fn begin_separator_conversion(
    runtime: &mut Runtime,
    mut state: StringSplitContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = StringSplitStage::AwaitSeparatorConversion;
    let value = state.separator.duplicate();
    convert_split_value(
        runtime,
        state,
        value,
        OperatorPrimitiveHint::String,
        return_to,
        execution_budget,
    )
}

fn convert_split_value(
    runtime: &mut Runtime,
    state: StringSplitContinuation,
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
        OperatorPrimitiveTarget::StringSplitValue(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn finish_plain_split(
    runtime: &mut Runtime,
    state: &StringSplitContinuation,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let subject = state
        .subject
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "String split lost its converted subject",
        })?;
    let separator = state
        .separator_string
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "String split lost its converted separator",
        })?;
    let limit = state.converted_limit.ok_or(EngineFault::RuntimeInvariant {
        message: "String split lost its converted limit",
    })?;

    let mut elements = Vec::new();
    if limit == 0 {
        execution_budget.charge_instructions(1)?;
        return split_array(runtime, state.realm, elements);
    }
    if matches!(state.separator, StoredValue::Undefined) {
        execution_budget.charge_instructions(1)?;
        push_split_element(&mut elements, subject.clone())?;
        return split_array(runtime, state.realm, elements);
    }
    if subject.is_empty() {
        execution_budget.charge_instructions(1)?;
        if !separator.is_empty() {
            push_split_element(&mut elements, subject.clone())?;
        }
        return split_array(runtime, state.realm, elements);
    }
    if separator.is_empty() {
        execution_budget.charge_instructions(u64::from(subject.len()).saturating_add(1))?;
        let count = subject.len().min(limit);
        elements
            .try_reserve_exact(usize::try_from(count).unwrap_or(usize::MAX))
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: usize::try_from(count).unwrap_or(usize::MAX),
            })?;
        for index in 0..count {
            elements.push(StoredValue::String(subject.slice(index..index + 1)?));
        }
        return split_array(runtime, state.realm, elements);
    }

    execution_budget.charge_instructions(
        u64::from(subject.len())
            .saturating_mul(u64::from(separator.len()))
            .saturating_add(u64::from(subject.len()))
            .saturating_add(1),
    )?;

    let mut position = 0;
    while let Some(found) = find_forward(subject, separator, position) {
        push_split_element(&mut elements, subject.slice(position..found)?)?;
        if u32::try_from(elements.len()).unwrap_or(u32::MAX) == limit {
            return split_array(runtime, state.realm, elements);
        }
        position = found
            .checked_add(separator.len())
            .ok_or(EngineFault::RuntimeInvariant {
                message: "String split match endpoint overflowed",
            })?;
    }
    push_split_element(&mut elements, subject.slice(position..subject.len())?)?;
    split_array(runtime, state.realm, elements)
}

fn push_split_element(
    elements: &mut Vec<StoredValue>,
    value: JsString,
) -> Result<(), NativeFailure> {
    elements
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: 1,
        })?;
    elements.push(StoredValue::String(value));
    Ok(())
}

fn split_array(
    runtime: &mut Runtime,
    realm: RealmId,
    elements: Vec<StoredValue>,
) -> Result<NativeDispatch, NativeFailure> {
    Ok(NativeDispatch::Immediate(StoredValue::Object(
        runtime.allocate_array(realm, elements)?,
    )))
}

fn call_split_function(
    function: FunctionId,
    receiver: StoredValue,
    arguments: CallArguments,
    origin: JsStackFrame,
    continuation: Option<StringSplitContinuation>,
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
        continuations.push(NativeContinuation::StringSplit(Box::new(continuation)));
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

fn split_type_error(
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
