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

//! Primitive conversion continuations, constructor intrinsics, and operators.

use std::cmp::Ordering;

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) fn symbol_descriptive_string(symbol: &crate::Atom) -> Result<JsString, NativeFailure> {
    let description = symbol
        .description()
        .cloned()
        .unwrap_or_else(JsString::empty);
    Ok(JsString::from_utf8("Symbol(")?
        .concat(&description)?
        .concat(&JsString::from_utf8(")")?)?)
}

pub(super) fn boolean_receiver_value(
    runtime: &Runtime,
    realm: RealmId,
    receiver: &StoredValue,
    origin: Option<&JsStackFrame>,
) -> Result<bool, NativeFailure> {
    let value = match receiver {
        StoredValue::Boolean(value) => Some(*value),
        StoredValue::Object(object) => runtime.boxed_boolean(*object)?,
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Function(_) => None,
    };
    if let Some(value) = value {
        return Ok(value);
    }
    let origin = origin.cloned().unwrap_or_else(native_function_host_origin);
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a boolean")?,
        },
        origin,
    }))
}

pub(super) fn number_receiver_value(
    runtime: &Runtime,
    realm: RealmId,
    receiver: &StoredValue,
    origin: Option<&JsStackFrame>,
) -> Result<JsNumber, NativeFailure> {
    let value = match receiver {
        StoredValue::Number(value) => Some(*value),
        StoredValue::Object(object) => runtime.boxed_number(*object)?,
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::String(_)
        | StoredValue::BigInt(_)
        | StoredValue::Symbol(_)
        | StoredValue::Function(_) => None,
    };
    if let Some(value) = value {
        return Ok(value);
    }
    let origin = origin.cloned().unwrap_or_else(native_function_host_origin);
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a number")?,
        },
        origin,
    }))
}

pub(super) fn string_receiver_value(
    runtime: &Runtime,
    realm: RealmId,
    receiver: &StoredValue,
    origin: Option<&JsStackFrame>,
) -> Result<JsString, NativeFailure> {
    let value = match receiver {
        StoredValue::String(value) => Some(value.clone()),
        StoredValue::Object(object) => runtime.boxed_string(*object)?.cloned(),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::Symbol(_)
        | StoredValue::Function(_) => None,
    };
    if let Some(value) = value {
        return Ok(value);
    }
    let origin = origin.cloned().unwrap_or_else(native_function_host_origin);
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a string")?,
        },
        origin,
    }))
}

pub(super) fn symbol_receiver_value(
    runtime: &Runtime,
    realm: RealmId,
    receiver: &StoredValue,
    origin: Option<&JsStackFrame>,
) -> Result<crate::Atom, NativeFailure> {
    let value = match receiver {
        StoredValue::Symbol(value) => Some(value.clone()),
        StoredValue::Object(object) => runtime.boxed_symbol(*object)?.cloned(),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Function(_) => None,
    };
    if let Some(value) = value {
        return Ok(value);
    }
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a symbol")?,
        },
        origin: origin.cloned().unwrap_or_else(native_function_host_origin),
    }))
}

pub(super) fn begin_boolean_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    value: bool,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    begin_intrinsic_get(
        runtime,
        HeapReference::Function(new_target),
        StoredValue::Function(new_target),
        &prototype_key,
        IntrinsicGetContinuation::BooleanConstructor { new_target, value },
        return_to,
        origin,
    )
}

pub(super) fn begin_number_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    value: JsNumber,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    begin_intrinsic_get(
        runtime,
        HeapReference::Function(new_target),
        StoredValue::Function(new_target),
        &prototype_key,
        IntrinsicGetContinuation::NumberConstructor { new_target, value },
        return_to,
        origin,
    )
}

pub(super) fn begin_string_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    value: JsString,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    begin_intrinsic_get(
        runtime,
        HeapReference::Function(new_target),
        StoredValue::Function(new_target),
        &prototype_key,
        IntrinsicGetContinuation::StringConstructor { new_target, value },
        return_to,
        origin,
    )
}

fn precharge_array_constructor_work(
    arguments: &[StoredValue],
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    if matches!(arguments, [StoredValue::Number(_)]) {
        return Ok(());
    }
    execution_budget.charge_instructions(usize_to_u64(arguments.len()))?;
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "Array construction retains its realm, newTarget, arguments, source origin, and caller continuation across an observable prototype Get"
)]
pub(super) fn begin_array_constructor_prototype_get(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: FunctionId,
    arguments: Vec<StoredValue>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let receiver = StoredValue::Function(new_target);
    charge_heap_property_lookup(runtime, &receiver, execution_budget)?;
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    match read_heap_property_for_receiver(
        runtime,
        HeapReference::Function(new_target),
        receiver,
        &prototype_key,
    )? {
        PropertyReadOutcome::Value(value) => finish_array_constructor_after_prototype_get(
            runtime,
            realm,
            new_target,
            arguments,
            origin,
            &value,
            execution_budget,
        ),
        PropertyReadOutcome::Getter { function, receiver } => intrinsic_getter_call(
            function,
            receiver,
            IntrinsicGetContinuation::ArrayConstructor {
                realm,
                new_target,
                arguments,
                origin: origin.clone(),
            },
            return_to,
            Some(origin),
        ),
        PropertyReadOutcome::Failed(_) => Err(EngineFault::RuntimeInvariant {
            message: "function-valued Array newTarget prototype Get failed as a primitive",
        }
        .into()),
    }
}

pub(super) fn finish_array_constructor_after_prototype_get(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: FunctionId,
    arguments: Vec<StoredValue>,
    origin: JsStackFrame,
    requested: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let target_realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_array_prototype(target_realm)?)
        }
    };
    finish_array_constructor(
        runtime,
        realm,
        prototype,
        arguments,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_array_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    prototype: HeapReference,
    arguments: Vec<StoredValue>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    precharge_array_constructor_work(&arguments, execution_budget)?;
    let object = match arguments.as_slice() {
        [StoredValue::Number(value)] => {
            let Some(length) = array_length_from_number(*value) else {
                return Err(NativeFailure::Abrupt(PendingException {
                    realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::RangeError,
                        message: JsString::from_utf8("invalid array length")?,
                    },
                    origin,
                }));
            };
            runtime.allocate_sparse_array_with_prototype(prototype, length)?
        }
        _ => runtime.allocate_array_with_prototype(prototype, arguments)?,
    };
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the generic resumable Get boundary preserves its receiver, continuation target, caller continuation, and source origin"
)]
pub(super) fn begin_intrinsic_get(
    runtime: &mut Runtime,
    reference: HeapReference,
    receiver: StoredValue,
    key: &PropertyKey,
    continuation: IntrinsicGetContinuation,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    match read_heap_property_for_receiver(runtime, reference, receiver, key)? {
        PropertyReadOutcome::Value(value) => {
            finish_intrinsic_get(runtime, continuation, value, &[], &[])
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            intrinsic_getter_call(function, receiver, continuation, return_to, origin)
        }
        PropertyReadOutcome::Failed(_) => Err(EngineFault::RuntimeInvariant {
            message: "heap-only intrinsic Get produced a primitive property failure",
        }
        .into()),
    }
}

fn intrinsic_getter_call(
    function: FunctionId,
    receiver: StoredValue,
    continuation: IntrinsicGetContinuation,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let continuations = reserve_intrinsic_get_continuation()?;
    Ok(intrinsic_getter_call_with_reserved_continuation(
        function,
        receiver,
        continuation,
        return_to,
        origin,
        continuations,
    ))
}

pub(super) fn reserve_intrinsic_get_continuation() -> Result<Vec<NativeContinuation>, NativeFailure>
{
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    Ok(continuations)
}

pub(super) fn intrinsic_getter_call_with_reserved_continuation(
    function: FunctionId,
    receiver: StoredValue,
    continuation: IntrinsicGetContinuation,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
    mut continuations: Vec<NativeContinuation>,
) -> NativeDispatch {
    debug_assert!(continuations.capacity() >= 1);
    continuations.push(NativeContinuation::IntrinsicGet(continuation));
    NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::empty(),
        return_to,
        origin: origin.unwrap_or_else(native_function_host_origin),
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    })
}

pub(super) fn finish_intrinsic_get(
    runtime: &mut Runtime,
    continuation: IntrinsicGetContinuation,
    value: StoredValue,
    active_root_frames: &[Frame],
    outer_continuations: &[NativeContinuation],
) -> Result<NativeDispatch, NativeFailure> {
    match continuation {
        IntrinsicGetContinuation::BooleanConstructor {
            new_target,
            value: boolean_value,
        } => finish_boolean_constructor_wrapper(runtime, new_target, boolean_value, &value),
        IntrinsicGetContinuation::NumberConstructor {
            new_target,
            value: number_value,
        } => finish_number_constructor_wrapper(runtime, new_target, number_value, &value),
        IntrinsicGetContinuation::StringConstructor {
            new_target,
            value: string_value,
        } => finish_string_constructor_wrapper(runtime, new_target, string_value, &value),
        IntrinsicGetContinuation::ArrayConstructor { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Array prototype getter resumed without an execution budget",
        }
        .into()),
        IntrinsicGetContinuation::ObjectPrototypeToString {
            default_tag,
            temporary_receiver,
        } => {
            if temporary_receiver.is_none() {
                return finish_object_prototype_to_string(default_tag, value);
            }

            // Release the intrinsic's temporary receiver before allocating the
            // result string, exactly as QuickJS releases its local boxed value
            // immediately after Get. The getter completion remains a root
            // until it has been consumed, so `return this` and heap graphs
            // reachable only through the completion cannot be reclaimed early.
            let completion_holds_heap =
                matches!(value, StoredValue::Function(_) | StoredValue::Object(_));
            let cleanup = collect_cycles_with_execution_roots(
                runtime,
                active_root_frames,
                outer_continuations,
                std::slice::from_ref(&value),
            );
            let result = finish_object_prototype_to_string(default_tag, value);

            // Collection scratch allocation is host bookkeeping, not a
            // JavaScript operation. It must never replace a completed getter,
            // a successful result, or the formatting failure that already won.
            // Retry after consuming a heap-valued completion so a receiver
            // kept alive only by `tag` is released at the same boundary.
            if cleanup.is_err() || completion_holds_heap {
                let _ = collect_cycles_with_execution_roots(
                    runtime,
                    active_root_frames,
                    outer_continuations,
                    &[],
                );
            }
            result
        }
    }
}

