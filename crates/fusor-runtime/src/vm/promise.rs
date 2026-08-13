//! ECMAScript Promise core and runtime-owned job execution.

use super::array_from_async::begin_array_from_async_resume;
use super::async_function::begin_async_function_resume;
use super::async_generator::begin_async_generator_await_resume;
use super::{
    Arc, CallArguments, CallInputs, CallReturn, CollectionRoot, EngineFault, ExceptionKind,
    ExecutionBudget, ExecutionError, ExecutionLimits, FunctionId, HeapFunction, HeapReference,
    IntrinsicGetContinuation, JsStackFrame, JsString, NativeCall, NativeContinuation,
    NativeDispatch, NativeFailure, NativeFunction, ObjectId, OrdinaryDynamicFunctionCompiler,
    PendingException, PendingExceptionPayload, PredefinedAtom, PromiseCapabilityCapture,
    PromiseCapabilityExecutor, PromiseCapabilityPurpose, PromiseCombinatorKind,
    PromiseContinuation, PromiseFinallyFunction, PromiseFinallyState, PromiseFinallyThenState,
    PromiseFinallyThunkKind, PromiseJob, PromiseResolvingFunction, PromiseResolvingKind,
    PromiseStatic, PromiseThenState, PropertyKey, Rc, RealmId, RefCell, Runtime, RuntimeResource,
    StoredValue, attach_native_continuations, begin_internal_get, begin_promise_combinator,
    begin_value_get, charge_heap_property_lookup, check_execution_limit, continue_get_after,
    continue_intrinsic_get_after, execute_root_dispatch_with_budget, function_is_constructor,
    native_function_host_origin, prepend_native_continuations, resolve_native_dispatch,
    trace_stored_value_root, usize_to_u64,
};
use crate::object::{
    HeapObject, PromiseCapability, PromiseReaction, PromiseReactionKind, PromiseReactionTarget,
    PromiseState,
};
use crate::promise_rejection::PromiseRejectionOperation;
use crate::runtime::PromiseFinallyHandlerKind;

pub(super) fn begin_promise_constructor(
    runtime: &mut Runtime,
    native: NativeFunction,
    mut inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = origin.unwrap_or_else(native_function_host_origin);
    let Some(new_target) = inputs.new_target else {
        return promise_type_error(native.realm, "Promise constructor requires 'new'", origin);
    };
    let executor = inputs.arguments.take_first_or_undefined();
    let StoredValue::Function(executor) = executor else {
        return promise_type_error(native.realm, "Promise executor is not a function", origin);
    };

    let receiver = StoredValue::Function(new_target);
    charge_heap_property_lookup(runtime, &receiver, execution_budget)?;
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let continuation = IntrinsicGetContinuation::PromiseConstructor {
        realm: native.realm,
        new_target,
        executor,
        origin: origin.clone(),
    };
    let dispatch = begin_internal_get(
        runtime,
        HeapReference::Function(new_target),
        receiver,
        prototype_key,
        native.realm,
        return_to,
        origin.clone(),
        execution_budget,
    )?;
    continue_intrinsic_get_after(runtime, dispatch, continuation, return_to, execution_budget)
}

