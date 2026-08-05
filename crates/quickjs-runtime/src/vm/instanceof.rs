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
        prototype: None,
        current: None,
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
        prototype: None,
        current: None,
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
            finish_ordinary_has_instance(runtime, state, completion, return_to, execution_budget)
        }
        InstanceOfStage::PrototypeWalk => advance_instance_of_prototype_walk(
            runtime,
            state,
            completion,
            return_to,
            execution_budget,
        ),
    }
}

fn continue_instance_of_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: InstanceOfContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_instance_of(runtime, state, &value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(&mut call, vec![NativeContinuation::InstanceOf(state)])?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(&mut frame, vec![NativeContinuation::InstanceOf(state)])?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "instanceof internal method produced a structured result",
        }
        .into()),
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
    let reference = state
        .target
        .heap_reference()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "instanceof target lost its object",
        })?;
    let dispatch = begin_internal_get(
        runtime,
        reference,
        state.target.duplicate(),
        key,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_instance_of_after(runtime, dispatch, state, return_to, execution_budget)
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
        | StoredValue::BigInt(_)
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
        state.stage = InstanceOfStage::MethodRead;
        let dispatch = begin_internal_get(
            runtime,
            HeapReference::Function(target),
            state.target.duplicate(),
            key,
            state.realm,
            return_to,
            state.origin.clone(),
            execution_budget,
        )?;
        let NativeDispatch::Immediate(value) = dispatch else {
            return continue_instance_of_after(
                runtime,
                dispatch,
                state,
                return_to,
                execution_budget,
            );
        };
        match value {
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
            | StoredValue::BigInt(_)
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
    state.stage = InstanceOfStage::PrototypeRead;
    let reference = state
        .target
        .heap_reference()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "instanceof callable target lost its object",
        })?;
    let dispatch = begin_internal_get(
        runtime,
        reference,
        state.target.duplicate(),
        prototype_key,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_instance_of_after(runtime, dispatch, state, return_to, execution_budget)
}

fn finish_ordinary_has_instance(
    runtime: &mut Runtime,
    mut state: InstanceOfContinuation,
    prototype: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype_reference = match prototype {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
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
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
        }
    };
    state.prototype = Some(prototype_reference);
    state.current = Some(value_reference);
    state.stage = InstanceOfStage::PrototypeWalk;
    execution_budget.charge_instructions(1)?;
    let dispatch = begin_internal_get_prototype_of(
        runtime,
        value_reference,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_instance_of_after(runtime, dispatch, state, return_to, execution_budget)
}

fn advance_instance_of_prototype_walk(
    runtime: &mut Runtime,
    mut state: InstanceOfContinuation,
    completion: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion.duplicate();
    loop {
        let Some(reference) = completion.heap_reference() else {
            if matches!(completion, StoredValue::Null) {
                return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
            }
            return Err(EngineFault::RuntimeInvariant {
                message: "instanceof [[GetPrototypeOf]] returned neither object nor null",
            }
            .into());
        };
        if Some(reference) == state.prototype {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)));
        }
        state.current = Some(reference);
        execution_budget.charge_instructions(1)?;
        let dispatch = begin_internal_get_prototype_of(
            runtime,
            reference,
            state.realm,
            return_to,
            state.origin.clone(),
            execution_budget,
        )?;
        match dispatch {
            NativeDispatch::Immediate(value) => completion = value,
            dispatch => {
                return continue_instance_of_after(
                    runtime,
                    dispatch,
                    state,
                    return_to,
                    execution_budget,
                );
            }
        }
    }
}