fn finish_boolean_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    boolean_value: bool,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_boolean_prototype(realm)?)
        }
    };
    let object = runtime
        .allocate_boxed_boolean_with_prototype(prototype, boolean_value)
        .map_err(NativeFailure::Execution)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn finish_number_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    number_value: JsNumber,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_number_prototype(realm)?)
        }
    };
    let object = runtime
        .allocate_boxed_number_with_prototype(prototype, number_value)
        .map_err(NativeFailure::Execution)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn finish_string_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    string_value: JsString,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_string_prototype(realm)?)
        }
    };
    let object = runtime
        .allocate_boxed_string_with_prototype(prototype, string_value)
        .map_err(NativeFailure::Execution)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the suspended source-conversion state preserves the original native call boundary"
)]
pub(super) fn begin_function_source_conversion(
    runtime: &mut Runtime,
    native: NativeFunction,
    arguments: Vec<StoredValue>,
    construction: Option<FunctionId>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    active_frames: usize,
    active_frame_values: u64,
    compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    advance_function_source_conversion(
        runtime,
        FunctionSourceContinuation {
            native,
            arguments,
            index: 0,
            stage: PrimitiveConversionStage::Start,
            construction,
            origin,
        },
        None,
        return_to,
        active_frames,
        active_frame_values,
        compiler,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the explicit ToPrimitive(String) state machine keeps every observable lookup and call in one audited order"
)]
pub(super) fn advance_function_source_conversion(
    runtime: &mut Runtime,
    mut state: FunctionSourceContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    active_frames: usize,
    active_frame_values: u64,
    compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        match state.stage {
            PrimitiveConversionStage::AwaitExoticProperty
            | PrimitiveConversionStage::AwaitToStringProperty
            | PrimitiveConversionStage::AwaitValueOfProperty => {
                let property = match state.stage {
                    PrimitiveConversionStage::AwaitExoticProperty => {
                        PrimitiveConversionProperty::Exotic
                    }
                    PrimitiveConversionStage::AwaitToStringProperty => {
                        PrimitiveConversionProperty::ToString
                    }
                    PrimitiveConversionStage::AwaitValueOfProperty => {
                        PrimitiveConversionProperty::ValueOf
                    }
                    PrimitiveConversionStage::Start
                    | PrimitiveConversionStage::ToString
                    | PrimitiveConversionStage::ValueOf
                    | PrimitiveConversionStage::AwaitExotic
                    | PrimitiveConversionStage::AwaitToString
                    | PrimitiveConversionStage::AwaitValueOf => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "dynamic Function property stage changed while resuming",
                        }
                        .into());
                    }
                };
                match use_primitive_conversion_property(
                    &mut state.stage,
                    property,
                    &value,
                    state.native.realm,
                    &state.origin,
                )? {
                    PrimitiveConversionPropertyAction::Continue => {}
                    PrimitiveConversionPropertyAction::Call {
                        function,
                        arguments,
                    } => {
                        return function_source_method_call(state, function, arguments, return_to);
                    }
                }
            }
            PrimitiveConversionStage::AwaitToString
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                state.stage = PrimitiveConversionStage::ValueOf;
            }
            PrimitiveConversionStage::AwaitExotic | PrimitiveConversionStage::AwaitValueOf
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                return Err(primitive_conversion_type_error(
                    state.native.realm,
                    &state.origin,
                    "toPrimitive",
                )?);
            }
            PrimitiveConversionStage::AwaitExotic
            | PrimitiveConversionStage::AwaitToString
            | PrimitiveConversionStage::AwaitValueOf => {
                let converted =
                    dynamic_source_primitive_to_string(value, state.native.realm, &state.origin)?;
                let argument =
                    state
                        .arguments
                        .get_mut(state.index)
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "dynamic Function source conversion lost its current argument",
                        })?;
                *argument = StoredValue::String(converted);
                state.index = state.index.saturating_add(1);
                state.stage = PrimitiveConversionStage::Start;
            }
            PrimitiveConversionStage::Start
            | PrimitiveConversionStage::ToString
            | PrimitiveConversionStage::ValueOf => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "dynamic Function source conversion resumed outside a call stage",
                }
                .into());
            }
        }
    }

    loop {
        if state.index == state.arguments.len() {
            let source = completed_dynamic_function_source(state.arguments)?;
            return finish_ordinary_function_constructor(
                runtime,
                state.native,
                state.construction,
                source,
                return_to,
                state.origin,
                active_frames,
                active_frame_values,
                compiler,
                execution_budget,
            );
        }

        let current = state
            .arguments
            .get(state.index)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "dynamic Function source conversion index escaped its arguments",
            })?;
        if !matches!(current, StoredValue::Function(_) | StoredValue::Object(_)) {
            let converted = dynamic_source_primitive_to_string(
                current.duplicate(),
                state.native.realm,
                &state.origin,
            )?;
            let current =
                state
                    .arguments
                    .get_mut(state.index)
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "dynamic Function primitive conversion lost its argument",
                    })?;
            *current = StoredValue::String(converted);
            state.index = state.index.saturating_add(1);
            state.stage = PrimitiveConversionStage::Start;
            continue;
        }

        let reference = current
            .heap_reference()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "object-valued dynamic Function source has no heap reference",
            })?;
        let (property, key, awaiting_property) = match state.stage {
            PrimitiveConversionStage::Start => (
                PrimitiveConversionProperty::Exotic,
                runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive),
                PrimitiveConversionStage::AwaitExoticProperty,
            ),
            PrimitiveConversionStage::ToString => (
                PrimitiveConversionProperty::ToString,
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PrimitiveConversionStage::AwaitToStringProperty,
            ),
            PrimitiveConversionStage::ValueOf => (
                PrimitiveConversionProperty::ValueOf,
                runtime.predefined_property_key(PredefinedAtom::ValueOf),
                PrimitiveConversionStage::AwaitValueOfProperty,
            ),
            PrimitiveConversionStage::AwaitExoticProperty
            | PrimitiveConversionStage::AwaitToStringProperty
            | PrimitiveConversionStage::AwaitValueOfProperty
            | PrimitiveConversionStage::AwaitExotic
            | PrimitiveConversionStage::AwaitToString
            | PrimitiveConversionStage::AwaitValueOf => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "dynamic Function source conversion awaited without a completion",
                }
                .into());
            }
        };
        match lookup_primitive_conversion_property(runtime, reference, &key)? {
            PrimitiveConversionPropertyLookup::Getter(function) => {
                state.stage = awaiting_property;
                return function_source_method_call(state, function, Vec::new(), return_to);
            }
            PrimitiveConversionPropertyLookup::Value(value) => {
                match use_primitive_conversion_property(
                    &mut state.stage,
                    property,
                    &value,
                    state.native.realm,
                    &state.origin,
                )? {
                    PrimitiveConversionPropertyAction::Continue => {}
                    PrimitiveConversionPropertyAction::Call {
                        function,
                        arguments,
                    } => {
                        return function_source_method_call(state, function, arguments, return_to);
                    }
                }
            }
        }
    }
}

fn lookup_primitive_conversion_property(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<PrimitiveConversionPropertyLookup, NativeFailure> {
    Ok(match lookup_heap_property(runtime, Some(reference), key)? {
        None => PrimitiveConversionPropertyLookup::Value(StoredValue::Undefined),
        Some(OwnProperty::Data { value, .. }) => PrimitiveConversionPropertyLookup::Value(value),
        Some(OwnProperty::Accessor {
            getter: Some(function),
            ..
        }) => PrimitiveConversionPropertyLookup::Getter(function),
        Some(OwnProperty::Accessor { getter: None, .. }) => {
            PrimitiveConversionPropertyLookup::Value(StoredValue::Undefined)
        }
    })
}

fn use_primitive_conversion_property(
    stage: &mut PrimitiveConversionStage,
    property: PrimitiveConversionProperty,
    value: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<PrimitiveConversionPropertyAction, NativeFailure> {
    match property {
        PrimitiveConversionProperty::Exotic => match value {
            StoredValue::Undefined | StoredValue::Null => {
                *stage = PrimitiveConversionStage::ToString;
                Ok(PrimitiveConversionPropertyAction::Continue)
            }
            StoredValue::Function(function) => {
                *stage = PrimitiveConversionStage::AwaitExotic;
                let mut arguments = Vec::new();
                arguments
                    .try_reserve_exact(1)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: 1,
                    })?;
                arguments.push(StoredValue::String(JsString::from_utf8("string")?));
                Ok(PrimitiveConversionPropertyAction::Call {
                    function: *function,
                    arguments,
                })
            }
            StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => Err(primitive_conversion_type_error(
                realm,
                origin,
                "not a function",
            )?),
        },
        PrimitiveConversionProperty::ToString => match value {
            StoredValue::Function(function) => {
                *stage = PrimitiveConversionStage::AwaitToString;
                Ok(PrimitiveConversionPropertyAction::Call {
                    function: *function,
                    arguments: Vec::new(),
                })
            }
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => {
                *stage = PrimitiveConversionStage::ValueOf;
                Ok(PrimitiveConversionPropertyAction::Continue)
            }
        },
        PrimitiveConversionProperty::ValueOf => match value {
            StoredValue::Function(function) => {
                *stage = PrimitiveConversionStage::AwaitValueOf;
                Ok(PrimitiveConversionPropertyAction::Call {
                    function: *function,
                    arguments: Vec::new(),
                })
            }
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => Err(primitive_conversion_type_error(
                realm,
                origin,
                "toPrimitive",
            )?),
        },
    }
}

fn function_source_method_call(
    state: FunctionSourceContinuation,
    function: FunctionId,
    arguments: Vec<StoredValue>,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let receiver = state
        .arguments
        .get(state.index)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "dynamic Function source method lost its receiver",
        })?
        .duplicate();
    let origin = state.origin.clone();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::FunctionSource(state));
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