pub(super) fn finish_promise_constructor_after_prototype_get(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: FunctionId,
    executor: FunctionId,
    origin: JsStackFrame,
    return_to: Option<CallReturn>,
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
            let target_realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_promise_prototype(target_realm)?)
        }
    };
    let promise = runtime.allocate_promise_with_prototype(prototype)?;
    let (resolve, reject) = runtime.allocate_promise_resolving_functions(promise, realm)?;
    let arguments = promise_call_arguments([
        StoredValue::Function(resolve),
        StoredValue::Function(reject),
    ])?;
    Ok(NativeDispatch::Call(NativeCall {
        function: executor,
        receiver: StoredValue::Undefined,
        arguments,
        return_to,
        origin,
        continuations: one_promise_continuation(PromiseContinuation::ConstructorExecutor {
            promise,
            reject,
        })?,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

pub(super) fn begin_promise_resolve(
    runtime: &mut Runtime,
    native: NativeFunction,
    mut inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = origin.unwrap_or_else(native_function_host_origin);
    let constructor =
        promise_constructor_receiver(runtime, native.realm, &inputs.receiver, &origin)?;
    let resolution = inputs.arguments.take_first_or_undefined();
    begin_promise_resolve_with_constructor(
        runtime,
        native.realm,
        constructor,
        resolution,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn begin_promise_static(
    runtime: &mut Runtime,
    native: NativeFunction,
    method: PromiseStatic,
    mut inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
    _execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = origin.unwrap_or_else(native_function_host_origin);
    let constructor =
        promise_constructor_receiver(runtime, native.realm, &inputs.receiver, &origin)?;
    match method {
        PromiseStatic::Try => {
            let callback = inputs.arguments.take_first_or_undefined();
            let arguments = inputs.arguments.into_remaining_values();
            begin_new_promise_capability(
                runtime,
                constructor,
                native.realm,
                PromiseCapabilityPurpose::Try {
                    callback,
                    arguments,
                },
                return_to,
                origin,
            )
        }
        PromiseStatic::WithResolvers => begin_new_promise_capability(
            runtime,
            constructor,
            native.realm,
            PromiseCapabilityPurpose::WithResolvers,
            return_to,
            origin,
        ),
        PromiseStatic::All
        | PromiseStatic::AllSettled
        | PromiseStatic::Any
        | PromiseStatic::Race => {
            let iterable = inputs.arguments.take_first_or_undefined();
            let kind = match method {
                PromiseStatic::All => PromiseCombinatorKind::All,
                PromiseStatic::AllSettled => PromiseCombinatorKind::AllSettled,
                PromiseStatic::Any => PromiseCombinatorKind::Any,
                PromiseStatic::Race => PromiseCombinatorKind::Race,
                PromiseStatic::Try | PromiseStatic::WithResolvers => {
                    unreachable!("non-combinator Promise static was matched above")
                }
            };
            begin_new_promise_capability(
                runtime,
                constructor,
                native.realm,
                PromiseCapabilityPurpose::Combinator {
                    constructor,
                    kind,
                    iterable,
                },
                return_to,
                origin,
            )
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "PromiseResolve carries its constructor, resolution value, caller completion, source Realm, and execution authority"
)]
pub(super) fn begin_promise_resolve_with_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    constructor: FunctionId,
    resolution: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(object) = resolution
        && runtime
            .objects
            .get(object)
            .is_some_and(HeapObject::is_promise)
    {
        let promise = StoredValue::Object(object);
        let constructor_key = runtime.predefined_property_key(PredefinedAtom::Constructor);
        return begin_promise_get(
            runtime,
            realm,
            &promise,
            constructor_key,
            Some("constructor"),
            PromiseContinuation::ResolveConstructorGet {
                realm,
                constructor,
                promise: object,
                origin: origin.clone(),
            },
            return_to,
            origin,
            execution_budget,
        );
    }
    begin_new_promise_capability(
        runtime,
        constructor,
        realm,
        PromiseCapabilityPurpose::Resolve { resolution },
        return_to,
        origin,
    )
}

fn finish_promise_resolve_after_constructor_get(
    runtime: &mut Runtime,
    realm: RealmId,
    constructor: FunctionId,
    promise: ObjectId,
    observed_constructor: &StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(observed_constructor, StoredValue::Function(value) if *value == constructor) {
        return Ok(NativeDispatch::Immediate(StoredValue::Object(promise)));
    }
    begin_new_promise_capability(
        runtime,
        constructor,
        realm,
        PromiseCapabilityPurpose::Resolve {
            resolution: StoredValue::Object(promise),
        },
        return_to,
        origin,
    )
}

fn begin_new_promise_capability(
    runtime: &mut Runtime,
    constructor: FunctionId,
    realm: RealmId,
    purpose: PromiseCapabilityPurpose,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    if !function_is_constructor(runtime, constructor)? {
        return promise_type_error(realm, "Promise capability requires a constructor", origin);
    }
    let executor_realm = runtime.function_realm(constructor)?;
    let (executor, capture) = runtime.allocate_promise_capability_executor(executor_realm)?;
    let arguments = promise_call_arguments([StoredValue::Function(executor)])?;
    Ok(NativeDispatch::Call(NativeCall {
        function: constructor,
        receiver: StoredValue::Undefined,
        arguments,
        return_to,
        origin: origin.clone(),
        continuations: one_promise_continuation(PromiseContinuation::NewCapabilityConstruct {
            capture,
            realm,
            origin,
            purpose,
        })?,
        pre_call: None,
        new_target: Some(constructor),
        native_caller: None,
    }))
}

#[allow(
    clippy::too_many_arguments,
    reason = "NewPromiseCapability completion carries its capture, purpose, result, Realm, caller completion, source origin, and execution authority"
)]
fn finish_new_promise_capability(
    runtime: &mut Runtime,
    capture: &Rc<RefCell<PromiseCapabilityCapture>>,
    realm: RealmId,
    origin: JsStackFrame,
    purpose: PromiseCapabilityPurpose,
    promise: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if !matches!(promise, StoredValue::Function(_) | StoredValue::Object(_)) {
        return promise_type_error(
            realm,
            "Promise capability constructor returned a primitive",
            origin,
        );
    }
    let capture = capture
        .try_borrow()
        .map_err(|_| EngineFault::RuntimeInvariant {
            message: "Promise capability capture is mutably borrowed after construction",
        })?;
    let Some(StoredValue::Function(resolve)) = capture.resolve.as_ref() else {
        return promise_type_error(realm, "Promise capability resolve is not callable", origin);
    };
    let Some(StoredValue::Function(reject)) = capture.reject.as_ref() else {
        return promise_type_error(realm, "Promise capability reject is not callable", origin);
    };
    let capability = PromiseCapability {
        promise,
        resolve: *resolve,
        reject: *reject,
    };
    drop(capture);
    match purpose {
        PromiseCapabilityPurpose::Resolve { resolution } => {
            call_capability_settlement(capability, true, resolution, return_to, origin)
        }
        PromiseCapabilityPurpose::Reject { reason } => {
            call_capability_settlement(capability, false, reason, return_to, origin)
        }
        PromiseCapabilityPurpose::Then {
            promise,
            on_fulfilled,
            on_rejected,
        } => {
            let result = capability.promise.duplicate();
            perform_promise_then(runtime, promise, on_fulfilled, on_rejected, capability)?;
            Ok(NativeDispatch::Immediate(result))
        }
        PromiseCapabilityPurpose::Try {
            callback,
            arguments,
        } => begin_promise_try_callback(
            runtime, realm, capability, &callback, arguments, return_to, origin,
        ),
        PromiseCapabilityPurpose::Combinator {
            constructor,
            kind,
            iterable,
        } => begin_promise_combinator(
            runtime,
            realm,
            constructor,
            kind,
            iterable,
            capability,
            return_to,
            origin,
            execution_budget,
        ),
        PromiseCapabilityPurpose::WithResolvers => Ok(NativeDispatch::Immediate(
            StoredValue::Object(runtime.allocate_promise_capability_record(realm, capability)?),
        )),
    }
}

fn begin_promise_try_callback(
    runtime: &mut Runtime,
    realm: RealmId,
    capability: PromiseCapability,
    callback: &StoredValue,
    arguments: Vec<StoredValue>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(callback) = callback else {
        let reason = StoredValue::Object(runtime.materialize_error_object(
            realm,
            ExceptionKind::TypeError,
            JsString::from_utf8("Promise.try callback is not callable")?,
            None,
        )?);
        return call_capability_settlement(capability, false, reason, return_to, origin);
    };
    Ok(NativeDispatch::Call(NativeCall {
        function: *callback,
        receiver: StoredValue::Undefined,
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin: origin.clone(),
        continuations: one_promise_continuation(PromiseContinuation::TryCallback {
            capability,
            origin,
        })?,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

pub(super) fn call_capability_settlement(
    capability: PromiseCapability,
    resolve: bool,
    argument: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let function = if resolve {
        capability.resolve
    } else {
        capability.reject
    };
    let arguments = promise_call_arguments([argument])?;
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver: StoredValue::Undefined,
        arguments,
        return_to,
        origin,
        continuations: one_promise_continuation(PromiseContinuation::CapabilitySettlement {
            promise: capability.promise,
        })?,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

pub(super) fn call_promise_capability_direct(
    capability: &PromiseCapability,
    resolve: bool,
    argument: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let function = if resolve {
        capability.resolve
    } else {
        capability.reject
    };
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver: StoredValue::Undefined,
        arguments: promise_call_arguments([argument])?,
        return_to,
        origin,
        continuations: Vec::new(),
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

fn call_capability_job_settlement(
    capability: &PromiseCapability,
    resolve: bool,
    argument: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let function = if resolve {
        capability.resolve
    } else {
        capability.reject
    };
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver: StoredValue::Undefined,
        arguments: promise_call_arguments([argument])?,
        return_to: None,
        origin: native_function_host_origin(),
        continuations: Vec::new(),
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

pub(super) fn begin_promise_reject(
    runtime: &mut Runtime,
    native: NativeFunction,
    mut inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = origin.unwrap_or_else(native_function_host_origin);
    let constructor =
        promise_constructor_receiver(runtime, native.realm, &inputs.receiver, &origin)?;
    let reason = inputs.arguments.take_first_or_undefined();
    begin_new_promise_capability(
        runtime,
        constructor,
        native.realm,
        PromiseCapabilityPurpose::Reject { reason },
        return_to,
        origin,
    )
}

pub(super) fn begin_promise_then(
    runtime: &mut Runtime,
    native: NativeFunction,
    mut inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = origin.unwrap_or_else(native_function_host_origin);
    let StoredValue::Object(promise) = inputs.receiver else {
        return promise_type_error(
            native.realm,
            "Promise.prototype.then called on an incompatible receiver",
            origin,
        );
    };
    if !runtime
        .objects
        .get(promise)
        .is_some_and(HeapObject::is_promise)
    {
        return promise_type_error(
            native.realm,
            "Promise.prototype.then called on an incompatible receiver",
            origin,
        );
    }
    let on_fulfilled = callable_handler(&inputs.arguments.take_first_or_undefined());
    let on_rejected = callable_handler(&inputs.arguments.take_first_or_undefined());
    let state = PromiseThenState {
        promise,
        realm: native.realm,
        on_fulfilled,
        on_rejected,
        origin: origin.clone(),
    };
    let constructor_key = runtime.predefined_property_key(PredefinedAtom::Constructor);
    begin_promise_get(
        runtime,
        native.realm,
        &StoredValue::Object(promise),
        constructor_key,
        Some("constructor"),
        PromiseContinuation::ThenConstructorGet(state),
        return_to,
        origin,
        execution_budget,
    )
}

fn finish_promise_then_constructor_get(
    runtime: &mut Runtime,
    state: PromiseThenState,
    constructor: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(constructor, StoredValue::Undefined) {
        return begin_promise_then_capability(runtime, state, None, return_to);
    }
    if !matches!(
        constructor,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        return promise_type_error(
            state.realm,
            "Promise constructor property is not an object",
            state.origin,
        );
    }
    let species_key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolSpecies);
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_promise_get(
        runtime,
        realm,
        constructor,
        species_key,
        Some("Symbol.species"),
        PromiseContinuation::ThenSpeciesGet(state),
        return_to,
        origin,
        execution_budget,
    )
}

fn finish_promise_then_species_get(
    runtime: &mut Runtime,
    state: PromiseThenState,
    species: &StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(species, StoredValue::Undefined | StoredValue::Null) {
        return begin_promise_then_capability(runtime, state, None, return_to);
    }
    let StoredValue::Function(constructor) = species else {
        return promise_type_error(
            state.realm,
            "Promise species is not a constructor",
            state.origin,
        );
    };
    if !function_is_constructor(runtime, *constructor)? {
        return promise_type_error(
            state.realm,
            "Promise species is not a constructor",
            state.origin,
        );
    }
    begin_promise_then_capability(runtime, state, Some(*constructor), return_to)
}

fn begin_promise_then_capability(
    runtime: &mut Runtime,
    state: PromiseThenState,
    constructor: Option<FunctionId>,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let constructor = constructor.unwrap_or(runtime.realm_promise_constructor(state.realm)?);
    begin_new_promise_capability(
        runtime,
        constructor,
        state.realm,
        PromiseCapabilityPurpose::Then {
            promise: state.promise,
            on_fulfilled: state.on_fulfilled,
            on_rejected: state.on_rejected,
        },
        return_to,
        state.origin,
    )
}

pub(super) fn begin_promise_catch(
    runtime: &mut Runtime,
    native: NativeFunction,
    mut inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = origin.unwrap_or_else(native_function_host_origin);
    let receiver = inputs.receiver;
    let on_rejected = inputs.arguments.take_first_or_undefined();
    let then_key = runtime.predefined_property_key(PredefinedAtom::Then);
    begin_promise_get(
        runtime,
        native.realm,
        &receiver,
        then_key,
        Some("then"),
        PromiseContinuation::CatchThenGet {
            realm: native.realm,
            receiver: receiver.duplicate(),
            on_rejected,
            origin: origin.clone(),
        },
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn begin_promise_finally(
    runtime: &mut Runtime,
    native: NativeFunction,
    mut inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = origin.unwrap_or_else(native_function_host_origin);
    let receiver = inputs.receiver;
    if receiver.heap_reference().is_none() {
        return promise_type_error(
            native.realm,
            "Promise.prototype.finally called on a non-object",
            origin,
        );
    }
    let state = PromiseFinallyState {
        receiver,
        realm: native.realm,
        on_finally: inputs.arguments.take_first_or_undefined(),
        origin: origin.clone(),
    };
    let constructor_key = runtime.predefined_property_key(PredefinedAtom::Constructor);
    let realm = state.realm;
    let receiver = state.receiver.duplicate();
    begin_promise_get(
        runtime,
        realm,
        &receiver,
        constructor_key,
        Some("constructor"),
        PromiseContinuation::FinallyConstructorGet(state),
        return_to,
        origin,
        execution_budget,
    )
}

fn finish_promise_finally_constructor_get(
    runtime: &mut Runtime,
    state: PromiseFinallyState,
    constructor: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(constructor, StoredValue::Undefined) {
        return finish_promise_finally_species_get(
            runtime,
            state,
            None,
            return_to,
            execution_budget,
        );
    }
    if !matches!(
        constructor,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        return promise_type_error(
            state.realm,
            "Promise finally constructor property is not an object",
            state.origin,
        );
    }
    let species_key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolSpecies);
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_promise_get(
        runtime,
        realm,
        constructor,
        species_key,
        Some("Symbol.species"),
        PromiseContinuation::FinallySpeciesGet(state),
        return_to,
        origin,
        execution_budget,
    )
}

fn finish_promise_finally_species_get(
    runtime: &mut Runtime,
    state: PromiseFinallyState,
    species: Option<&StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let constructor = match species {
        None | Some(StoredValue::Undefined | StoredValue::Null) => {
            runtime.realm_promise_constructor(state.realm)?
        }
        Some(StoredValue::Function(constructor))
            if function_is_constructor(runtime, *constructor)? =>
        {
            *constructor
        }
        Some(_) => {
            return promise_type_error(
                state.realm,
                "Promise finally species is not a constructor",
                state.origin,
            );
        }
    };
    let (then_finally, catch_finally) = match state.on_finally {
        StoredValue::Function(on_finally) => {
            let (then_finally, catch_finally) =
                runtime.allocate_promise_finally_handlers(state.realm, on_finally, constructor)?;
            (
                StoredValue::Function(then_finally),
                StoredValue::Function(catch_finally),
            )
        }
        on_finally => (on_finally.duplicate(), on_finally),
    };
    begin_promise_finally_then_get(
        runtime,
        PromiseFinallyThenState {
            receiver: state.receiver,
            then_finally,
            catch_finally,
            realm: state.realm,
            origin: state.origin,
        },
        return_to,
        execution_budget,
    )
}

fn begin_promise_finally_then_get(
    runtime: &mut Runtime,
    state: PromiseFinallyThenState,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let then_key = runtime.predefined_property_key(PredefinedAtom::Then);
    let realm = state.realm;
    let origin = state.origin.clone();
    let receiver = state.receiver.duplicate();
    begin_promise_get(
        runtime,
        realm,
        &receiver,
        then_key,
        Some("then"),
        PromiseContinuation::FinallyThenGet(state),
        return_to,
        origin,
        execution_budget,
    )
}

fn finish_promise_finally_then_get(
    state: PromiseFinallyThenState,
    then: &StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(then) = then else {
        return promise_type_error(
            state.realm,
            "Promise.prototype.finally receiver has no callable then",
            state.origin,
        );
    };
    Ok(NativeDispatch::Call(NativeCall {
        function: *then,
        receiver: state.receiver,
        arguments: promise_call_arguments([state.then_finally, state.catch_finally])?,
        return_to,
        origin: state.origin,
        continuations: Vec::new(),
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

pub(super) fn dispatch_promise_resolving(
    runtime: &mut Runtime,
    resolving: &PromiseResolvingFunction,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let resolution = arguments.take_first_or_undefined();
    if resolving.already_resolved.replace(true) {
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    }
    match resolving.kind {
        PromiseResolvingKind::Reject => {
            reject_promise(runtime, resolving.promise, resolution)?;
            Ok(NativeDispatch::Immediate(StoredValue::Undefined))
        }
        PromiseResolvingKind::Resolve => begin_promise_resolution(
            runtime,
            resolving.promise,
            resolving.realm,
            resolution,
            StoredValue::Undefined,
            return_to,
            origin,
            execution_budget,
        ),
    }
}

pub(super) fn dispatch_promise_capability_executor(
    executor: &PromiseCapabilityExecutor,
    mut arguments: CallArguments,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let mut capture =
        executor
            .capture
            .try_borrow_mut()
            .map_err(|_| EngineFault::RuntimeInvariant {
                message: "Promise capability executor capture is already borrowed",
            })?;
    if capture
        .resolve
        .as_ref()
        .is_some_and(|value| !matches!(value, StoredValue::Undefined))
        || capture
            .reject
            .as_ref()
            .is_some_and(|value| !matches!(value, StoredValue::Undefined))
    {
        return promise_type_error(
            executor.realm,
            "Promise capability executor was called more than once",
            origin,
        );
    }
    capture.resolve = Some(arguments.take_first_or_undefined());
    capture.reject = Some(arguments.take_first_or_undefined());
    Ok(NativeDispatch::Immediate(StoredValue::Undefined))
}

pub(super) fn dispatch_promise_finally_function(
    function: &PromiseFinallyFunction,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    match function {
        PromiseFinallyFunction::Handler {
            realm,
            on_finally,
            constructor,
            kind,
        } => {
            let completion = arguments.take_first_or_undefined();
            let kind = match kind {
                PromiseFinallyHandlerKind::Then => PromiseFinallyThunkKind::Return,
                PromiseFinallyHandlerKind::Catch => PromiseFinallyThunkKind::Throw,
            };
            Ok(NativeDispatch::Call(NativeCall {
                function: *on_finally,
                receiver: StoredValue::Undefined,
                arguments: CallArguments::empty(),
                return_to,
                origin: origin.clone(),
                continuations: one_promise_continuation(PromiseContinuation::FinallyCallback {
                    realm: *realm,
                    constructor: *constructor,
                    completion,
                    kind,
                    origin,
                })?,
                pre_call: None,
                new_target: None,
                native_caller: None,
            }))
        }
        PromiseFinallyFunction::Thunk {
            realm: _,
            completion,
            kind: PromiseFinallyThunkKind::Return,
        } => Ok(NativeDispatch::Immediate(completion.duplicate())),
        PromiseFinallyFunction::Thunk {
            realm,
            completion,
            kind: PromiseFinallyThunkKind::Throw,
        } => Err(NativeFailure::Abrupt(PendingException {
            realm: *realm,
            payload: PendingExceptionPayload::ThrownValue(completion.duplicate()),
            origin,
        })),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the finally callback continuation carries its captured completion, constructor, Realm, source origin, and execution authority"
)]
fn finish_promise_finally_callback(
    runtime: &mut Runtime,
    realm: RealmId,
    constructor: FunctionId,
    completion: StoredValue,
    kind: PromiseFinallyThunkKind,
    origin: JsStackFrame,
    result: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let dispatch = begin_promise_resolve_with_constructor(
        runtime,
        realm,
        constructor,
        result,
        return_to,
        origin.clone(),
        execution_budget,
    )?;
    match dispatch {
        NativeDispatch::Immediate(promise) => finish_promise_finally_resolved(
            runtime,
            realm,
            completion,
            kind,
            origin,
            promise,
            return_to,
            execution_budget,
        ),
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                one_promise_continuation(PromiseContinuation::FinallyResolved {
                    realm,
                    completion,
                    kind,
                    origin,
                })?,
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                one_promise_continuation(PromiseContinuation::FinallyResolved {
                    realm,
                    completion,
                    kind,
                    origin,
                })?,
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "PromiseResolve produced a structured finally result",
        }
        .into()),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "the finally PromiseResolve continuation owns the resolved promise beside the captured completion and execution authority"
)]
fn finish_promise_finally_resolved(
    runtime: &mut Runtime,
    realm: RealmId,
    completion: StoredValue,
    kind: PromiseFinallyThunkKind,
    origin: JsStackFrame,
    promise: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if promise.heap_reference().is_none() {
        return Err(EngineFault::RuntimeInvariant {
            message: "PromiseResolve returned a primitive to Promise.prototype.finally",
        }
        .into());
    }
    let thunk = runtime.allocate_promise_finally_thunk(realm, completion, kind)?;
    let then_key = runtime.predefined_property_key(PredefinedAtom::Then);
    begin_promise_get(
        runtime,
        realm,
        &promise,
        then_key,
        Some("then"),
        PromiseContinuation::FinallyResolvedThenGet {
            realm,
            promise: promise.duplicate(),
            thunk,
            origin: origin.clone(),
        },
        return_to,
        origin,
        execution_budget,
    )
}

fn finish_promise_finally_resolved_then_get(
    realm: RealmId,
    promise: StoredValue,
    thunk: FunctionId,
    origin: JsStackFrame,
    then: &StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(then) = then else {
        return promise_type_error(realm, "PromiseResolve result has no callable then", origin);
    };
    Ok(NativeDispatch::Call(NativeCall {
        function: *then,
        receiver: promise,
        arguments: promise_call_arguments([StoredValue::Function(thunk)])?,
        return_to,
        origin,
        continuations: Vec::new(),
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

#[allow(
    clippy::too_many_arguments,
    reason = "Promise resolution carries the target, Realm, observable value, caller completion, and execution authority"
)]
fn begin_promise_resolution(
    runtime: &mut Runtime,
    promise: ObjectId,
    realm: RealmId,
    resolution: StoredValue,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(resolution, StoredValue::Object(object) if object == promise) {
        let error = runtime.materialize_error_object(
            realm,
            ExceptionKind::TypeError,
            JsString::from_utf8("cannot resolve a Promise with itself")?,
            None,
        )?;
        reject_promise(runtime, promise, StoredValue::Object(error))?;
        return Ok(NativeDispatch::Immediate(completion));
    }
    if !matches!(
        resolution,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        fulfill_promise(runtime, promise, resolution)?;
        return Ok(NativeDispatch::Immediate(completion));
    }

    let then_key = runtime.predefined_property_key(PredefinedAtom::Then);
    begin_promise_get(
        runtime,
        realm,
        &resolution,
        then_key,
        Some("then"),
        PromiseContinuation::ResolveThenGet {
            promise,
            realm,
            resolution: resolution.duplicate(),
            completion,
        },
        return_to,
        origin,
        execution_budget,
    )
}

fn finish_promise_resolution_after_then_get(
    runtime: &mut Runtime,
    promise: ObjectId,
    realm: RealmId,
    resolution: StoredValue,
    then: &StoredValue,
    completion: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Function(then) = then {
        enqueue_promise_job(
            runtime,
            PromiseJob::Thenable {
                promise,
                realm,
                thenable: resolution,
                then: *then,
            },
        )?;
    } else {
        fulfill_promise(runtime, promise, resolution)?;
    }
    Ok(NativeDispatch::Immediate(completion))
}

enum PromiseThenDisposition {
    Pending,
    Fulfilled(StoredValue),
    Rejected {
        reason: StoredValue,
        is_handled: bool,
    },
}

#[allow(
    clippy::too_many_lines,
    reason = "the three Promise-state branches keep HostPromiseRejectionTracker ordering visible"
)]
pub(super) fn perform_promise_then(
    runtime: &mut Runtime,
    promise: ObjectId,
    on_fulfilled: Option<FunctionId>,
    on_rejected: Option<FunctionId>,
    capability: PromiseCapability,
) -> Result<(), NativeFailure> {
    let fulfill_reaction = PromiseReaction {
        kind: PromiseReactionKind::Fulfill,
        target: PromiseReactionTarget::Then {
            handler: on_fulfilled,
            capability: capability.clone(),
        },
    };
    let reject_reaction = PromiseReaction {
        kind: PromiseReactionKind::Reject,
        target: PromiseReactionTarget::Then {
            handler: on_rejected,
            capability,
        },
    };
    perform_promise_reactions(runtime, promise, fulfill_reaction, reject_reaction)
}

pub(super) fn perform_async_function_await(
    runtime: &mut Runtime,
    promise: ObjectId,
    activation: ObjectId,
) -> Result<(), NativeFailure> {
    let fulfill_reaction = PromiseReaction {
        kind: PromiseReactionKind::Fulfill,
        target: PromiseReactionTarget::AsyncFunction { activation },
    };
    let reject_reaction = PromiseReaction {
        kind: PromiseReactionKind::Reject,
        target: PromiseReactionTarget::AsyncFunction { activation },
    };
    perform_promise_reactions(runtime, promise, fulfill_reaction, reject_reaction)
}

pub(super) fn perform_async_generator_await(
    runtime: &mut Runtime,
    promise: ObjectId,
    generator: ObjectId,
) -> Result<(), NativeFailure> {
    let fulfill_reaction = PromiseReaction {
        kind: PromiseReactionKind::Fulfill,
        target: PromiseReactionTarget::AsyncGenerator { generator },
    };
    let reject_reaction = PromiseReaction {
        kind: PromiseReactionKind::Reject,
        target: PromiseReactionTarget::AsyncGenerator { generator },
    };
    perform_promise_reactions(runtime, promise, fulfill_reaction, reject_reaction)
}

pub(super) fn perform_array_from_async_await(
    runtime: &mut Runtime,
    promise: ObjectId,
    operation: ObjectId,
) -> Result<(), NativeFailure> {
    let fulfill_reaction = PromiseReaction {
        kind: PromiseReactionKind::Fulfill,
        target: PromiseReactionTarget::ArrayFromAsync { operation },
    };
    let reject_reaction = PromiseReaction {
        kind: PromiseReactionKind::Reject,
        target: PromiseReactionTarget::ArrayFromAsync { operation },
    };
    perform_promise_reactions(runtime, promise, fulfill_reaction, reject_reaction)
}

/// Attaches fulfill and reject reactions sharing one runtime-owned target
/// (async module continuations, deferred dynamic-import settlement).
pub(crate) fn perform_targeted_promise_reactions(
    runtime: &mut Runtime,
    promise: ObjectId,
    target: PromiseReactionTarget,
) -> Result<(), NativeFailure> {
    let fulfill_reaction = PromiseReaction {
        kind: PromiseReactionKind::Fulfill,
        target: target.clone(),
    };
    let reject_reaction = PromiseReaction {
        kind: PromiseReactionKind::Reject,
        target,
    };
    perform_promise_reactions(runtime, promise, fulfill_reaction, reject_reaction)
}

/// [`perform_targeted_promise_reactions`] for callers outside the VM, where
/// the private [`NativeFailure`] type is not visible; an abrupt registration
/// would indicate an internal invariant failure (see [`fulfill_promise_host`]).
pub(crate) fn perform_targeted_promise_reactions_host(
    runtime: &mut Runtime,
    promise: ObjectId,
    target: PromiseReactionTarget,
) -> Result<(), ExecutionError> {
    perform_targeted_promise_reactions(runtime, promise, target).map_err(|failure| match failure {
        NativeFailure::Execution(error) => error,
        NativeFailure::Abrupt(_) | NativeFailure::AbruptAfterTransient(_) => {
            EngineFault::RuntimeInvariant {
                message: "runtime-owned promise reaction registration completed abruptly",
            }
            .into()
        }
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "all Promise reaction targets share one state transition and rejection-tracker order"
)]
fn perform_promise_reactions(
    runtime: &mut Runtime,
    promise: ObjectId,
    fulfill_reaction: PromiseReaction,
    reject_reaction: PromiseReaction,
) -> Result<(), NativeFailure> {
    let disposition = match runtime
        .objects
        .get(promise)
        .and_then(HeapObject::promise_state)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "Promise",
            index: promise.index(),
            generation: promise.generation(),
        })? {
        PromiseState::Pending { .. } => PromiseThenDisposition::Pending,
        PromiseState::Fulfilled(value) => PromiseThenDisposition::Fulfilled(value.duplicate()),
        PromiseState::Rejected { reason, is_handled } => PromiseThenDisposition::Rejected {
            reason: reason.duplicate(),
            is_handled: *is_handled,
        },
    };
    match disposition {
        PromiseThenDisposition::Pending => {
            let state = runtime
                .objects
                .get_mut(promise)
                .and_then(HeapObject::promise_state_mut)
                .ok_or(EngineFault::StaleHeapEdge {
                    edge: "Promise",
                    index: promise.index(),
                    generation: promise.generation(),
                })?;
            let PromiseState::Pending {
                fulfill_reactions,
                reject_reactions,
                is_handled,
            } = state
            else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Promise state changed without runtime re-entry",
                }
                .into());
            };
            fulfill_reactions
                .try_reserve(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::PromiseJobs,
                    additional: 1,
                })?;
            reject_reactions
                .try_reserve(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::PromiseJobs,
                    additional: 1,
                })?;
            fulfill_reactions.push(fulfill_reaction);
            reject_reactions.push(reject_reaction);
            *is_handled = true;
        }
        PromiseThenDisposition::Fulfilled(argument) => {
            enqueue_promise_job(
                runtime,
                PromiseJob::Reaction {
                    reaction: fulfill_reaction,
                    argument,
                },
            )?;
        }
        PromiseThenDisposition::Rejected { reason, is_handled } => {
            reserve_promise_jobs(runtime, 1)?;
            if !is_handled {
                runtime.dispatch_promise_rejection(
                    PromiseRejectionOperation::Handle,
                    promise,
                    reason.duplicate(),
                );
            }
            runtime.promise_jobs.push_back(PromiseJob::Reaction {
                reaction: reject_reaction,
                argument: reason,
            });
            let state = runtime
                .objects
                .get_mut(promise)
                .and_then(HeapObject::promise_state_mut)
                .ok_or(EngineFault::StaleHeapEdge {
                    edge: "Promise",
                    index: promise.index(),
                    generation: promise.generation(),
                })?;
            let PromiseState::Rejected { is_handled, .. } = state else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "rejected Promise state changed without runtime re-entry",
                }
                .into());
            };
            *is_handled = true;
        }
    }
    runtime.collection_pending = true;
    Ok(())
}

pub(super) fn fulfill_promise(
    runtime: &mut Runtime,
    promise: ObjectId,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    settle_promise(runtime, promise, value, PromiseReactionKind::Fulfill)
}

/// Settles an intrinsic promise from a runtime-owned host job. The supplied
/// resolution is always a primitive, so an abrupt JavaScript completion would
/// indicate an internal invariant failure rather than user-observable
/// thenable assimilation.
pub(crate) fn fulfill_promise_host(
    runtime: &mut Runtime,
    promise: ObjectId,
    value: StoredValue,
) -> Result<(), ExecutionError> {
    fulfill_promise(runtime, promise, value).map_err(|failure| match failure {
        NativeFailure::Execution(error) => error,
        NativeFailure::Abrupt(_) | NativeFailure::AbruptAfterTransient(_) => {
            EngineFault::RuntimeInvariant {
                message: "primitive Atomics waiter settlement completed abruptly",
            }
            .into()
        }
    })
}

pub(super) fn reject_promise(
    runtime: &mut Runtime,
    promise: ObjectId,
    reason: StoredValue,
) -> Result<(), NativeFailure> {
    settle_promise(runtime, promise, reason, PromiseReactionKind::Reject)
}

/// Rejects an intrinsic promise from a runtime-owned host job; see
/// [`fulfill_promise_host`].
pub(crate) fn reject_promise_host(
    runtime: &mut Runtime,
    promise: ObjectId,
    reason: StoredValue,
) -> Result<(), ExecutionError> {
    reject_promise(runtime, promise, reason).map_err(|failure| match failure {
        NativeFailure::Execution(error) => error,
        NativeFailure::Abrupt(_) | NativeFailure::AbruptAfterTransient(_) => {
            EngineFault::RuntimeInvariant {
                message: "intrinsic promise rejection completed abruptly",
            }
            .into()
        }
    })
}

fn settle_promise(
    runtime: &mut Runtime,
    promise: ObjectId,
    value: StoredValue,
    kind: PromiseReactionKind,
) -> Result<(), NativeFailure> {
    let (reaction_count, notify_rejection) = {
        let state = runtime
            .objects
            .get(promise)
            .and_then(HeapObject::promise_state)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "Promise",
                index: promise.index(),
                generation: promise.generation(),
            })?;
        match (kind, state) {
            (
                PromiseReactionKind::Fulfill,
                PromiseState::Pending {
                    fulfill_reactions, ..
                },
            ) => (fulfill_reactions.len(), false),
            (
                PromiseReactionKind::Reject,
                PromiseState::Pending {
                    reject_reactions,
                    is_handled,
                    ..
                },
            ) => (reject_reactions.len(), !is_handled),
            (_, PromiseState::Fulfilled(_) | PromiseState::Rejected { .. }) => return Ok(()),
        }
    };
    reserve_promise_jobs(runtime, reaction_count)?;
    let state = runtime
        .objects
        .get_mut(promise)
        .and_then(HeapObject::promise_state_mut)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "Promise",
            index: promise.index(),
            generation: promise.generation(),
        })?;
    let replacement = match kind {
        PromiseReactionKind::Fulfill => PromiseState::Fulfilled(value),
        PromiseReactionKind::Reject => PromiseState::Rejected {
            reason: value,
            is_handled: false,
        },
    };
    let previous = std::mem::replace(state, replacement);
    let (reactions, argument) = match previous {
        PromiseState::Pending {
            fulfill_reactions,
            reject_reactions,
            is_handled,
        } => {
            if kind == PromiseReactionKind::Reject
                && let PromiseState::Rejected {
                    is_handled: settled_handled,
                    ..
                } = state
            {
                *settled_handled = is_handled;
            }
            let reactions = match kind {
                PromiseReactionKind::Fulfill => fulfill_reactions,
                PromiseReactionKind::Reject => reject_reactions,
            };
            let argument = match state {
                PromiseState::Fulfilled(value) => value.duplicate(),
                PromiseState::Rejected { reason, .. } => reason.duplicate(),
                PromiseState::Pending { .. } => unreachable!("settlement installed a final state"),
            };
            (reactions, argument)
        }
        settled @ (PromiseState::Fulfilled(_) | PromiseState::Rejected { .. }) => {
            *state = settled;
            return Ok(());
        }
    };
    if notify_rejection {
        runtime.dispatch_promise_rejection(
            PromiseRejectionOperation::Reject,
            promise,
            argument.duplicate(),
        );
    }
    for reaction in reactions {
        runtime.promise_jobs.push_back(PromiseJob::Reaction {
            reaction,
            argument: argument.duplicate(),
        });
    }
    runtime.collection_pending = true;
    Ok(())
}

