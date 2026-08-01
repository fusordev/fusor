/*
 * JavaScript bytecode execution and closure semantics derived from QuickJS.
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

//! Resumable `instanceof` execution matching the pinned upstream operator.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

fn instanceof_exception(
    realm: RealmId,
    origin: JsStackFrame,
    kind: ExceptionKind,
    message: &str,
) -> Result<NativeFailure, NativeFailure> {
    Ok(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}

/// Starts the `instanceof` operator (`JS_IsInstanceOf`) for one verified
/// `InstanceOf` instruction.
pub(super) fn begin_instance_of(
    runtime: &mut Runtime,
    value: StoredValue,
    target: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if !matches!(target, StoredValue::Function(_) | StoredValue::Object(_)) {
        return Err(instanceof_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "invalid 'instanceof' right operand",
        )?);
    }
    let state = InstanceOfContinuation {
        value,
        target,
        realm,
        stage: InstanceOfStage::MethodRead,
        origin,
    };
    instance_of_method_read(runtime, state, return_to, execution_budget)
}

/// Starts `Function.prototype[@@hasInstance]`, which runs only the ordinary
/// instance-of algorithm without an initial `@@hasInstance` method lookup.
pub(super) fn begin_function_has_instance(
    runtime: &mut Runtime,
    realm: RealmId,
    value: StoredValue,
    target: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = InstanceOfContinuation {
        value,
        target,
        realm,
        stage: InstanceOfStage::MethodRead,
        origin,
    };
    ordinary_has_instance(runtime, state, return_to, execution_budget)
}

/// Resumes a suspended `instanceof` continuation with one getter or
/// `@@hasInstance` call completion.
pub(super) fn advance_instance_of(
    runtime: &mut Runtime,
    state: InstanceOfContinuation,
    completion: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        InstanceOfStage::MethodRead => {
            instance_of_method_decision(runtime, state, completion, return_to, execution_budget)
        }
        InstanceOfStage::MethodCall => Ok(NativeDispatch::Immediate(StoredValue::Boolean(
            completion.is_truthy(),
        ))),
        InstanceOfStage::PrototypeRead => {
            finish_ordinary_has_instance(runtime, state, completion, return_to)
        }
    }
}

fn instance_of_method_read(
    runtime: &mut Runtime,
    mut state: InstanceOfContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = InstanceOfStage::MethodRead;
    let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolHasInstance);
    charge_iterator_property_lookup(runtime, &state.target, execution_budget)?;
    match read_static_property(runtime, state.realm, &state.target, &key)? {
        PropertyReadOutcome::Value(value) => {
            instance_of_method_decision(runtime, state, &value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            state.stage = InstanceOfStage::MethodRead;
            let origin = state.origin.clone();
            iterator_getter_call(
                function,
                receiver,
                NativeContinuation::InstanceOf(state),
                return_to,
                origin,
                None,
            )
        }
        PropertyReadOutcome::Failed(_) => Err(EngineFault::RuntimeInvariant {
            message: "instanceof target property read failed",
        }
        .into()),
    }
}

fn instance_of_method_decision(
    runtime: &mut Runtime,
    mut state: InstanceOfContinuation,
    method: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        StoredValue::Undefined | StoredValue::Null => {
            if !matches!(state.target, StoredValue::Function(_)) {
                return Err(instanceof_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "invalid 'instanceof' right operand",
                )?);
            }
            ordinary_has_instance(runtime, state, return_to, execution_budget)
        }
        StoredValue::Function(function) => {
            let receiver = state.target.duplicate();
            let argument = state.value.duplicate();
            state.stage = InstanceOfStage::MethodCall;
            let origin = state.origin.clone();
            let mut continuations = Vec::new();
            continuations
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 1,
                })?;
            continuations.push(NativeContinuation::InstanceOf(state));
            Ok(NativeDispatch::Call(NativeCall {
                function: *function,
                receiver,
                arguments: CallArguments::from_values(vec![argument]),
                return_to,
                origin,
                continuations,
                pre_call: None,
                new_target: None,
                native_caller: None,
            }))
        }
        StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Object(_) => Err(instanceof_exception(
            state.realm,
            state.origin,
            ExceptionKind::TypeError,
            "not a function",
        )?),
    }
}

/// `JS_OrdinaryIsInstanceOf`: non-callable targets are `false` without an
/// exception, bound targets re-run the full operator on their target
/// (including `@@hasInstance`), and ordinary targets compare their
/// `prototype` property against the value's chain. The bound-target unwrap
/// loop is iterative over the whole chain.
#[allow(
    clippy::too_many_lines,
    reason = "the bound-target unwrap loop, method re-read, and ordinary prototype path stay one audited algorithm"
)]
fn ordinary_has_instance(
    runtime: &mut Runtime,
    mut state: InstanceOfContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        let StoredValue::Function(function) = state.target else {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
        };
        let Some(target) = runtime
            .functions
            .get(function)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "function",
                index: function.index(),
                generation: function.generation(),
            })?
            .bound()
            .map(|bound| bound.target)
        else {
            break;
        };
        state.target = StoredValue::Function(target);
        let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolHasInstance);
        charge_iterator_property_lookup(runtime, &state.target, execution_budget)?;
        match read_static_property(runtime, state.realm, &state.target, &key)? {
            PropertyReadOutcome::Value(value) => match value {
                StoredValue::Undefined | StoredValue::Null => {}
                StoredValue::Function(method) => {
                    let receiver = state.target.duplicate();
                    let argument = state.value.duplicate();
                    state.stage = InstanceOfStage::MethodCall;
                    let origin = state.origin.clone();
                    let mut continuations = Vec::new();
                    continuations.try_reserve_exact(1).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 1,
                        }
                    })?;
                    continuations.push(NativeContinuation::InstanceOf(state));
                    return Ok(NativeDispatch::Call(NativeCall {
                        function: method,
                        receiver,
                        arguments: CallArguments::from_values(vec![argument]),
                        return_to,
                        origin,
                        continuations,
                        pre_call: None,
                        new_target: None,
                        native_caller: None,
                    }));
                }
                StoredValue::Boolean(_)
                | StoredValue::Number(_)
                | StoredValue::String(_)
                | StoredValue::Symbol(_)
                | StoredValue::Object(_) => {
                    return Err(instanceof_exception(
                        state.realm,
                        state.origin,
                        ExceptionKind::TypeError,
                        "not a function",
                    )?);
                }
            },
            PropertyReadOutcome::Getter { function, receiver } => {
                state.stage = InstanceOfStage::MethodRead;
                let origin = state.origin.clone();
                return iterator_getter_call(
                    function,
                    receiver,
                    NativeContinuation::InstanceOf(state),
                    return_to,
                    origin,
                    None,
                );
            }
            PropertyReadOutcome::Failed(_) => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "instanceof target property read failed",
                }
                .into());
            }
        }
    }
    if !matches!(
        state.value,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
    }
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    charge_iterator_property_lookup(runtime, &state.target, execution_budget)?;
    match read_static_property(runtime, state.realm, &state.target, &prototype_key)? {
        PropertyReadOutcome::Value(value) => {
            finish_ordinary_has_instance(runtime, state, &value, return_to)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            state.stage = InstanceOfStage::PrototypeRead;
            let origin = state.origin.clone();
            iterator_getter_call(
                function,
                receiver,
                NativeContinuation::InstanceOf(state),
                return_to,
                origin,
                None,
            )
        }
        PropertyReadOutcome::Failed(_) => Err(EngineFault::RuntimeInvariant {
            message: "instanceof prototype property read failed",
        }
        .into()),
    }
}

fn finish_ordinary_has_instance(
    runtime: &Runtime,
    state: InstanceOfContinuation,
    prototype: &StoredValue,
    _return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype_reference = match prototype {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Err(instanceof_exception(
                state.realm,
                state.origin,
                ExceptionKind::TypeError,
                "operand 'prototype' property is not an object",
            )?);
        }
    };
    let value_reference = match state.value {
        StoredValue::Function(function) => HeapReference::Function(function),
        StoredValue::Object(object) => HeapReference::Object(object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
        }
    };
    let mut current = Some(value_reference);
    let mut remaining = runtime
        .functions
        .len()
        .saturating_add(runtime.objects.len())
        .saturating_add(1);
    while let Some(reference) = current {
        if reference == prototype_reference {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)));
        }
        if remaining == 0 {
            return Err(NativeFailure::Execution(
                EngineFault::RuntimeInvariant {
                    message: "instanceof prototype chain exceeds the heap size",
                }
                .into(),
            ));
        }
        remaining -= 1;
        current = runtime
            .object_record(reference)
            .map_err(NativeFailure::from)?
            .prototype();
    }
    Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
}