pub(super) fn begin_property_key_conversion(
    runtime: &mut Runtime,
    value: StoredValue,
    target: PropertyKeyTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
        return advance_property_key_conversion(
            runtime,
            PropertyKeyContinuation {
                receiver: value,
                realm,
                stage: PrimitiveConversionStage::Start,
                target,
                origin,
            },
            None,
            return_to,
            execution_budget,
        );
    }
    finish_property_key_target(runtime, value, target, return_to, &origin, execution_budget)
}

#[allow(
    clippy::too_many_lines,
    reason = "the explicit ToPropertyKey state machine preserves every observable lookup, getter, and call boundary"
)]
pub(super) fn advance_property_key_conversion(
    runtime: &mut Runtime,
    mut state: PropertyKeyContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        match state.stage {
            PrimitiveConversionStage::AwaitExoticProperty
            | PrimitiveConversionStage::AwaitToStringProperty
            | PrimitiveConversionStage::AwaitValueOfProperty => {
                let property = match state.stage {
                    PrimitiveConversionStage::AwaitExoticProperty => {
                        PrimitiveConversionProperty::Exotic
                    }
                    PrimitiveConversionStage::AwaitToStringProperty => {
                        PrimitiveConversionProperty::ToString
                    }
                    PrimitiveConversionStage::AwaitValueOfProperty => {
                        PrimitiveConversionProperty::ValueOf
                    }
                    PrimitiveConversionStage::Start
                    | PrimitiveConversionStage::ToString
                    | PrimitiveConversionStage::ValueOf
                    | PrimitiveConversionStage::AwaitExotic
                    | PrimitiveConversionStage::AwaitToString
                    | PrimitiveConversionStage::AwaitValueOf => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "property-key conversion property stage changed while resuming",
                        }
                        .into());
                    }
                };
                match use_primitive_conversion_property(
                    &mut state.stage,
                    property,
                    &value,
                    state.realm,
                    &state.origin,
                )? {
                    PrimitiveConversionPropertyAction::Continue => {}
                    PrimitiveConversionPropertyAction::Call {
                        function,
                        arguments,
                    } => {
                        return property_key_method_call(state, function, arguments, return_to);
                    }
                }
            }
            PrimitiveConversionStage::AwaitToString
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                state.stage = PrimitiveConversionStage::ValueOf;
            }
            PrimitiveConversionStage::AwaitExotic | PrimitiveConversionStage::AwaitValueOf
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                return Err(primitive_conversion_type_error(
                    state.realm,
                    &state.origin,
                    "toPrimitive",
                )?);
            }
            PrimitiveConversionStage::AwaitExotic
            | PrimitiveConversionStage::AwaitToString
            | PrimitiveConversionStage::AwaitValueOf => {
                return finish_property_key_target(
                    runtime,
                    value,
                    state.target,
                    return_to,
                    &state.origin,
                    execution_budget,
                );
            }
            PrimitiveConversionStage::Start
            | PrimitiveConversionStage::ToString
            | PrimitiveConversionStage::ValueOf => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "property-key conversion resumed outside a call stage",
                }
                .into());
            }
        }
    }

    loop {
        let reference = state
            .receiver
            .heap_reference()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "object-valued property key has no heap reference",
            })?;
        let (property, key, awaiting_property) = match state.stage {
            PrimitiveConversionStage::Start => (
                PrimitiveConversionProperty::Exotic,
                runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive),
                PrimitiveConversionStage::AwaitExoticProperty,
            ),
            PrimitiveConversionStage::ToString => (
                PrimitiveConversionProperty::ToString,
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PrimitiveConversionStage::AwaitToStringProperty,
            ),
            PrimitiveConversionStage::ValueOf => (
                PrimitiveConversionProperty::ValueOf,
                runtime.predefined_property_key(PredefinedAtom::ValueOf),
                PrimitiveConversionStage::AwaitValueOfProperty,
            ),
            PrimitiveConversionStage::AwaitExoticProperty
            | PrimitiveConversionStage::AwaitToStringProperty
            | PrimitiveConversionStage::AwaitValueOfProperty
            | PrimitiveConversionStage::AwaitExotic
            | PrimitiveConversionStage::AwaitToString
            | PrimitiveConversionStage::AwaitValueOf => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "property-key conversion awaited without a completion",
                }
                .into());
            }
        };
        match lookup_primitive_conversion_property(runtime, reference, &key)? {
            PrimitiveConversionPropertyLookup::Getter(function) => {
                state.stage = awaiting_property;
                return property_key_method_call(state, function, Vec::new(), return_to);
            }
            PrimitiveConversionPropertyLookup::Value(value) => {
                match use_primitive_conversion_property(
                    &mut state.stage,
                    property,
                    &value,
                    state.realm,
                    &state.origin,
                )? {
                    PrimitiveConversionPropertyAction::Continue => {}
                    PrimitiveConversionPropertyAction::Call {
                        function,
                        arguments,
                    } => {
                        return property_key_method_call(state, function, arguments, return_to);
                    }
                }
            }
        }
    }
}

fn property_key_method_call(
    state: PropertyKeyContinuation,
    function: FunctionId,
    arguments: Vec<StoredValue>,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let receiver = state.receiver.duplicate();
    let origin = state.origin.clone();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::PropertyKey(state));
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

#[allow(
    clippy::too_many_arguments,
    reason = "the resumable conversion owns distinct verified call, realm, source, and fuel capabilities"
)]
pub(super) fn begin_operator_primitive_conversion(
    runtime: &mut Runtime,
    value: StoredValue,
    hint: OperatorPrimitiveHint,
    target: OperatorPrimitiveTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
        return advance_operator_primitive_conversion(
            runtime,
            OperatorPrimitiveContinuation {
                receiver: value,
                realm,
                hint,
                stage: OperatorPrimitiveStage::Start,
                target,
                origin,
            },
            None,
            return_to,
            execution_budget,
        );
    }
    finish_operator_primitive_target(
        runtime,
        value,
        target,
        realm,
        return_to,
        &origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "the explicit ToPrimitive state machine preserves every observable lookup, getter, and call boundary"
)]
pub(super) fn advance_operator_primitive_conversion(
    runtime: &mut Runtime,
    mut state: OperatorPrimitiveContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        match state.stage {
            OperatorPrimitiveStage::AwaitExoticProperty
            | OperatorPrimitiveStage::AwaitValueOfProperty
            | OperatorPrimitiveStage::AwaitToStringProperty => {
                let property = match state.stage {
                    OperatorPrimitiveStage::AwaitExoticProperty => {
                        PrimitiveConversionProperty::Exotic
                    }
                    OperatorPrimitiveStage::AwaitValueOfProperty => {
                        PrimitiveConversionProperty::ValueOf
                    }
                    OperatorPrimitiveStage::AwaitToStringProperty => {
                        PrimitiveConversionProperty::ToString
                    }
                    OperatorPrimitiveStage::Start
                    | OperatorPrimitiveStage::ValueOf
                    | OperatorPrimitiveStage::ToString
                    | OperatorPrimitiveStage::AwaitExotic
                    | OperatorPrimitiveStage::AwaitValueOf
                    | OperatorPrimitiveStage::AwaitToString => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "operator primitive property stage changed while resuming",
                        }
                        .into());
                    }
                };
                if let Some((function, arguments)) =
                    use_operator_primitive_property(&mut state, property, &value)?
                {
                    return operator_primitive_method_call(state, function, arguments, return_to);
                }
            }
            OperatorPrimitiveStage::AwaitValueOf
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                if matches!(state.hint, OperatorPrimitiveHint::String) {
                    return Err(primitive_conversion_type_error(
                        state.realm,
                        &state.origin,
                        "toPrimitive",
                    )?);
                }
                state.stage = OperatorPrimitiveStage::ToString;
            }
            OperatorPrimitiveStage::AwaitToString
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                if matches!(state.hint, OperatorPrimitiveHint::String) {
                    state.stage = OperatorPrimitiveStage::ValueOf;
                } else {
                    return Err(primitive_conversion_type_error(
                        state.realm,
                        &state.origin,
                        "toPrimitive",
                    )?);
                }
            }
            OperatorPrimitiveStage::AwaitExotic
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) =>
            {
                return Err(primitive_conversion_type_error(
                    state.realm,
                    &state.origin,
                    "toPrimitive",
                )?);
            }
            OperatorPrimitiveStage::AwaitExotic
            | OperatorPrimitiveStage::AwaitValueOf
            | OperatorPrimitiveStage::AwaitToString => {
                return finish_operator_primitive_target(
                    runtime,
                    value,
                    state.target,
                    state.realm,
                    return_to,
                    &state.origin,
                    execution_budget,
                );
            }
            OperatorPrimitiveStage::Start
            | OperatorPrimitiveStage::ValueOf
            | OperatorPrimitiveStage::ToString => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "operator primitive conversion resumed outside a call stage",
                }
                .into());
            }
        }
    }

    loop {
        let reference = state
            .receiver
            .heap_reference()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "object-valued operator operand has no heap reference",
            })?;
        let (property, key, awaiting_property) = match state.stage {
            OperatorPrimitiveStage::Start => (
                PrimitiveConversionProperty::Exotic,
                runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive),
                OperatorPrimitiveStage::AwaitExoticProperty,
            ),
            OperatorPrimitiveStage::ValueOf => (
                PrimitiveConversionProperty::ValueOf,
                runtime.predefined_property_key(PredefinedAtom::ValueOf),
                OperatorPrimitiveStage::AwaitValueOfProperty,
            ),
            OperatorPrimitiveStage::ToString => (
                PrimitiveConversionProperty::ToString,
                runtime.predefined_property_key(PredefinedAtom::ToString),
                OperatorPrimitiveStage::AwaitToStringProperty,
            ),
            OperatorPrimitiveStage::AwaitExoticProperty
            | OperatorPrimitiveStage::AwaitValueOfProperty
            | OperatorPrimitiveStage::AwaitToStringProperty
            | OperatorPrimitiveStage::AwaitExotic
            | OperatorPrimitiveStage::AwaitValueOf
            | OperatorPrimitiveStage::AwaitToString => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "operator primitive conversion awaited without a completion",
                }
                .into());
            }
        };
        match lookup_primitive_conversion_property(runtime, reference, &key)? {
            PrimitiveConversionPropertyLookup::Getter(function) => {
                state.stage = awaiting_property;
                return operator_primitive_method_call(state, function, Vec::new(), return_to);
            }
            PrimitiveConversionPropertyLookup::Value(value) => {
                if let Some((function, arguments)) =
                    use_operator_primitive_property(&mut state, property, &value)?
                {
                    return operator_primitive_method_call(state, function, arguments, return_to);
                }
            }
        }
    }
}