fn enqueue_promise_job(runtime: &mut Runtime, job: PromiseJob) -> Result<(), NativeFailure> {
    reserve_promise_jobs(runtime, 1)?;
    runtime.promise_jobs.push_back(job);
    runtime.collection_pending = true;
    Ok(())
}

fn reserve_promise_jobs(runtime: &mut Runtime, additional: usize) -> Result<(), NativeFailure> {
    check_execution_limit(
        RuntimeResource::PromiseJobs,
        runtime.limits.max_pending_promise_jobs,
        usize_to_u64(runtime.promise_jobs.len()).saturating_add(usize_to_u64(additional)),
    )?;
    runtime
        .promise_jobs
        .try_reserve(additional)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::PromiseJobs,
            additional,
        })?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive dispatcher keeps every typed Promise continuation visibly paired with its resume algorithm"
)]
pub(super) fn advance_promise_continuation(
    runtime: &mut Runtime,
    state: PromiseContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state {
        PromiseContinuation::AsyncFunctionSettlement { capability } => call_capability_settlement(
            capability,
            true,
            value,
            return_to,
            native_function_host_origin(),
        ),
        PromiseContinuation::ConstructorExecutor { promise, .. } => {
            Ok(NativeDispatch::Immediate(StoredValue::Object(promise)))
        }
        PromiseContinuation::ResolveThenGet {
            promise,
            realm,
            resolution,
            completion,
        } => finish_promise_resolution_after_then_get(
            runtime, promise, realm, resolution, &value, completion,
        ),
        PromiseContinuation::ResolveConstructorGet {
            realm,
            constructor,
            promise,
            origin,
        } => finish_promise_resolve_after_constructor_get(
            runtime,
            realm,
            constructor,
            promise,
            &value,
            return_to,
            origin,
        ),
        PromiseContinuation::NewCapabilityConstruct {
            capture,
            realm,
            origin,
            purpose,
        } => finish_new_promise_capability(
            runtime,
            &capture,
            realm,
            origin,
            purpose,
            value,
            return_to,
            execution_budget,
        ),
        PromiseContinuation::CapabilitySettlement { promise } => {
            Ok(NativeDispatch::Immediate(promise))
        }
        PromiseContinuation::ThenConstructorGet(state) => {
            finish_promise_then_constructor_get(runtime, state, &value, return_to, execution_budget)
        }
        PromiseContinuation::ThenSpeciesGet(state) => {
            finish_promise_then_species_get(runtime, state, &value, return_to)
        }
        PromiseContinuation::FinallyConstructorGet(state) => {
            finish_promise_finally_constructor_get(
                runtime,
                state,
                &value,
                return_to,
                execution_budget,
            )
        }
        PromiseContinuation::FinallySpeciesGet(state) => finish_promise_finally_species_get(
            runtime,
            state,
            Some(&value),
            return_to,
            execution_budget,
        ),
        PromiseContinuation::FinallyThenGet(state) => {
            finish_promise_finally_then_get(state, &value, return_to)
        }
        PromiseContinuation::FinallyCallback {
            realm,
            constructor,
            completion,
            kind,
            origin,
        } => finish_promise_finally_callback(
            runtime,
            realm,
            constructor,
            completion,
            kind,
            origin,
            value,
            return_to,
            execution_budget,
        ),
        PromiseContinuation::FinallyResolved {
            realm,
            completion,
            kind,
            origin,
        } => finish_promise_finally_resolved(
            runtime,
            realm,
            completion,
            kind,
            origin,
            value,
            return_to,
            execution_budget,
        ),
        PromiseContinuation::FinallyResolvedThenGet {
            realm,
            promise,
            thunk,
            origin,
        } => finish_promise_finally_resolved_then_get(
            realm, promise, thunk, origin, &value, return_to,
        ),
        PromiseContinuation::CatchThenGet {
            realm,
            receiver,
            on_rejected,
            origin,
        } => finish_promise_catch_get(realm, &value, receiver, on_rejected, return_to, origin),
        PromiseContinuation::ReactionHandler { capability } => {
            call_capability_job_settlement(&capability, true, value)
        }
        PromiseContinuation::ThenableCall { .. } => {
            Ok(NativeDispatch::Immediate(StoredValue::Undefined))
        }
        PromiseContinuation::TryCallback { capability, origin } => {
            call_capability_settlement(capability, true, value, return_to, origin)
        }
    }
}

