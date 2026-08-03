/*
 * JavaScript String.raw semantics derived from QuickJS.
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

//! Resumable `String.raw` array-like traversal and UTF-16 concatenation.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::enum_variant_names,
    reason = "the Await prefix makes every observable String.raw boundary explicit"
)]
enum StringRawStage {
    AwaitRaw,
    AwaitLength,
    AwaitLengthConversion,
    AwaitLiteral,
    AwaitLiteralConversion,
    AwaitSubstitutionConversion,
}

/// One suspended `String.raw` execution.
pub(crate) struct StringRawContinuation {
    cooked: StoredValue,
    literals: Option<StoredValue>,
    substitutions: Vec<StoredValue>,
    result: JsString,
    literal_count: u64,
    next_index: u64,
    realm: RealmId,
    stage: StringRawStage,
    origin: JsStackFrame,
}

impl StringRawContinuation {
    pub(crate) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.literals.is_some()))
            .saturating_add(usize_to_u64(self.substitutions.len()))
    }

    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.cooked, mark);
        if let Some(literals) = &self.literals {
            trace_stored_value_root(literals, mark);
        }
        for substitution in &self.substitutions {
            trace_stored_value_root(substitution, mark);
        }
    }
}

/// Performs `ToObject(template)` before the observable `raw` lookup.
pub(super) fn begin_string_raw(
    runtime: &mut Runtime,
    realm: RealmId,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let template = arguments.take_first_or_undefined();
    let cooked = match to_object_value(runtime, realm, template, origin.clone())? {
        Ok(cooked) => cooked,
        Err(exception) => return Err(NativeFailure::Abrupt(exception)),
    };
    let substitutions = arguments.into_remaining_values();
    let state = StringRawContinuation {
        cooked,
        literals: None,
        substitutions,
        result: JsString::empty(),
        literal_count: 0,
        next_index: 0,
        realm,
        stage: StringRawStage::AwaitRaw,
        origin,
    };
    read_string_raw_property(
        runtime,
        state,
        runtime.predefined_property_key(PredefinedAtom::Raw),
        "raw",
        return_to,
        execution_budget,
    )
}

/// Advances one raw lookup, length conversion, indexed getter, or string
/// conversion boundary.
pub(super) fn advance_string_raw(
    runtime: &mut Runtime,
    mut state: StringRawContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let completion = take_string_raw_completion(completion)?;
    match state.stage {
        StringRawStage::AwaitRaw => {
            let literals =
                match to_object_value(runtime, state.realm, completion, state.origin.clone())? {
                    Ok(literals) => literals,
                    Err(exception) => return Err(NativeFailure::Abrupt(exception)),
                };
            state.literals = Some(literals);
            state.stage = StringRawStage::AwaitLength;
            read_string_raw_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Length),
                "length",
                return_to,
                execution_budget,
            )
        }
        StringRawStage::AwaitLength => {
            state.stage = StringRawStage::AwaitLengthConversion;
            convert_string_raw_value(
                runtime,
                state,
                completion,
                OperatorPrimitiveHint::Number,
                return_to,
                execution_budget,
            )
        }
        StringRawStage::AwaitLengthConversion => {
            state.literal_count =
                number_to_length(operator_to_number(completion, state.realm, &state.origin)?);
            if state.literal_count == 0 {
                return Ok(NativeDispatch::Immediate(StoredValue::String(state.result)));
            }
            read_next_string_raw_literal(runtime, state, return_to, execution_budget)
        }
        StringRawStage::AwaitLiteral => {
            state.stage = StringRawStage::AwaitLiteralConversion;
            convert_string_raw_value(
                runtime,
                state,
                completion,
                OperatorPrimitiveHint::String,
                return_to,
                execution_budget,
            )
        }
        StringRawStage::AwaitLiteralConversion => {
            let literal = operator_primitive_to_string(completion, state.realm, &state.origin)?;
            append_string_raw_text(&mut state, &literal, execution_budget)?;
            if state.next_index.saturating_add(1) == state.literal_count {
                return Ok(NativeDispatch::Immediate(StoredValue::String(state.result)));
            }
            let substitution_index = usize::try_from(state.next_index).ok();
            if let Some(substitution) = substitution_index
                .and_then(|index| state.substitutions.get(index))
                .map(StoredValue::duplicate)
            {
                state.stage = StringRawStage::AwaitSubstitutionConversion;
                return convert_string_raw_value(
                    runtime,
                    state,
                    substitution,
                    OperatorPrimitiveHint::String,
                    return_to,
                    execution_budget,
                );
            }
            state.next_index = state.next_index.saturating_add(1);
            read_next_string_raw_literal(runtime, state, return_to, execution_budget)
        }
        StringRawStage::AwaitSubstitutionConversion => {
            let substitution =
                operator_primitive_to_string(completion, state.realm, &state.origin)?;
            append_string_raw_text(&mut state, &substitution, execution_budget)?;
            state.next_index = state.next_index.saturating_add(1);
            read_next_string_raw_literal(runtime, state, return_to, execution_budget)
        }
    }
}

fn read_next_string_raw_literal(
    runtime: &mut Runtime,
    mut state: StringRawContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let key = string_raw_index_key(runtime, state.next_index)?;
    state.stage = StringRawStage::AwaitLiteral;
    read_string_raw_property(
        runtime,
        state,
        key,
        "raw literal",
        return_to,
        execution_budget,
    )
}

fn append_string_raw_text(
    state: &mut StringRawContinuation,
    text: &JsString,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    execution_budget.charge_instructions(u64::from(text.len()).saturating_add(1))?;
    state.result = state.result.concat(text)?;
    Ok(())
}

fn convert_string_raw_value(
    runtime: &mut Runtime,
    state: StringRawContinuation,
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
        OperatorPrimitiveTarget::StringRawValue(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "dynamic index keys and predefined keys share one ownership shape across immediate and suspended reads"
)]
fn read_string_raw_property(
    runtime: &mut Runtime,
    state: StringRawContinuation,
    key: PropertyKey,
    diagnostic_name: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = match state.stage {
        StringRawStage::AwaitRaw => &state.cooked,
        StringRawStage::AwaitLength | StringRawStage::AwaitLiteral => state
            .literals
            .as_ref()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "String.raw property read lost its raw array-like object",
            })?,
        StringRawStage::AwaitLengthConversion
        | StringRawStage::AwaitLiteralConversion
        | StringRawStage::AwaitSubstitutionConversion => {
            return Err(EngineFault::RuntimeInvariant {
                message: "String.raw conversion stage attempted a property read",
            }
            .into());
        }
    };
    charge_string_raw_lookup(runtime, base, execution_budget)?;
    match read_static_property(runtime, state.realm, base, &key)? {
        PropertyReadOutcome::Value(value) => {
            advance_string_raw(runtime, state, Some(value), return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            call_string_raw_function(state, function, receiver, return_to)
        }
        PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            state.realm,
            state.origin,
            Some(&JsString::from_utf8(diagnostic_name)?),
            failure,
        )?)),
    }
}

fn call_string_raw_function(
    state: StringRawContinuation,
    function: FunctionId,
    receiver: StoredValue,
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
    continuations.push(NativeContinuation::StringRaw(Box::new(state)));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::empty(),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

fn string_raw_index_key(runtime: &mut Runtime, index: u64) -> Result<PropertyKey, NativeFailure> {
    if let Ok(index) = u32::try_from(index)
        && let Some(index) = ArrayIndex::new(index)
    {
        return Ok(PropertyKey::from_index(index));
    }
    let name = JsNumber::from_f64(string_raw_index_as_f64(index)).to_javascript_string()?;
    Ok(runtime.property_key_from_string(&name)?)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "LengthOfArrayLike bounds indices below 2^53 - 1, so every index is exactly representable in binary64"
)]
fn string_raw_index_as_f64(index: u64) -> f64 {
    index as f64
}

fn charge_string_raw_lookup(
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

fn take_string_raw_completion(completion: Option<StoredValue>) -> Result<StoredValue, EngineFault> {
    completion.ok_or(EngineFault::RuntimeInvariant {
        message: "String.raw resumed without a completion",
    })
}