fn use_operator_primitive_property(
    state: &mut OperatorPrimitiveContinuation,
    property: PrimitiveConversionProperty,
    value: &StoredValue,
) -> Result<Option<(FunctionId, Vec<StoredValue>)>, NativeFailure> {
    match property {
        PrimitiveConversionProperty::Exotic => match value {
            StoredValue::Undefined | StoredValue::Null => {
                state.stage = state.hint.first_ordinary_stage();
                Ok(None)
            }
            StoredValue::Function(function) => {
                state.stage = OperatorPrimitiveStage::AwaitExotic;
                let mut arguments = Vec::new();
                arguments
                    .try_reserve_exact(1)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: 1,
                    })?;
                arguments.push(StoredValue::String(JsString::from_utf8(state.hint.name())?));
                Ok(Some((*function, arguments)))
            }
            StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => Err(primitive_conversion_type_error(
                state.realm,
                &state.origin,
                "not a function",
            )?),
        },
        PrimitiveConversionProperty::ValueOf => match value {
            StoredValue::Function(function) => {
                state.stage = OperatorPrimitiveStage::AwaitValueOf;
                Ok(Some((*function, Vec::new())))
            }
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => {
                if matches!(state.hint, OperatorPrimitiveHint::String) {
                    return Err(primitive_conversion_type_error(
                        state.realm,
                        &state.origin,
                        "toPrimitive",
                    )?);
                }
                state.stage = OperatorPrimitiveStage::ToString;
                Ok(None)
            }
        },
        PrimitiveConversionProperty::ToString => match value {
            StoredValue::Function(function) => {
                state.stage = OperatorPrimitiveStage::AwaitToString;
                Ok(Some((*function, Vec::new())))
            }
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => {
                if matches!(state.hint, OperatorPrimitiveHint::String) {
                    state.stage = OperatorPrimitiveStage::ValueOf;
                    Ok(None)
                } else {
                    Err(primitive_conversion_type_error(
                        state.realm,
                        &state.origin,
                        "toPrimitive",
                    )?)
                }
            }
        },
    }
}

fn operator_primitive_method_call(
    state: OperatorPrimitiveContinuation,
    function: FunctionId,
    arguments: Vec<StoredValue>,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let receiver = state.receiver.duplicate();
    let origin = state.origin.clone();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::OperatorPrimitive(state));
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

#[allow(
    clippy::too_many_lines,
    reason = "primitive conversion targets remain centralized at the audited conversion boundary"
)]
fn finish_operator_primitive_target(
    runtime: &mut Runtime,
    value: StoredValue,
    target: OperatorPrimitiveTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match target {
        OperatorPrimitiveTarget::Unary { opcode } => {
            apply_unary_operator(opcode, value, realm, origin)
        }
        OperatorPrimitiveTarget::BinaryRight {
            opcode,
            right,
            hint,
        } => {
            // The eager left conversion pins the operand order for the Number
            // domain, but a `BigInt` must survive it so the operator can decide
            // the domain from both operands. Converting it here would report
            // `cannot convert bigint to number` even for `1n - 2n`.
            let left = if binary_operator_converts_left_to_number_first(opcode)
                && !matches!(value, StoredValue::BigInt(_))
            {
                StoredValue::Number(operator_to_number(value, realm, origin)?)
            } else {
                value
            };
            begin_operator_primitive_conversion(
                runtime,
                right,
                hint,
                OperatorPrimitiveTarget::BinaryFinish { opcode, left },
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        OperatorPrimitiveTarget::BinaryFinish { opcode, left } => {
            apply_binary_operator(opcode, left, value, realm, origin)
        }
        OperatorPrimitiveTarget::EqualityFinish { opcode, other } => begin_abstract_equality(
            runtime,
            value,
            other,
            opcode,
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        OperatorPrimitiveTarget::NumberIntrinsic { new_target } => {
            // `Number()` is the one coercion that crosses the numeric domains.
            // It applies `ToNumeric` and then converts a `BigInt` result to the
            // nearest binary64 (`js_number_constructor`, `quickjs.c:44595`),
            // which is why `Number(1n)` is `1` while `1n | 0` still throws.
            let value = operator_to_numeric(value, realm, origin)?;
            new_target.map_or_else(
                || Ok(NativeDispatch::Immediate(StoredValue::Number(value))),
                |new_target| {
                    begin_number_constructor_wrapper(
                        runtime,
                        new_target,
                        value,
                        return_to,
                        Some(origin.clone()),
                    )
                },
            )
        }
        OperatorPrimitiveTarget::NumberToString { number } => {
            let radix = operator_to_number(value, realm, origin)?;
            finish_number_to_string_radix(number, radix, realm, origin)
        }
        OperatorPrimitiveTarget::NumberFormatDigits { number, format } => {
            let digits = operator_to_number(value, realm, origin)?;
            finish_number_format(number, format, digits, realm, origin)
        }
        OperatorPrimitiveTarget::StringIntrinsic { new_target } => {
            let value = operator_primitive_to_string(value, realm, origin)?;
            if let Some(new_target) = new_target {
                begin_string_constructor_wrapper(
                    runtime,
                    new_target,
                    value,
                    return_to,
                    Some(origin.clone()),
                )
            } else {
                Ok(NativeDispatch::Immediate(StoredValue::String(value)))
            }
        }
        OperatorPrimitiveTarget::SymbolIntrinsic { global_registry } => {
            let description = operator_primitive_to_string(value, realm, origin)?;
            let symbol = if global_registry {
                runtime.intern_global_symbol(&description)?
            } else {
                runtime.new_unique_symbol(Some(&description))?
            };
            Ok(NativeDispatch::Immediate(StoredValue::Symbol(symbol)))
        }
        OperatorPrimitiveTarget::StringIteratorIntrinsic => {
            let string = operator_primitive_to_string(value, realm, origin)?;
            Ok(NativeDispatch::Immediate(StoredValue::Object(
                runtime.allocate_string_iterator(realm, string)?,
            )))
        }
        OperatorPrimitiveTarget::ErrorConstructorMessage(state) => {
            let message = operator_primitive_to_string(value, realm, origin)?;
            finish_error_constructor_message(runtime, state, message, return_to, execution_budget)
        }
        OperatorPrimitiveTarget::ErrorToStringName(state) => {
            let name = operator_primitive_to_string(value, realm, origin)?;
            finish_error_to_string_name(runtime, state, name, return_to, execution_budget)
        }
        OperatorPrimitiveTarget::ErrorToStringMessage(state) => {
            let message = operator_primitive_to_string(value, realm, origin)?;
            finish_error_to_string_message(state, message)
        }
        OperatorPrimitiveTarget::ArrayIteratorLength(state) => {
            finish_array_iterator_length(runtime, state, value, return_to, execution_budget)
        }
        OperatorPrimitiveTarget::FunctionApplyLength(state) => {
            finish_function_apply_length(runtime, state, value, return_to, execution_budget)
        }
        OperatorPrimitiveTarget::BigIntToString { value: receiver } => {
            let radix = operator_to_number(value, realm, origin)?;
            let radix = validated_radix(radix, realm, origin)?;
            bigint_prototype_to_string(&receiver, radix, realm, origin)
        }
        OperatorPrimitiveTarget::BigIntTruncationBits {
            value: pending_value,
            truncation,
        } => {
            // `bits` is a Number here; `ToIndex` bounds it before it becomes a
            // width, and then the value needs its own `ToBigInt`.
            let bits = operator_to_number(value, realm, origin)?;
            let Some(bits) = number_to_index(bits) else {
                return Err(NativeFailure::Abrupt(PendingException {
                    realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::RangeError,
                        message: JsString::from_utf8("invalid array index")?,
                    },
                    origin: origin.clone(),
                }));
            };
            begin_operator_primitive_conversion(
                runtime,
                pending_value,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::BigIntTruncationValue { bits, truncation },
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        OperatorPrimitiveTarget::BigIntTruncationValue { bits, truncation } => {
            let converted = to_bigint_from_primitive(&value, realm, origin)?;
            let bits = JsBigInt::from_u64(bits);
            bigint_truncate(&bits, &converted, truncation, realm, origin)
        }
        OperatorPrimitiveTarget::ArrayJoinSeparator(state)
        | OperatorPrimitiveTarget::ArrayJoinElement(state) => {
            advance_array_join(runtime, *state, Some(value), return_to, execution_budget)
        }
        OperatorPrimitiveTarget::ArraySearchPosition(state) => {
            advance_array_search(runtime, *state, Some(value), return_to, execution_budget)
        }
        OperatorPrimitiveTarget::ArrayMutatorArgument(state) => {
            advance_array_mutator(runtime, *state, Some(value), return_to, execution_budget)
        }
        OperatorPrimitiveTarget::ArrayCopierArgument(state) => {
            advance_array_copier(runtime, *state, Some(value), return_to, execution_budget)
        }
        OperatorPrimitiveTarget::ArraySpliceArgument(state) => {
            advance_array_splice(runtime, *state, Some(value), return_to, execution_budget)
        }
        OperatorPrimitiveTarget::StringMethodSubject(state)
        | OperatorPrimitiveTarget::StringMethodArgument(state) => {
            advance_string_method(runtime, *state, Some(value), return_to, execution_budget)
        }
        OperatorPrimitiveTarget::ArrayLengthWrite(state) => finish_array_length_write(
            runtime,
            state,
            value,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
    }
}

pub(super) fn finish_array_length_write(
    runtime: &mut Runtime,
    mut state: ArrayLengthWriteState,
    value: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let number = operator_to_number(value, realm, origin)?;
    if let Some(original) = state.original.take() {
        if state.first_length.is_some() {
            return Err(EngineFault::RuntimeInvariant {
                message: "array length conversion retained both an original value and first pass",
            }
            .into());
        }
        state.first_length = Some(number_to_uint32(number));
        return begin_operator_primitive_conversion(
            runtime,
            original,
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::ArrayLengthWrite(state),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        );
    }

    let length = match state.first_length {
        Some(first_length) if JsNumber::from_u32(first_length).strict_equals(number) => {
            first_length
        }
        Some(_) => return invalid_array_length(realm, origin),
        None => {
            let Some(length) = array_length_from_number(number) else {
                return invalid_array_length(realm, origin);
            };
            length
        }
    };
    let object = match &state.base {
        StoredValue::Object(object) => *object,
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "array length conversion lost its array base",
            }
            .into());
        }
    };
    if let Some(definition) = state.definition {
        return finish_array_length_definition(
            runtime,
            object,
            length,
            definition,
            state.base,
            &state.name,
            realm,
            origin,
            execution_budget,
        );
    }
    let work = runtime.preview_array_length_write_work(object, length)?;
    execution_budget.charge_instructions(work)?;
    match runtime.set_array_length(object, length)? {
        ArrayLengthWriteOutcome::Complete if state.reflect => {
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)))
        }
        ArrayLengthWriteOutcome::ReadOnly
        | ArrayLengthWriteOutcome::BlockedByNonConfigurable { .. }
            if state.reflect =>
        {
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
        }
        ArrayLengthWriteOutcome::Complete
        | ArrayLengthWriteOutcome::ReadOnly
        | ArrayLengthWriteOutcome::BlockedByNonConfigurable { .. }
            if !state.strict =>
        {
            Ok(NativeDispatch::Immediate(StoredValue::Undefined))
        }
        ArrayLengthWriteOutcome::Complete => Ok(NativeDispatch::Immediate(StoredValue::Undefined)),
        ArrayLengthWriteOutcome::ReadOnly => Err(NativeFailure::Abrupt(property_exception_at(
            realm,
            origin.clone(),
            Some(&state.name),
            PropertyFailure::ReadOnly,
        )?)),
        ArrayLengthWriteOutcome::BlockedByNonConfigurable { .. } => {
            Err(NativeFailure::Abrupt(property_exception_at(
                realm,
                origin.clone(),
                Some(&state.name),
                PropertyFailure::NotConfigurable,
            )?))
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "ArraySetLength completion keeps the converted length, descriptor flags, target completion, diagnostic identity, and budget explicit"
)]
fn finish_array_length_definition(
    runtime: &mut Runtime,
    object: ObjectId,
    length: u32,
    requested: ArrayLengthDefinition,
    target: StoredValue,
    name: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let key = runtime.predefined_property_key(PredefinedAtom::Length);
    let existing =
        runtime
            .array_own_property(object, &key)?
            .ok_or(EngineFault::RuntimeInvariant {
                message: "ArraySetLength target has no own length property",
            })?;
    let current_writable = existing.layout().writable() == Some(true);
    let definition = PropertyDefinition::data(
        Requested::Present(StoredValue::Number(JsNumber::from_u32(length))),
        requested_bool(requested.writable),
    )
    .with_enumerable(requested_bool(requested.enumerable))
    .with_configurable(requested_bool(requested.configurable));
    let final_writable = match validate_and_apply_existing(&definition, &existing) {
        DefinitionDecision::Rejected => {
            return array_length_definition_result(
                requested.result,
                false,
                target,
                realm,
                origin,
                name,
                PropertyFailure::NotConfigurable,
            );
        }
        DefinitionDecision::Unchanged => existing.layout().writable() == Some(true),
        DefinitionDecision::Replace(OwnProperty::Data { layout, .. }) => {
            layout.writable() == Some(true)
        }
        DefinitionDecision::Create(_)
        | DefinitionDecision::Replace(OwnProperty::Accessor { .. }) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "ArraySetLength validation changed the length property's kind or presence",
            }
            .into());
        }
    };
    let current_length = runtime
        .array_length(object)?
        .ok_or(EngineFault::RuntimeInvariant {
            message: "ArraySetLength target lost its Array state",
        })?;
    let outcome = if current_length == length {
        ArrayLengthWriteOutcome::Complete
    } else {
        let work = runtime.preview_array_length_write_work(object, length)?;
        execution_budget.charge_instructions(work)?;
        runtime.set_array_length(object, length)?
    };
    if current_writable && !final_writable {
        // ArraySetLength applies a requested false writable flag even when a
        // non-configurable index stops the shrink and makes the definition
        // itself return false.
        runtime.set_array_length_writable(object, false)?;
    }
    match outcome {
        ArrayLengthWriteOutcome::Complete => array_length_definition_result(
            requested.result,
            true,
            target,
            realm,
            origin,
            name,
            PropertyFailure::NotConfigurable,
        ),
        ArrayLengthWriteOutcome::BlockedByNonConfigurable { .. } => array_length_definition_result(
            requested.result,
            false,
            target,
            realm,
            origin,
            name,
            PropertyFailure::NotConfigurable,
        ),
        ArrayLengthWriteOutcome::ReadOnly => Err(EngineFault::RuntimeInvariant {
            message: "validated ArraySetLength definition reached a read-only mutation",
        }
        .into()),
    }
}