pub(super) fn resume_promise_abrupt(
    runtime: &mut Runtime,
    state: PromiseContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    _execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(
        state,
        PromiseContinuation::CatchThenGet { .. }
            | PromiseContinuation::ResolveConstructorGet { .. }
            | PromiseContinuation::FinallyConstructorGet(_)
            | PromiseContinuation::FinallySpeciesGet(_)
            | PromiseContinuation::FinallyThenGet(_)
            | PromiseContinuation::FinallyCallback { .. }
            | PromiseContinuation::FinallyResolved { .. }
            | PromiseContinuation::FinallyResolvedThenGet { .. }
    ) {
        return Err(NativeFailure::Abrupt(pending));
    }
    let reason = pending_exception_value(runtime, pending)?;
    match state {
        PromiseContinuation::AsyncFunctionSettlement { capability } => call_capability_settlement(
            capability,
            false,
            reason,
            return_to,
            native_function_host_origin(),
        ),
        PromiseContinuation::ConstructorExecutor { promise, reject } => {
            reject_through_resolving_function(runtime, reject, reason)?;
            Ok(NativeDispatch::Immediate(StoredValue::Object(promise)))
        }
        PromiseContinuation::ResolveThenGet {
            promise,
            completion,
            ..
        } => {
            reject_promise(runtime, promise, reason)?;
            Ok(NativeDispatch::Immediate(completion))
        }
        PromiseContinuation::ResolveConstructorGet { .. } => {
            unreachable!("checked before materializing")
        }
        PromiseContinuation::NewCapabilityConstruct { .. }
        | PromiseContinuation::CapabilitySettlement { .. }
        | PromiseContinuation::ThenConstructorGet(_)
        | PromiseContinuation::ThenSpeciesGet(_)
        | PromiseContinuation::FinallyConstructorGet(_)
        | PromiseContinuation::FinallySpeciesGet(_)
        | PromiseContinuation::FinallyThenGet(_)
        | PromiseContinuation::FinallyCallback { .. }
        | PromiseContinuation::FinallyResolved { .. }
        | PromiseContinuation::FinallyResolvedThenGet { .. } => {
            unreachable!("non-handling Promise continuation reached abrupt recovery")
        }
        PromiseContinuation::ReactionHandler { capability } => {
            call_capability_job_settlement(&capability, false, reason)
        }
        PromiseContinuation::ThenableCall { reject, .. } => {
            reject_through_resolving_function(runtime, reject, reason)?;
            Ok(NativeDispatch::Immediate(StoredValue::Undefined))
        }
        PromiseContinuation::TryCallback { capability, origin } => {
            call_capability_settlement(capability, false, reason, return_to, origin)
        }
        PromiseContinuation::CatchThenGet { .. } => unreachable!("checked before materializing"),
    }
}