const fn requested_bool(value: Option<bool>) -> Requested<bool> {
    match value {
        Some(value) => Requested::Present(value),
        None => Requested::Absent,
    }
}

fn array_length_definition_result(
    result: DefinePropertyResult,
    success: bool,
    target: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
    name: &JsString,
    failure: PropertyFailure,
) -> Result<NativeDispatch, NativeFailure> {
    match (result, success) {
        (DefinePropertyResult::Target, true) => Ok(NativeDispatch::Immediate(target)),
        (DefinePropertyResult::Boolean, _) => {
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(success)))
        }
        (DefinePropertyResult::Target, false) => Err(NativeFailure::Abrupt(property_exception_at(
            realm,
            origin.clone(),
            Some(name),
            failure,
        )?)),
    }
}

fn invalid_array_length(
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::RangeError,
            message: JsString::from_utf8("invalid array length")?,
        },
        origin: origin.clone(),
    }))
}

/// Validates a `toString` radix, which must lie in `2..=36`.
fn validated_radix(
    radix: JsNumber,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<u32, NativeFailure> {
    let radix = saturated_i32_from_number(radix);
    u32::try_from(radix)
        .ok()
        .filter(|radix| (2..=36).contains(radix))
        .ok_or(())
        .or_else(|()| {
            Err(NativeFailure::Abrupt(PendingException {
                realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::RangeError,
                    message: JsString::from_utf8("radix must be between 2 and 36")?,
                },
                origin: origin.clone(),
            }))
        })
}

fn finish_number_to_string_radix(
    number: JsNumber,
    radix: JsNumber,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let radix = saturated_i32_from_number(radix);
    let Some(radix) = u32::try_from(radix)
        .ok()
        .filter(|radix| (2..=36).contains(radix))
    else {
        return Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("radix must be between 2 and 36")?,
            },
            origin: origin.clone(),
        }));
    };
    Ok(NativeDispatch::Immediate(StoredValue::String(
        number.to_radix_string(radix)?,
    )))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "Rust float-to-int casts exactly provide QuickJS JS_ToInt32Sat semantics: truncation, saturation, and NaN to zero"
)]
fn saturated_i32_from_number(number: JsNumber) -> i32 {
    number.as_f64() as i32
}

const fn binary_operator_converts_left_to_number_first(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::Mul
            | FinalOpcode::Div
            | FinalOpcode::Mod
            | FinalOpcode::Sub
            | FinalOpcode::Pow
            | FinalOpcode::Shl
            | FinalOpcode::Sar
            | FinalOpcode::Shr
            | FinalOpcode::And
            | FinalOpcode::Xor
            | FinalOpcode::Or
    )
}

fn apply_unary_operator(
    opcode: FinalOpcode,
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    // A `BigInt` operand keeps the whole operation in the BigInt domain, except
    // for unary `+`, which has no BigInt form at all.
    if let StoredValue::BigInt(value) = &value {
        return apply_bigint_unary_operator(opcode, value, realm, origin);
    }
    let number = operator_to_number(value, realm, origin)?;
    let dispatch = match opcode {
        FinalOpcode::Plus => NativeDispatch::Immediate(StoredValue::Number(number)),
        FinalOpcode::Neg => {
            NativeDispatch::Immediate(StoredValue::Number(JsNumber::from_f64(-number.as_f64())))
        }
        FinalOpcode::Inc => NativeDispatch::Immediate(StoredValue::Number(
            number.add_numeric(JsNumber::from_i32(1)),
        )),
        FinalOpcode::Dec => NativeDispatch::Immediate(StoredValue::Number(
            number.add_numeric(JsNumber::from_i32(-1)),
        )),
        FinalOpcode::PostInc => NativeDispatch::Pair(
            StoredValue::Number(number),
            StoredValue::Number(number.add_numeric(JsNumber::from_i32(1))),
        ),
        FinalOpcode::PostDec => NativeDispatch::Pair(
            StoredValue::Number(number),
            StoredValue::Number(number.add_numeric(JsNumber::from_i32(-1))),
        ),
        FinalOpcode::Not => NativeDispatch::Immediate(StoredValue::Number(JsNumber::from_i32(
            !number_to_int32(number),
        ))),
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-unary opcode reached unary dynamic-operator execution",
            }
            .into());
        }
    };
    Ok(dispatch)
}

fn apply_binary_operator(
    opcode: FinalOpcode,
    left: StoredValue,
    right: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    match opcode {
        FinalOpcode::Add => apply_addition(left, right, realm, origin),
        FinalOpcode::Mul
        | FinalOpcode::Div
        | FinalOpcode::Mod
        | FinalOpcode::Sub
        | FinalOpcode::Pow => apply_numeric_arithmetic(opcode, left, right, realm, origin),
        FinalOpcode::Shl
        | FinalOpcode::Sar
        | FinalOpcode::Shr
        | FinalOpcode::And
        | FinalOpcode::Xor
        | FinalOpcode::Or => apply_numeric_bitwise(opcode, left, right, realm, origin),
        FinalOpcode::Lt | FinalOpcode::Lte | FinalOpcode::Gt | FinalOpcode::Gte => {
            apply_relational(opcode, left, right, realm, origin)
        }
        _ => Err(EngineFault::RuntimeInvariant {
            message: "unsupported opcode reached binary dynamic-operator execution",
        }
        .into()),
    }
}