pub(super) fn pending_exception_value(
    runtime: &mut Runtime,
    pending: PendingException,
) -> Result<StoredValue, NativeFailure> {
    Ok(match pending.payload {
        PendingExceptionPayload::ThrownValue(value) => value,
        PendingExceptionPayload::EngineError { kind, message } => StoredValue::Object(
            runtime.materialize_error_object(pending.realm, kind, message, None)?,
        ),
        PendingExceptionPayload::FrozenEngineError {
            kind,
            message,
            stack,
        } => StoredValue::Object(runtime.materialize_error_object(
            pending.realm,
            kind,
            message,
            Some(stack),
        )?),
    })
}

fn reject_through_resolving_function(
    runtime: &mut Runtime,
    function: FunctionId,
    reason: StoredValue,
) -> Result<(), NativeFailure> {
    let resolving = runtime
        .functions
        .get(function)
        .and_then(HeapFunction::promise_resolving)
        .cloned()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Promise rejection continuation lost its resolving function",
        })?;
    if resolving.already_resolved.replace(true) {
        return Ok(());
    }
    reject_promise(runtime, resolving.promise, reason)
}

fn finish_promise_catch_get(
    realm: RealmId,
    then: &StoredValue,
    receiver: StoredValue,
    on_rejected: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(then) = then else {
        return promise_type_error(
            realm,
            "Promise.prototype.catch receiver has no callable then",
            origin,
        );
    };
    let arguments = promise_call_arguments([StoredValue::Undefined, on_rejected])?;
    Ok(NativeDispatch::Call(NativeCall {
        function: *then,
        receiver,
        arguments,
        return_to,
        origin,
        continuations: Vec::new(),
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

fn callable_handler(value: &StoredValue) -> Option<FunctionId> {
    match value {
        StoredValue::Function(function) => Some(*function),
        _ => None,
    }
}

fn promise_constructor_receiver(
    runtime: &Runtime,
    realm: RealmId,
    receiver: &StoredValue,
    origin: &JsStackFrame,
) -> Result<FunctionId, NativeFailure> {
    let StoredValue::Function(constructor) = receiver else {
        return promise_type_error(
            realm,
            "Promise static method receiver is not a constructor",
            origin.clone(),
        );
    };
    if !function_is_constructor(runtime, *constructor)? {
        return promise_type_error(
            realm,
            "Promise static method receiver is not a constructor",
            origin.clone(),
        );
    }
    Ok(*constructor)
}

fn promise_type_error<T>(
    realm: RealmId,
    message: &'static str,
    origin: JsStackFrame,
) -> Result<T, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}

fn promise_native_continuation(state: PromiseContinuation) -> NativeContinuation {
    NativeContinuation::Promise(state)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the shared Promise Get boundary carries its typed continuation, diagnostic, caller completion, and execution authority"
)]
fn begin_promise_get(
    runtime: &mut Runtime,
    realm: RealmId,
    base: &StoredValue,
    key: PropertyKey,
    diagnostic_name: Option<&str>,
    state: PromiseContinuation,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if base.heap_reference().is_some() {
        charge_heap_property_lookup(runtime, base, execution_budget)?;
    } else {
        execution_budget.charge_instructions(1)?;
    }
    let name = diagnostic_name.map(JsString::from_utf8).transpose()?;
    let dispatch = match begin_value_get(
        runtime,
        base,
        key,
        name.as_ref(),
        realm,
        return_to,
        origin,
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending)) if state.handles_abrupt() => {
            return resume_promise_abrupt(runtime, state, pending, return_to, execution_budget);
        }
        Err(failure) => return Err(failure),
    };
    continue_get_after(
        dispatch,
        state,
        promise_native_continuation,
        |state, value| {
            advance_promise_continuation(runtime, state, value, return_to, execution_budget)
        },
        "Promise Get produced a structured result",
    )
}

fn one_promise_continuation(
    continuation: PromiseContinuation,
) -> Result<Vec<NativeContinuation>, NativeFailure> {
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::Promise(continuation));
    Ok(continuations)
}

pub(super) fn promise_call_arguments<const N: usize>(
    values: [StoredValue; N],
) -> Result<CallArguments, NativeFailure> {
    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(N)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: N,
        })?;
    arguments.extend(values);
    Ok(CallArguments::from_values(arguments))
}

impl PromiseContinuation {
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive tracer keeps every Promise continuation's retained heap edges auditable"
    )]
    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::ConstructorExecutor { promise, reject }
            | Self::ThenableCall { promise, reject } => {
                mark(CollectionRoot::Heap(HeapReference::Object(*promise)));
                mark(CollectionRoot::Heap(HeapReference::Function(*reject)));
            }
            Self::ResolveThenGet {
                promise,
                realm: _,
                resolution,
                completion,
            } => {
                mark(CollectionRoot::Heap(HeapReference::Object(*promise)));
                trace_stored_value_root(resolution, mark);
                trace_stored_value_root(completion, mark);
            }
            Self::ResolveConstructorGet {
                constructor,
                promise,
                ..
            } => {
                mark(CollectionRoot::Heap(HeapReference::Function(*constructor)));
                mark(CollectionRoot::Heap(HeapReference::Object(*promise)));
            }
            Self::NewCapabilityConstruct {
                capture, purpose, ..
            } => {
                let capture = capture.borrow();
                for value in [capture.resolve.as_ref(), capture.reject.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    trace_stored_value_root(value, mark);
                }
                match purpose {
                    PromiseCapabilityPurpose::Resolve { resolution } => {
                        trace_stored_value_root(resolution, mark);
                    }
                    PromiseCapabilityPurpose::Reject { reason } => {
                        trace_stored_value_root(reason, mark);
                    }
                    PromiseCapabilityPurpose::Then {
                        promise,
                        on_fulfilled,
                        on_rejected,
                    } => {
                        mark(CollectionRoot::Heap(HeapReference::Object(*promise)));
                        for handler in [on_fulfilled, on_rejected].into_iter().flatten() {
                            mark(CollectionRoot::Heap(HeapReference::Function(*handler)));
                        }
                    }
                    PromiseCapabilityPurpose::Try {
                        callback,
                        arguments,
                    } => {
                        trace_stored_value_root(callback, mark);
                        for argument in arguments {
                            trace_stored_value_root(argument, mark);
                        }
                    }
                    PromiseCapabilityPurpose::Combinator {
                        constructor,
                        iterable,
                        ..
                    } => {
                        mark(CollectionRoot::Heap(HeapReference::Function(*constructor)));
                        trace_stored_value_root(iterable, mark);
                    }
                    PromiseCapabilityPurpose::WithResolvers => {}
                }
            }
            Self::CapabilitySettlement { promise } => {
                trace_stored_value_root(promise, mark);
            }
            Self::ThenConstructorGet(state) | Self::ThenSpeciesGet(state) => {
                mark(CollectionRoot::Heap(HeapReference::Object(state.promise)));
                for handler in [state.on_fulfilled, state.on_rejected]
                    .into_iter()
                    .flatten()
                {
                    mark(CollectionRoot::Heap(HeapReference::Function(handler)));
                }
            }
            Self::FinallyConstructorGet(state) | Self::FinallySpeciesGet(state) => {
                trace_stored_value_root(&state.receiver, mark);
                trace_stored_value_root(&state.on_finally, mark);
            }
            Self::FinallyThenGet(state) => {
                trace_stored_value_root(&state.receiver, mark);
                trace_stored_value_root(&state.then_finally, mark);
                trace_stored_value_root(&state.catch_finally, mark);
            }
            Self::FinallyCallback {
                constructor,
                completion,
                ..
            } => {
                mark(CollectionRoot::Heap(HeapReference::Function(*constructor)));
                trace_stored_value_root(completion, mark);
            }
            Self::FinallyResolved { completion, .. } => {
                trace_stored_value_root(completion, mark);
            }
            Self::FinallyResolvedThenGet { promise, thunk, .. } => {
                trace_stored_value_root(promise, mark);
                mark(CollectionRoot::Heap(HeapReference::Function(*thunk)));
            }
            Self::CatchThenGet {
                realm: _,
                receiver,
                on_rejected,
                ..
            } => {
                trace_stored_value_root(receiver, mark);
                trace_stored_value_root(on_rejected, mark);
            }
            Self::AsyncFunctionSettlement { capability }
            | Self::ReactionHandler { capability, .. }
            | Self::TryCallback { capability, .. } => {
                trace_promise_capability(capability, mark);
            }
        }
    }
}