fn apply_addition(
    left: StoredValue,
    right: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(left, StoredValue::String(_)) || matches!(right, StoredValue::String(_)) {
        let left = operator_primitive_to_string(left, realm, origin)?;
        let right = operator_primitive_to_string(right, realm, origin)?;
        let value = match left.concat(&right) {
            Ok(value) => value,
            Err(JsStringError::TooLong { .. }) => {
                return Err(NativeFailure::Abrupt(PendingException {
                    realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::InternalError,
                        message: JsString::from_utf8("string too long")?,
                    },
                    origin: origin.clone(),
                }));
            }
            Err(error) => return Err(error.into()),
        };
        return Ok(NativeDispatch::Immediate(StoredValue::String(value)));
    }
    // String concatenation wins over the numeric domains, so the BigInt check
    // comes after it: `1n + "s"` concatenates while `1n + 1` throws.
    if let Some(dispatch) = apply_bigint_addition(&left, &right, realm, origin)? {
        return Ok(dispatch);
    }
    let left = operator_to_number(left, realm, origin)?;
    let right = operator_to_number(right, realm, origin)?;
    Ok(NativeDispatch::Immediate(StoredValue::Number(
        left.add_numeric(right),
    )))
}

fn apply_numeric_arithmetic(
    opcode: FinalOpcode,
    left: StoredValue,
    right: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(dispatch) = apply_bigint_arithmetic(opcode, &left, &right, realm, origin)? {
        return Ok(dispatch);
    }
    let left = operator_to_number(left, realm, origin)?.as_f64();
    let right = operator_to_number(right, realm, origin)?.as_f64();
    let result = match opcode {
        FinalOpcode::Mul => left * right,
        FinalOpcode::Div => left / right,
        FinalOpcode::Mod => left % right,
        FinalOpcode::Sub => left - right,
        FinalOpcode::Pow if !right.is_finite() && left.abs().to_bits() == 1.0_f64.to_bits() => {
            f64::NAN
        }
        FinalOpcode::Pow => left.powf(right),
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-arithmetic opcode reached numeric arithmetic",
            }
            .into());
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::Number(
        JsNumber::from_f64(result),
    )))
}

fn apply_numeric_bitwise(
    opcode: FinalOpcode,
    left: StoredValue,
    right: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(dispatch) = apply_bigint_bitwise(opcode, &left, &right, realm, origin)? {
        return Ok(dispatch);
    }
    let left = operator_to_number(left, realm, origin)?;
    let right = operator_to_number(right, realm, origin)?;
    let shift = number_to_uint32(right) & 0x1f;
    let result = match opcode {
        FinalOpcode::Shl => StoredValue::Number(JsNumber::from_i32(
            number_to_int32(left).wrapping_shl(shift),
        )),
        FinalOpcode::Sar => StoredValue::Number(JsNumber::from_i32(number_to_int32(left) >> shift)),
        FinalOpcode::Shr => {
            StoredValue::Number(JsNumber::from_u32(number_to_uint32(left) >> shift))
        }
        FinalOpcode::And => StoredValue::Number(JsNumber::from_i32(
            number_to_int32(left) & number_to_int32(right),
        )),
        FinalOpcode::Xor => StoredValue::Number(JsNumber::from_i32(
            number_to_int32(left) ^ number_to_int32(right),
        )),
        FinalOpcode::Or => StoredValue::Number(JsNumber::from_i32(
            number_to_int32(left) | number_to_int32(right),
        )),
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-bitwise opcode reached numeric bitwise execution",
            }
            .into());
        }
    };
    Ok(NativeDispatch::Immediate(result))
}

fn apply_relational(
    opcode: FinalOpcode,
    left: StoredValue,
    right: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let result = match (left, right) {
        (StoredValue::String(left), StoredValue::String(right)) => match opcode {
            FinalOpcode::Lt => left < right,
            FinalOpcode::Lte => left <= right,
            FinalOpcode::Gt => left > right,
            FinalOpcode::Gte => left >= right,
            _ => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "non-relational opcode reached string comparison",
                }
                .into());
            }
        },
        (left, right) => {
            // Relational comparison is the one place the two numeric domains do
            // mix: `1n < 2` is `true`. The comparison is mathematical, so a
            // `BigInt` operand is compared exactly rather than rounded.
            let comparison = bigint_relational_ordering(&left, &right, realm, origin)?;
            if comparison != BigIntComparison::NotApplicable {
                let ordering = match comparison {
                    BigIntComparison::Ordered(ordering) => Some(ordering),
                    // An unordered comparison makes every relational operator
                    // `false`, which is the `NaN` behavior.
                    BigIntComparison::Unordered => None,
                    BigIntComparison::NotApplicable => unreachable!("checked above"),
                };
                let result = match opcode {
                    FinalOpcode::Lt => ordering == Some(Ordering::Less),
                    FinalOpcode::Lte => {
                        matches!(ordering, Some(Ordering::Less | Ordering::Equal))
                    }
                    FinalOpcode::Gt => ordering == Some(Ordering::Greater),
                    FinalOpcode::Gte => {
                        matches!(ordering, Some(Ordering::Greater | Ordering::Equal))
                    }
                    _ => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "non-relational opcode reached BigInt comparison",
                        }
                        .into());
                    }
                };
                return Ok(NativeDispatch::Immediate(StoredValue::Boolean(result)));
            }
            let left = operator_to_number(left, realm, origin)?.as_f64();
            let right = operator_to_number(right, realm, origin)?.as_f64();
            match opcode {
                FinalOpcode::Lt => left < right,
                FinalOpcode::Lte => left <= right,
                FinalOpcode::Gt => left > right,
                FinalOpcode::Gte => left >= right,
                _ => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "non-relational opcode reached numeric comparison",
                    }
                    .into());
                }
            }
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::Boolean(result)))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the resumable equality operation owns distinct verified call, realm, source, and fuel capabilities"
)]
pub(super) fn begin_abstract_equality(
    runtime: &mut Runtime,
    mut left: StoredValue,
    mut right: StoredValue,
    opcode: FinalOpcode,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let invert = match opcode {
        FinalOpcode::Eq => false,
        FinalOpcode::Neq => true,
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "non-equality opcode reached abstract equality",
            }
            .into());
        }
    };

    loop {
        if left.kind() == right.kind() || (is_object_value(&left) && is_object_value(&right)) {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(
                left.strict_equals(&right) ^ invert,
            )));
        }
        if matches!(
            (&left, &right),
            (StoredValue::Null, StoredValue::Undefined)
                | (StoredValue::Undefined, StoredValue::Null)
        ) {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(!invert)));
        }

        // A `BigInt` compares across the domains by mathematical value, so the
        // Boolean-to-Number rewrite below must not reach it: `0n == false` is
        // `true` through the BigInt comparison, not through a rounded Number.
        let comparison = bigint_relational_ordering(&left, &right, realm, &origin)?;
        if comparison != BigIntComparison::NotApplicable {
            let equal = comparison == BigIntComparison::Ordered(Ordering::Equal);
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(
                equal ^ invert,
            )));
        }

        match (&left, &right) {
            (StoredValue::String(_), StoredValue::Number(_)) => {
                left = StoredValue::Number(operator_to_number(left, realm, &origin)?);
                continue;
            }
            (StoredValue::Number(_), StoredValue::String(_)) => {
                right = StoredValue::Number(operator_to_number(right, realm, &origin)?);
                continue;
            }
            (StoredValue::Boolean(value), _) => {
                left = StoredValue::Number(JsNumber::from_i32(i32::from(*value)));
                continue;
            }
            (_, StoredValue::Boolean(value)) => {
                right = StoredValue::Number(JsNumber::from_i32(i32::from(*value)));
                continue;
            }
            _ => {}
        }

        if is_object_value(&left) && is_equality_conversion_primitive(&right) {
            return begin_operator_primitive_conversion(
                runtime,
                left,
                OperatorPrimitiveHint::Default,
                OperatorPrimitiveTarget::EqualityFinish {
                    opcode,
                    other: right,
                },
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        if is_object_value(&right) && is_equality_conversion_primitive(&left) {
            return begin_operator_primitive_conversion(
                runtime,
                right,
                OperatorPrimitiveHint::Default,
                OperatorPrimitiveTarget::EqualityFinish {
                    opcode,
                    other: left,
                },
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }

        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(invert)));
    }
}

const fn is_object_value(value: &StoredValue) -> bool {
    matches!(value, StoredValue::Function(_) | StoredValue::Object(_))
}

const fn is_equality_conversion_primitive(value: &StoredValue) -> bool {
    matches!(
        value,
        StoredValue::Number(_) | StoredValue::String(_) | StoredValue::Symbol(_)
    )
}

/// Renders a `BigInt` as the decimal string `ToString` produces.
///
/// There is no `n` suffix: the suffix belongs to the literal grammar and to
/// `console.log` formatting, not to `ToString`, so `String(1n)` is `"1"`.
pub(super) fn bigint_decimal_string(value: &JsBigInt) -> Result<JsString, NativeFailure> {
    let text = value
        .to_string_radix(10)
        .map_err(|_| EngineFault::RuntimeInvariant {
            message: "decimal BigInt rendering rejected the base-10 radix",
        })?;
    Ok(JsString::from_utf8(&text)?)
}

pub(super) fn operator_to_number(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<JsNumber, NativeFailure> {
    match value {
        StoredValue::Undefined => Ok(JsNumber::from_f64(f64::NAN)),
        StoredValue::Null | StoredValue::Boolean(false) => Ok(JsNumber::from_i32(0)),
        StoredValue::Boolean(true) => Ok(JsNumber::from_i32(1)),
        StoredValue::Number(value) => Ok(value),
        StoredValue::String(value) => Ok(string_to_number(&value)?),
        // A `BigInt` never implicitly becomes a Number, which is what keeps the
        // two numeric domains from silently mixing.
        StoredValue::BigInt(_) => Err(primitive_conversion_type_error(
            realm,
            origin,
            "cannot convert bigint to number",
        )?),
        StoredValue::Symbol(_) => Err(primitive_conversion_type_error(
            realm,
            origin,
            "cannot convert symbol to number",
        )?),
        StoredValue::Function(_) | StoredValue::Object(_) => Err(EngineFault::RuntimeInvariant {
            message: "object reached primitive operator Number conversion",
        }
        .into()),
    }
}

/// Applies ECMAScript `ToNumeric` to an already-primitive value, then narrows
/// the result to a Number.
///
/// `ToNumeric` differs from `ToNumber` only for a `BigInt`, which it admits
/// instead of rejecting (`JS_ToNumericFree`, `quickjs.c:13025`). The `Number`
/// constructor is the caller that wants this: it accepts a `BigInt` and rounds
/// it to the nearest binary64, while every operator keeps using
/// [`operator_to_number`] so the two numeric domains stay separate.
pub(super) fn operator_to_numeric(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<JsNumber, NativeFailure> {
    match value {
        StoredValue::BigInt(value) => Ok(JsNumber::from_f64(value.to_f64())),
        value => operator_to_number(value, realm, origin),
    }
}

pub(super) fn operator_primitive_to_string(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<JsString, NativeFailure> {
    match value {
        StoredValue::Undefined => Ok(JsString::from_utf8("undefined")?),
        StoredValue::Null => Ok(JsString::from_utf8("null")?),
        StoredValue::Boolean(false) => Ok(JsString::from_utf8("false")?),
        StoredValue::Boolean(true) => Ok(JsString::from_utf8("true")?),
        StoredValue::Number(value) => Ok(value.to_javascript_string()?),
        StoredValue::BigInt(value) => Ok(bigint_decimal_string(&value)?),
        StoredValue::String(value) => Ok(value),
        StoredValue::Symbol(_) => Err(primitive_conversion_type_error(
            realm,
            origin,
            "cannot convert symbol to string",
        )?),
        StoredValue::Function(_) | StoredValue::Object(_) => Err(EngineFault::RuntimeInvariant {
            message: "object reached primitive operator String conversion",
        }
        .into()),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the completed property-key dispatcher keeps read, resumable array-length write, accessor write, and method-definition outcomes at one audited boundary"
)]
fn finish_property_key_target(
    runtime: &mut Runtime,
    value: StoredValue,
    target: PropertyKeyTarget,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let value = property_key_primitive_to_value(value)?;
    if matches!(target, PropertyKeyTarget::ToKey) {
        return Ok(NativeDispatch::Immediate(value));
    }
    let property = computed_property_operand(runtime, &value)?;
    match target {
        PropertyKeyTarget::ToKey => Err(EngineFault::RuntimeInvariant {
            message: "property-key conversion lost its ToKey fast path",
        }
        .into()),
        PropertyKeyTarget::Read { base, realm } => {
            match read_static_property(runtime, realm, &base, &property.key)? {
                PropertyReadOutcome::Value(value) => Ok(NativeDispatch::Immediate(value)),
                PropertyReadOutcome::Getter { function, receiver } => {
                    Ok(NativeDispatch::Call(NativeCall {
                        function,
                        receiver,
                        arguments: CallArguments::empty(),
                        return_to,
                        origin: origin.clone(),
                        continuations: Vec::new(),
                        pre_call: None,
                        new_target: None,
                        native_caller: None,
                    }))
                }
                PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(
                    property_exception_at(realm, origin.clone(), Some(&property.name), failure)?,
                )),
            }
        }
        PropertyKeyTarget::Write {
            base,
            value,
            strict,
            realm,
        } => {
            if is_array_length_target(runtime, &base, &property.key)? {
                let target = array_length_write_target(base, property.name, strict, false, &value);
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::Number,
                    target,
                    realm,
                    return_to,
                    origin.clone(),
                    execution_budget,
                );
            }
            match write_static_property(
                runtime,
                realm,
                &base,
                property.key,
                value,
                strict,
                execution_budget,
            )? {
                PropertyWriteOutcome::Complete => {
                    Ok(NativeDispatch::Immediate(StoredValue::Undefined))
                }
                PropertyWriteOutcome::Setter {
                    function,
                    receiver,
                    value,
                } => {
                    let mut arguments = Vec::new();
                    arguments.try_reserve_exact(1).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::FrameValues,
                            additional: 1,
                        }
                    })?;
                    arguments.push(value);
                    Ok(NativeDispatch::Call(NativeCall {
                        function,
                        receiver,
                        arguments: CallArguments::from_values(arguments),
                        return_to,
                        origin: origin.clone(),
                        continuations: Vec::new(),
                        pre_call: None,
                        new_target: None,
                        native_caller: None,
                    }))
                }
                PropertyWriteOutcome::Failed(failure) => Err(NativeFailure::Abrupt(
                    property_exception_at(realm, origin.clone(), Some(&property.name), failure)?,
                )),
            }
        }
        PropertyKeyTarget::DefineProperty {
            target,
            descriptor,
            realm,
        } => begin_define_property(
            runtime,
            realm,
            target,
            property.key,
            property.name,
            descriptor,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        PropertyKeyTarget::ReflectDefineProperty {
            target,
            descriptor,
            realm,
        } => begin_define_property_with_result(
            runtime,
            realm,
            target,
            property.key,
            property.name,
            descriptor,
            return_to,
            origin.clone(),
            execution_budget,
            DefinePropertyResult::Boolean,
        ),
        PropertyKeyTarget::OwnPropertyDescriptor { target, realm }
        | PropertyKeyTarget::ReflectOwnPropertyDescriptor { target, realm } => {
            own_property_descriptor(runtime, realm, &target, &property.key, origin)
        }
        // `hasOwnProperty` and `propertyIsEnumerable` share one own-property
        // resolution with `getOwnPropertyDescriptor`, so all three agree on
        // every exotic case.
        PropertyKeyTarget::HasOwnProperty { target, realm } => {
            let own = resolve_own_property(runtime, realm, &target, &property.key, origin)?;
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(
                own.is_some(),
            )))
        }
        PropertyKeyTarget::PropertyIsEnumerable { target, realm } => {
            // An absent property is not enumerable, and neither is an inherited
            // one: the test is on the own property only.
            let own = resolve_own_property(runtime, realm, &target, &property.key, origin)?;
            let enumerable = own.is_some_and(|own| {
                let layout = match own {
                    OwnProperty::Data { layout, .. } | OwnProperty::Accessor { layout, .. } => {
                        layout
                    }
                };
                layout.is_enumerable()
            });
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(enumerable)))
        }
        PropertyKeyTarget::ReflectHas { target, realm } => Ok(NativeDispatch::Immediate(
            StoredValue::Boolean(has_property(runtime, realm, &target, &property.key)?),
        )),
        PropertyKeyTarget::ReflectGet { target, receiver } => {
            charge_heap_property_lookup(runtime, &target, execution_budget)?;
            let reference = target
                .heap_reference()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Reflect.get lost its validated target",
                })?;
            match read_heap_property_for_receiver(runtime, reference, receiver, &property.key)? {
                PropertyReadOutcome::Value(value) => Ok(NativeDispatch::Immediate(value)),
                PropertyReadOutcome::Getter { function, receiver } => {
                    Ok(NativeDispatch::Call(NativeCall {
                        function,
                        receiver,
                        arguments: CallArguments::empty(),
                        return_to,
                        origin: origin.clone(),
                        continuations: Vec::new(),
                        pre_call: None,
                        new_target: None,
                        native_caller: None,
                    }))
                }
                PropertyReadOutcome::Failed(_) => Err(EngineFault::RuntimeInvariant {
                    message: "Reflect.get failed an object-valued property read",
                }
                .into()),
            }
        }
        PropertyKeyTarget::ReflectSet {
            target,
            receiver,
            value,
            realm,
        } => reflect_set_property(
            runtime,
            realm,
            target,
            property.key,
            property.name,
            value,
            receiver,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        PropertyKeyTarget::Delete {
            base,
            strict,
            realm,
        } => match delete_static_property(runtime, &base, &property.key)? {
            PropertyDeleteOutcome::Deleted => {
                Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)))
            }
            PropertyDeleteOutcome::Refused if strict => {
                Err(NativeFailure::Abrupt(property_exception_at(
                    realm,
                    origin.clone(),
                    Some(&property.name),
                    PropertyFailure::NotDeletable,
                )?))
            }
            PropertyDeleteOutcome::Refused => {
                Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
            }
            PropertyDeleteOutcome::Failed(failure) => Err(NativeFailure::Abrupt(
                property_exception_at(realm, origin.clone(), Some(&property.name), failure)?,
            )),
        },
        PropertyKeyTarget::DefineMethod {
            base,
            function,
            kind,
            enumerable,
            realm,
        } => {
            let StoredValue::Function(function) = function else {
                return Err(NativeFailure::Abrupt(PendingException {
                    realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::TypeError,
                        message: JsString::from_utf8("not a function")?,
                    },
                    origin: origin.clone(),
                }));
            };
            let name = computed_method_name(&value)?;
            match define_static_method(
                runtime,
                &base,
                property.key,
                &name,
                function,
                kind,
                enumerable,
            )? {
                PropertyDefinitionOutcome::Complete => {
                    Ok(NativeDispatch::Immediate(StoredValue::Undefined))
                }
                PropertyDefinitionOutcome::Failed(failure) => Err(NativeFailure::Abrupt(
                    property_exception_at(realm, origin.clone(), Some(&property.name), failure)?,
                )),
            }
        }
    }
}

fn property_key_primitive_to_value(value: StoredValue) -> Result<StoredValue, NativeFailure> {
    Ok(match value {
        StoredValue::Undefined => StoredValue::String(JsString::from_utf8("undefined")?),
        StoredValue::Null => StoredValue::String(JsString::from_utf8("null")?),
        StoredValue::Boolean(false) => StoredValue::String(JsString::from_utf8("false")?),
        StoredValue::Boolean(true) => StoredValue::String(JsString::from_utf8("true")?),
        StoredValue::Number(value) => StoredValue::String(value.to_javascript_string()?),
        // `ToPropertyKey` stringifies a `BigInt`, so `o[1n]` addresses the same
        // slot as `o[1]`.
        StoredValue::BigInt(value) => StoredValue::String(bigint_decimal_string(&value)?),
        value @ (StoredValue::String(_) | StoredValue::Symbol(_)) => value,
        StoredValue::Function(_) | StoredValue::Object(_) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "object reached primitive property-key conversion",
            }
            .into());
        }
    })
}