pub(super) fn trace_promise_capability(
    capability: &PromiseCapability,
    mark: &mut dyn FnMut(CollectionRoot),
) {
    trace_stored_value_root(&capability.promise, mark);
    for function in [capability.resolve, capability.reject] {
        mark(CollectionRoot::Heap(HeapReference::Function(function)));
    }
}

pub(super) fn begin_promise_job(
    runtime: &mut Runtime,
    job: PromiseJob,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match job {
        PromiseJob::Reaction { reaction, argument } => match reaction.target {
            PromiseReactionTarget::Then {
                handler,
                capability,
            } => {
                if let Some(handler) = handler {
                    return Ok(NativeDispatch::Call(NativeCall {
                        function: handler,
                        receiver: StoredValue::Undefined,
                        arguments: promise_call_arguments([argument])?,
                        return_to: None,
                        origin: native_function_host_origin(),
                        continuations: one_promise_continuation(
                            PromiseContinuation::ReactionHandler { capability },
                        )?,
                        pre_call: None,
                        new_target: None,
                        native_caller: None,
                    }));
                }
                match reaction.kind {
                    PromiseReactionKind::Fulfill => {
                        call_capability_job_settlement(&capability, true, argument)
                    }
                    PromiseReactionKind::Reject => {
                        call_capability_job_settlement(&capability, false, argument)
                    }
                }
            }
            PromiseReactionTarget::AsyncFunction { activation } => {
                begin_async_function_resume(runtime, activation, reaction.kind, argument)
            }
            PromiseReactionTarget::AsyncGenerator { generator } => {
                begin_async_generator_await_resume(
                    runtime,
                    generator,
                    reaction.kind,
                    argument,
                    execution_budget,
                )
            }
            PromiseReactionTarget::ArrayFromAsync { operation } => begin_array_from_async_resume(
                runtime,
                operation,
                reaction.kind,
                argument,
                execution_budget,
            ),
            PromiseReactionTarget::AsyncModule { module } => {
                match reaction.kind {
                    PromiseReactionKind::Fulfill => {
                        crate::runtime::modules::async_module_execution_fulfilled(runtime, module)?;
                    }
                    PromiseReactionKind::Reject => {
                        crate::runtime::modules::async_module_execution_rejected(
                            runtime, module, argument,
                        )?;
                    }
                }
                Ok(NativeDispatch::Immediate(StoredValue::Undefined))
            }
            PromiseReactionTarget::FinishDynamicImport { promise, module } => {
                match reaction.kind {
                    PromiseReactionKind::Fulfill => {
                        let namespace = crate::runtime::modules::get_or_create_namespace(
                            runtime, module,
                        )
                        .map_err(|_| EngineFault::RuntimeInvariant {
                            message: "namespace creation failed after async module evaluation",
                        })?;
                        fulfill_promise(runtime, promise, StoredValue::Object(namespace))?;
                    }
                    PromiseReactionKind::Reject => {
                        reject_promise(runtime, promise, argument)?;
                    }
                }
                Ok(NativeDispatch::Immediate(StoredValue::Undefined))
            }
            PromiseReactionTarget::ImportDeferDeps { promise, module } => {
                // SafePerformPromiseAll element reaction: a rejection settles
                // the import immediately; fulfillments count down and the last
                // one fulfills with the module's deferred namespace.
                match reaction.kind {
                    PromiseReactionKind::Reject => {
                        runtime.deferred_import_waiters.remove(&promise);
                        reject_promise(runtime, promise, argument)?;
                    }
                    PromiseReactionKind::Fulfill => {
                        let remaining =
                            runtime
                                .deferred_import_waiters
                                .get_mut(&promise)
                                .and_then(|count| {
                                    *count = count.saturating_sub(1);
                                    (*count == 0).then_some(())
                                });
                        if remaining.is_some() {
                            runtime.deferred_import_waiters.remove(&promise);
                            let namespace = crate::runtime::modules::get_or_create_namespace_phase(
                                runtime, module, true,
                            )
                            .map_err(|_| {
                                EngineFault::RuntimeInvariant {
                                    message: "deferred namespace creation failed after async deps",
                                }
                            })?;
                            fulfill_promise(runtime, promise, StoredValue::Object(namespace))?;
                        }
                    }
                }
                Ok(NativeDispatch::Immediate(StoredValue::Undefined))
            }
        },
        PromiseJob::Thenable {
            promise,
            realm,
            thenable,
            then,
        } => {
            let (resolve, reject) = runtime.allocate_promise_resolving_functions(promise, realm)?;
            Ok(NativeDispatch::Call(NativeCall {
                function: then,
                receiver: thenable,
                arguments: promise_call_arguments([
                    StoredValue::Function(resolve),
                    StoredValue::Function(reject),
                ])?,
                return_to: None,
                origin: native_function_host_origin(),
                continuations: one_promise_continuation(PromiseContinuation::ThenableCall {
                    promise,
                    reject,
                })?,
                pre_call: None,
                new_target: None,
                native_caller: None,
            }))
        }
    }
}

fn drain_finalization_registry_job(
    runtime: &mut Runtime,
    registry: ObjectId,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), ExecutionError> {
    loop {
        let mut callback_arguments = Vec::new();
        callback_arguments
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::FrameValues,
                additional: 1,
            })?;
        let Some((_realm, callback, held_value)) =
            runtime.take_finalization_cleanup_value(registry)?
        else {
            break;
        };
        let prepared_frames = Vec::new();
        callback_arguments.push(held_value);
        let dispatch = resolve_native_dispatch(
            runtime,
            NativeDispatch::Call(NativeCall {
                function: callback,
                receiver: StoredValue::Undefined,
                arguments: CallArguments::from_values(callback_arguments),
                return_to: None,
                origin: native_function_host_origin(),
                continuations: Vec::new(),
                pre_call: None,
                new_target: None,
                native_caller: None,
            }),
            &prepared_frames,
            0,
            0,
            compiler,
            execution_budget,
        );
        match execute_root_dispatch_with_budget(
            runtime,
            dispatch,
            prepared_frames,
            compiler,
            execution_budget,
        ) {
            Ok(_) => {}
            Err(ExecutionError::Exception(_)) => break,
            Err(error) => return Err(error),
        }
        runtime.kept_alive.clear();
    }
    runtime.finish_finalization_cleanup_job(registry)?;
    Ok(())
}

pub(super) fn drain_promise_jobs(
    runtime: &mut Runtime,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), ExecutionError> {
    while let Some(job) = runtime.promise_jobs.pop_front() {
        let prepared_frames = Vec::new();
        let dispatch = begin_promise_job(runtime, job, execution_budget);
        let dispatch = match dispatch {
            Ok(dispatch) => resolve_native_dispatch(
                runtime,
                dispatch,
                &prepared_frames,
                0,
                0,
                compiler,
                execution_budget,
            ),
            Err(error) => Err(error),
        };
        let _ = execute_root_dispatch_with_budget(
            runtime,
            dispatch,
            prepared_frames,
            compiler,
            execution_budget,
        )?;
        runtime.kept_alive.clear();
    }
    Ok(())
}

/// Delivers only externally-ready `Atomics.waitAsync` completions before a
/// new host event. Each waiter settles and drains its Promise reactions before
/// the next waiter, preserving the runtime's established FIFO ordering.
/// Finalization cleanup remains an end-of-turn checkpoint and must not run
/// ahead of the host event.
pub(super) fn drain_ready_atomics_jobs_before_host_turn(
    runtime: &mut Runtime,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), ExecutionError> {
    while runtime.settle_next_ready_atomics_waiter()? {
        drain_promise_jobs(runtime, compiler, execution_budget)?;
    }
    Ok(())
}

pub(super) fn drain_host_jobs(
    runtime: &mut Runtime,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), ExecutionError> {
    loop {
        drain_promise_jobs(runtime, compiler, execution_budget)?;
        if let Some(registry) = runtime.finalization_jobs.pop_front() {
            drain_finalization_registry_job(runtime, registry, compiler, execution_budget)?;
            runtime.kept_alive.clear();
            continue;
        }
        if runtime.settle_next_ready_atomics_waiter()? {
            continue;
        }
        return Ok(());
    }
}

/// Drains queued host jobs (Promise reactions, finalization cleanup, ready
/// `Atomics.waitAsync` completions) to quiescence under a fresh budget.
///
/// This is the host-turn checkpoint a driver runs after completing parked
/// dynamic `import()` loads outside an interpreter call, so reactions queued
/// by the settlements run before the host considers the turn finished.
pub(crate) fn drain_host_jobs_with_limits(
    runtime: &mut Runtime,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    limits: ExecutionLimits,
) -> Result<(), ExecutionError> {
    let mut execution_budget = ExecutionBudget::new(limits);
    drain_host_jobs(runtime, compiler, &mut execution_budget)
}

/// Completes one host turn and performs its Promise-job checkpoint plus queued
/// finalization cleanup jobs. Ordinary JavaScript abrupt completion does not
/// suppress already-enqueued jobs; uncatchable host/runtime failures remain
/// immediate cancellation boundaries.
pub(super) fn complete_host_turn<T>(
    runtime: &mut Runtime,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    execution_budget: &mut ExecutionBudget,
    completion: Result<T, ExecutionError>,
) -> Result<T, ExecutionError> {
    runtime.kept_alive.clear();
    match completion {
        Ok(value) => {
            drain_host_jobs(runtime, compiler, execution_budget)?;
            runtime.kept_alive.clear();
            Ok(value)
        }
        Err(error @ ExecutionError::Exception(_)) => {
            drain_host_jobs(runtime, compiler, execution_budget)?;
            runtime.kept_alive.clear();
            Err(error)
        }
        Err(error) => {
            runtime.kept_alive.clear();
            Err(error)
        }
    }
}