pub(super) fn computed_property_operand(
    runtime: &mut Runtime,
    value: &StoredValue,
) -> Result<StaticPropertyOperand, ExecutionError> {
    match value {
        StoredValue::String(name) => Ok(StaticPropertyOperand {
            key: runtime.property_key_from_string(name)?,
            name: name.clone(),
        }),
        StoredValue::Symbol(atom) => Ok(StaticPropertyOperand {
            key: runtime.property_key_from_symbol(atom)?,
            name: atom.description().cloned().unwrap_or_else(JsString::empty),
        }),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::Function(_)
        | StoredValue::Object(_) => Err(EngineFault::RuntimeInvariant {
            message: "computed property operand was not a verified property-key value",
        }
        .into()),
    }
}

fn computed_method_name(value: &StoredValue) -> Result<JsString, NativeFailure> {
    match value {
        StoredValue::String(name) => Ok(name.clone()),
        StoredValue::Symbol(atom) => atom.description().map_or_else(
            || Ok(JsString::empty()),
            |description| {
                Ok(JsString::from_utf8("[")?
                    .concat(description)?
                    .concat(&JsString::from_utf8("]")?)?)
            },
        ),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::Function(_)
        | StoredValue::Object(_) => Err(EngineFault::RuntimeInvariant {
            message: "computed method name was not a verified property-key value",
        }
        .into()),
    }
}

fn primitive_conversion_type_error(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<NativeFailure, JsStringError> {
    Ok(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    }))
}

/// Completes one `Number.prototype` decimal rendering.
///
/// The digit count has already been converted; this applies its bounds and then
/// renders the value exactly. The bounds differ per method: `toFixed` and
/// `toExponential` admit `0..=100`, while `toPrecision` admits `1..=100`, and an
/// out-of-range count reports `RangeError: invalid number of digits`.
pub(super) fn finish_number_format(
    number: JsNumber,
    format: NumberFormat,
    digits: JsNumber,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let value = number.as_f64();
    // Only `toFixed` validates its digit count before short-circuiting a
    // non-finite value. The oracle draws that line sharply:
    // `(NaN).toFixed(101)` is a `RangeError` while `(NaN).toExponential(101)`
    // and `(NaN).toPrecision(101)` are both `"NaN"`.
    // An absent argument arrives as `undefined`, which `ToIntegerOrInfinity`
    // maps to `0`; that is the correct default for all three methods.
    let requested = number_to_integer_or_infinity(digits);

    match format {
        NumberFormat::Fixed => {
            let count = bounded_digits(requested, 0, 100, realm, origin)?;
            if !value.is_finite() {
                return Ok(NativeDispatch::Immediate(StoredValue::String(
                    value_to_string(value)?,
                )));
            }
            // A magnitude at or above 1e21 falls back to the ordinary Number
            // rendering, so `(1e21).toFixed(2)` is `"1e+21"`.
            if value.abs() >= 1e21 {
                return Ok(NativeDispatch::Immediate(StoredValue::String(
                    number.to_javascript_string()?,
                )));
            }
            let rendered = exact_fixed(value, count).map_err(bigint_render_failure)?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&rendered)?,
            )))
        }
        NumberFormat::Exponential => {
            // Like `toPrecision`, this short-circuits before validating.
            if !value.is_finite() {
                return Ok(NativeDispatch::Immediate(StoredValue::String(
                    value_to_string(value)?,
                )));
            }
            let count = bounded_digits(requested, 0, 100, realm, origin)?;
            let rendered = render_exponential(value, count).map_err(bigint_render_failure)?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&rendered)?,
            )))
        }
        NumberFormat::Precision => {
            // Non-finite values short-circuit before the digit check.
            if !value.is_finite() {
                return Ok(NativeDispatch::Immediate(StoredValue::String(
                    value_to_string(value)?,
                )));
            }
            let count = bounded_digits(requested, 1, 100, realm, origin)?;
            let rendered = render_precision(value, count).map_err(bigint_render_failure)?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&rendered)?,
            )))
        }
    }
}

/// Validates a digit count against its inclusive bounds.
fn bounded_digits(
    requested: f64,
    low: u32,
    high: u32,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<u32, NativeFailure> {
    if requested >= f64::from(low) && requested <= f64::from(high) {
        // The bounds prove the value is a small non-negative integer.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the preceding bounds keep the count within 0..=100"
        )]
        let count = requested as u32;
        return Ok(count);
    }
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::RangeError,
            message: JsString::from_utf8("invalid number of digits")?,
        },
        origin: origin.clone(),
    }))
}

/// Renders `NaN` and the infinities.
fn value_to_string(value: f64) -> Result<JsString, JsStringError> {
    if value.is_nan() {
        return JsString::from_utf8("NaN");
    }
    if value > 0.0 {
        return JsString::from_utf8("Infinity");
    }
    JsString::from_utf8("-Infinity")
}

/// Renders `value` in exponential notation with `fraction_digits` after the
/// point.
fn render_exponential(value: f64, fraction_digits: u32) -> Result<String, BigIntError> {
    let rendered = exact_significant(value, fraction_digits.saturating_add(1))?;
    Ok(assemble_exponential(&rendered))
}

/// Renders `value` with `precision` significant digits.
///
/// The exponent decides the spelling: a value whose exponent falls outside
/// `-6..precision` uses exponential notation, which is why
/// `(0.000001).toPrecision(2)` is `"0.0000010"` while `(12345).toPrecision(2)` is
/// `"1.2e+4"`.
fn render_precision(value: f64, precision: u32) -> Result<String, BigIntError> {
    let rendered = exact_significant(value, precision)?;
    let exponent = rendered.exponent;
    let precision_bound = i32::try_from(precision).unwrap_or(i32::MAX);
    if exponent < -6 || exponent >= precision_bound {
        return Ok(assemble_exponential(&rendered));
    }
    Ok(assemble_fixed(&rendered))
}

/// Assembles a `d.ddde±x` spelling from exact digits.
fn assemble_exponential(rendered: &DecimalDigits) -> String {
    let mut out = String::new();
    if rendered.negative && !rendered.digits.bytes().all(|byte| byte == b'0') {
        out.push('-');
    }
    let mut characters = rendered.digits.chars();
    if let Some(first) = characters.next() {
        out.push(first);
    }
    let remainder: String = characters.collect();
    if !remainder.is_empty() {
        out.push('.');
        out.push_str(&remainder);
    }
    out.push('e');
    // Zero always reports exponent `0`, so the sign is always explicit.
    let exponent = if rendered.digits.bytes().all(|byte| byte == b'0') {
        0
    } else {
        rendered.exponent
    };
    if exponent < 0 {
        out.push('-');
    } else {
        out.push('+');
    }
    out.push_str(&exponent.abs().to_string());
    out
}

/// Assembles a positional spelling from exact digits.
fn assemble_fixed(rendered: &DecimalDigits) -> String {
    let mut out = String::new();
    if rendered.negative && !rendered.digits.bytes().all(|byte| byte == b'0') {
        out.push('-');
    }
    let exponent = rendered.exponent;
    let digits = rendered.digits.as_bytes();
    if exponent < 0 {
        // A leading `0.` plus enough zeroes to place the first digit.
        out.push_str("0.");
        for _ in 0..(-exponent - 1) {
            out.push('0');
        }
        for byte in digits {
            out.push(char::from(*byte));
        }
        return out;
    }
    let integer_digits = usize::try_from(exponent).unwrap_or(0).saturating_add(1);
    for (index, byte) in digits.iter().enumerate() {
        if index == integer_digits {
            out.push('.');
        }
        out.push(char::from(*byte));
    }
    // A precision shorter than the integer part pads with zeroes.
    for _ in digits.len()..integer_digits {
        out.push('0');
    }
    out
}

/// Reports an exact-rendering failure as an engine fault.
///
/// Every admitted input stays far inside the `BigInt` limb cap, so reaching this
/// means an internal invariant broke rather than a script doing something the
/// specification allows.
fn bigint_render_failure(_error: BigIntError) -> NativeFailure {
    EngineFault::RuntimeInvariant {
        message: "exact decimal rendering exceeded the BigInt limb cap",
    }
    .into()
}

/// Completes a decimal rendering whose digit count was absent.
///
/// The three defaults differ. `toFixed()` is `toFixed(0)`. `toPrecision()` is
/// plain `ToString`, so `(123.456).toPrecision()` is `"123.456"`.
/// `toExponential()` uses as many fraction digits as the value needs rather than
/// a fixed count, which is why `(123.456).toExponential()` is `"1.23456e+2"`.
pub(super) fn finish_number_format_default(
    number: JsNumber,
    format: NumberFormat,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    match format {
        NumberFormat::Fixed => {
            finish_number_format(number, format, JsNumber::from_i32(0), realm, origin)
        }
        NumberFormat::Precision => Ok(NativeDispatch::Immediate(StoredValue::String(
            number.to_javascript_string()?,
        ))),
        NumberFormat::Exponential => {
            let value = number.as_f64();
            if !value.is_finite() {
                return Ok(NativeDispatch::Immediate(StoredValue::String(
                    value_to_string(value)?,
                )));
            }
            let rendered = render_shortest_exponential(value).map_err(bigint_render_failure)?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&rendered)?,
            )))
        }
    }
}

/// Renders exponential notation with the fewest digits that round-trip.
///
/// `toExponential` with no argument uses "as many digits as necessary to
/// uniquely specify the number", so the shortest round-tripping precision is
/// found by trying each in turn.
fn render_shortest_exponential(value: f64) -> Result<String, BigIntError> {
    for precision in 1..=17_u32 {
        let rendered = exact_significant(value, precision)?;
        let candidate = assemble_exponential(&rendered);
        // A candidate that parses back to the same bits is short enough.
        if candidate
            .parse::<f64>()
            .is_ok_and(|parsed| parsed.to_bits() == value.to_bits())
        {
            return Ok(candidate);
        }
    }
    // Seventeen significant digits always round-trip a binary64.
    let rendered = exact_significant(value, 17)?;
    Ok(assemble_exponential(&rendered))
}
