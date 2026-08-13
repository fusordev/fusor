//! ECMAScript Proxy construction and revocation entry points.

#![allow(
    clippy::needless_pass_by_value,
    clippy::option_option,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "typed Proxy continuations explicitly own tri-state specification slots and operands across re-entry"
)]

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// The `ReferenceError` a module namespace exotic object throws when reading
/// an export whose target binding is still in its temporal dead zone
/// (ECMA-262 10.4.6.2 `[[Get]]` / 10.4.6.3 `[[GetOwnProperty]]`).
fn namespace_uninitialized_error(
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<NativeFailure, NativeFailure> {
    Ok(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::ReferenceError,
            message: JsString::from_utf8("binding is not initialized")?,
        },
        origin,
    }))
}

/// Converts an ECMA-262 `EvaluateModuleSync` failure into the abrupt
/// completion a deferred namespace property access surfaces: a TypeError when
/// the module cannot complete synchronously, or the module's original
/// evaluation rejection value.
pub(super) fn deferred_namespace_evaluation_abrupt(
    realm: RealmId,
    origin: JsStackFrame,
    failure: crate::runtime::modules::DeferredNamespaceEvaluationFailure,
) -> Result<NativeFailure, NativeFailure> {
    use crate::runtime::modules::DeferredNamespaceEvaluationFailure;
    match failure {
        DeferredNamespaceEvaluationFailure::NotReady => {
            Ok(NativeFailure::Abrupt(PendingException {
                realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::TypeError,
                    message: JsString::from_utf8("module cannot be evaluated synchronously")?,
                },
                origin,
            }))
        }
        DeferredNamespaceEvaluationFailure::Thrown(value) => {
            Ok(NativeFailure::Abrupt(PendingException {
                realm,
                payload: PendingExceptionPayload::ThrownValue(value),
                origin,
            }))
        }
        DeferredNamespaceEvaluationFailure::Fault(fault) => {
            Err(NativeFailure::Execution(fault.into()))
        }
    }
}

/// Runs the deferred-namespace evaluation trigger for one internal-method
/// access when `object` is a namespace whose `[[Deferred]]` is still true.
pub(super) fn ensure_deferred_namespace_access(
    runtime: &mut Runtime,
    object: ObjectId,
    key: &PropertyKey,
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<(), NativeFailure> {
    if !runtime.module_namespace_is_deferred(object) {
        return Ok(());
    }
    match runtime.ensure_deferred_namespace_evaluation(object, Some(key)) {
        Ok(()) => Ok(()),
        Err(failure) => Err(deferred_namespace_evaluation_abrupt(
            realm, origin, failure,
        )?),
    }
}

pub(super) fn proxy_aware_is_array(
    runtime: &Runtime,
    value: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<bool, NativeFailure> {
    let mut current = value.heap_reference();
    while let Some(reference) = current {
        let Some(proxy) = runtime.proxy_state(reference)?.copied() else {
            return match reference {
                HeapReference::Object(object) => Ok(runtime.is_array_object(object)?),
                HeapReference::Function(_) => Ok(false),
            };
        };
        let Some(target) = proxy.target else {
            return Err(NativeFailure::Abrupt(PendingException {
                realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::TypeError,
                    message: JsString::from_utf8("revoked Proxy")?,
                },
                origin,
            }));
        };
        current = Some(target);
    }
    Ok(false)
}

fn proxy_reference_value(reference: HeapReference) -> StoredValue {
    match reference {
        HeapReference::Function(function) => StoredValue::Function(function),
        HeapReference::Object(object) => StoredValue::Object(object),
    }
}

fn proxy_property_key_value(key: &PropertyKey) -> Result<StoredValue, NativeFailure> {
    if let Some(index) = key.as_index() {
        return Ok(StoredValue::String(
            JsNumber::from_u32(index.get()).to_radix_string(10)?,
        ));
    }
    let atom = key.as_atom().ok_or(EngineFault::RuntimeInvariant {
        message: "Proxy property key is neither an index nor an atom",
    })?;
    match atom.kind() {
        crate::AtomKind::String => Ok(StoredValue::String(atom.description().cloned().ok_or(
            EngineFault::RuntimeInvariant {
                message: "Proxy string key has no description",
            },
        )?)),
        crate::AtomKind::Symbol | crate::AtomKind::GlobalSymbol => {
            Ok(StoredValue::Symbol(atom.clone()))
        }
        crate::AtomKind::Private => Err(EngineFault::RuntimeInvariant {
            message: "private name escaped through a Proxy internal method",
        }
        .into()),
    }
}

fn proxy_abrupt(
    realm: RealmId,
    origin: JsStackFrame,
    message: &'static str,
) -> Result<NativeDispatch, NativeFailure> {
    proxy_type_error(realm, origin, message)
}

fn continue_proxy_get_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: ProxyGetContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_proxy_get(runtime, state, value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::ProxyGet(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::ProxyGet(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Proxy [[Get]] nested operation produced a structured result",
        }
        .into()),
    }
}

fn continue_proxy_call_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: ProxyCallContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_proxy_call(runtime, state, value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::ProxyCall(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::ProxyCall(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Proxy call trap lookup produced a structured result",
        }
        .into()),
    }
}

fn continue_proxy_boolean_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: ProxyBooleanContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_proxy_boolean(runtime, state, value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::ProxyBoolean(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::ProxyBoolean(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Proxy Boolean trap lookup produced a structured result",
        }
        .into()),
    }
}

fn continue_ordinary_set_receiver_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: OrdinarySetReceiverContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_ordinary_set_receiver(runtime, state, value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::OrdinarySetReceiver(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::OrdinarySetReceiver(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "ordinary [[Set]] receiver operation produced a structured result",
        }
        .into()),
    }
}

fn continue_proxy_meta_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: ProxyMetaContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_proxy_meta(runtime, state, value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::ProxyMeta(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::ProxyMeta(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Proxy meta trap lookup produced a structured result",
        }
        .into()),
    }
}

fn continue_proxy_descriptor_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: ProxyDescriptorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_proxy_descriptor(runtime, state, Some(value), return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::ProxyDescriptor(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::ProxyDescriptor(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Proxy descriptor trap lookup produced a structured result",
        }
        .into()),
    }
}

fn continue_proxy_define_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: ProxyDefineContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_proxy_define(runtime, state, value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::ProxyDefine(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::ProxyDefine(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Proxy defineProperty operation produced a structured result",
        }
        .into()),
    }
}

fn continue_proxy_own_keys_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: ProxyOwnKeysContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_proxy_own_keys(runtime, state, value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::ProxyOwnKeys(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::ProxyOwnKeys(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Proxy ownKeys operation produced a structured result",
        }
        .into()),
    }
}

pub(super) fn advance_own_descriptor_query(
    runtime: &mut Runtime,
    state: OwnDescriptorQueryContinuation,
    completion: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(completion, StoredValue::Undefined) {
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
    }
    let StoredValue::Object(descriptor) = completion else {
        return Err(EngineFault::RuntimeInvariant {
            message: "internal own descriptor query returned a non-descriptor",
        }
        .into());
    };
    let result = match state.query {
        OwnDescriptorQuery::Present => true,
        OwnDescriptorQuery::Enumerable => {
            let key = runtime.predefined_property_key(PredefinedAtom::Enumerable);
            let Some(OwnProperty::Data {
                value: StoredValue::Boolean(value),
                ..
            }) = heap_own_property(runtime, HeapReference::Object(descriptor), &key)?
            else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "internal own descriptor lacks enumerable",
                }
                .into());
            };
            value
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::Boolean(result)))
}

pub(super) fn continue_own_descriptor_query_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    query: OwnDescriptorQuery,
) -> Result<NativeDispatch, NativeFailure> {
    let state = OwnDescriptorQueryContinuation { query };
    match dispatch {
        NativeDispatch::Immediate(value) => advance_own_descriptor_query(runtime, state, value),
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::OwnDescriptorQuery(state)],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::OwnDescriptorQuery(state)],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "own descriptor query produced a structured result",
        }
        .into()),
    }
}

fn definition_from_complete_own(own: &OwnProperty) -> PropertyDefinition {
    match own {
        OwnProperty::Data { layout, value } => PropertyDefinition::data(
            Requested::Present(value.duplicate()),
            Requested::Present(layout.writable() == Some(true)),
        )
        .with_enumerable(Requested::Present(layout.is_enumerable()))
        .with_configurable(Requested::Present(layout.is_configurable())),
        OwnProperty::Accessor {
            layout,
            getter,
            setter,
        } => PropertyDefinition::accessor(Requested::Present(*getter), Requested::Present(*setter))
            .with_enumerable(Requested::Present(layout.is_enumerable()))
            .with_configurable(Requested::Present(layout.is_configurable())),
    }
}

fn finish_proxy_get_own_descriptor(
    runtime: &mut Runtime,
    state: ProxyDescriptorContinuation,
    extensible: bool,
) -> Result<NativeDispatch, NativeFailure> {
    let result = state.trap_descriptor.ok_or(EngineFault::RuntimeInvariant {
        message: "Proxy descriptor validation lost its trap descriptor",
    })?;
    let target = state
        .target_descriptor
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Proxy descriptor validation lost its target descriptor",
        })?;
    let Some(result) = result else {
        if let Some(target) = target
            && (!target.layout().is_configurable() || !extensible)
        {
            return proxy_abrupt(
                state.realm,
                state.origin,
                "Proxy descriptor trap hid a protected target property",
            );
        }
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    };
    let setting_config_false = !result.layout().is_configurable();
    let reporting_non_writable_data = matches!(
        &result,
        OwnProperty::Data { layout, .. } if layout.writable() == Some(false)
    );
    let target_is_writable_data = matches!(
        &target,
        Some(OwnProperty::Data { layout, .. }) if layout.writable() == Some(true)
    );
    let definition = definition_from_complete_own(&result);
    let compatible = match &target {
        Some(target) => !matches!(
            validate_and_apply_existing(&definition, target),
            DefinitionDecision::Rejected
        ),
        None => !matches!(
            validate_and_apply_new(&definition, extensible),
            DefinitionDecision::Rejected
        ),
    };
    if !compatible
        || (target.is_none() && setting_config_false)
        || target
            .as_ref()
            .is_some_and(|target| target.layout().is_configurable() && setting_config_false)
        || (setting_config_false && reporting_non_writable_data && target_is_writable_data)
    {
        return proxy_abrupt(
            state.realm,
            state.origin,
            "Proxy descriptor trap returned an incompatible descriptor",
        );
    }
    let descriptor = build_descriptor_object(runtime, state.realm, result)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(descriptor)))
}

fn begin_proxy_descriptor_target_check(
    runtime: &mut Runtime,
    mut state: ProxyDescriptorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = ProxyDescriptorStage::TargetDescriptor;
    let dispatch = begin_internal_get_own_property(
        runtime,
        state.target,
        state.key.clone(),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_proxy_descriptor_after(runtime, dispatch, state, return_to, execution_budget)
}

fn begin_proxy_descriptor_extensible_check(
    runtime: &mut Runtime,
    mut state: ProxyDescriptorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = ProxyDescriptorStage::ExtensibleCheck;
    let dispatch = begin_internal_is_extensible(
        runtime,
        state.target,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_proxy_descriptor_after(runtime, dispatch, state, return_to, execution_budget)
}

pub(super) fn begin_internal_get_own_property(
    runtime: &mut Runtime,
    reference: HeapReference,
    key: PropertyKey,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if runtime.proxy_state(reference)?.is_some() {
        let proxy =
            runtime
                .proxy_state(reference)?
                .copied()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Proxy [[GetOwnProperty]] target is not a Proxy",
                })?;
        let (Some(target), Some(handler)) = (proxy.target, proxy.handler) else {
            return proxy_abrupt(realm, origin, "revoked Proxy");
        };
        let state = ProxyDescriptorContinuation {
            proxy: reference,
            target,
            handler,
            key,
            reader: None,
            trap_descriptor: None,
            target_descriptor: None,
            realm,
            origin: origin.clone(),
            stage: ProxyDescriptorStage::TrapLookup,
        };
        let dispatch = begin_internal_get(
            runtime,
            handler,
            proxy_reference_value(handler),
            runtime.predefined_property_key(PredefinedAtom::GetOwnPropertyDescriptor),
            realm,
            return_to,
            origin,
            execution_budget,
        )?;
        return continue_proxy_descriptor_after(
            runtime,
            dispatch,
            state,
            return_to,
            execution_budget,
        );
    }
    if let HeapReference::Object(object) = reference
        && runtime.module_namespace_is_deferred(object)
    {
        ensure_deferred_namespace_access(
            runtime,
            object,
            &key,
            realm,
            origin.clone(),
        )?;
    }
    if let HeapReference::Object(object) = reference
        && runtime.module_namespace_export_is_uninitialized(object, &key)?
    {
        return Err(namespace_uninitialized_error(realm, origin)?);
    }
    let Some(own) = heap_own_property(runtime, reference, &key)? else {
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    };
    Ok(NativeDispatch::Immediate(StoredValue::Object(
        build_descriptor_object(runtime, realm, own)?,
    )))
}

pub(super) fn advance_proxy_descriptor(
    runtime: &mut Runtime,
    mut state: ProxyDescriptorContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ProxyDescriptorStage::TrapLookup => {
            let completion = completion.ok_or(EngineFault::RuntimeInvariant {
                message: "Proxy descriptor trap lookup resumed without a value",
            })?;
            if matches!(completion, StoredValue::Undefined | StoredValue::Null) {
                return begin_internal_get_own_property(
                    runtime,
                    state.target,
                    state.key,
                    state.realm,
                    return_to,
                    state.origin,
                    execution_budget,
                );
            }
            let StoredValue::Function(trap) = completion else {
                return proxy_abrupt(
                    state.realm,
                    state.origin,
                    "Proxy getOwnPropertyDescriptor trap is not callable",
                );
            };
            state.stage = ProxyDescriptorStage::TrapCall;
            let arguments = vec![
                proxy_reference_value(state.target),
                proxy_property_key_value(&state.key)?,
            ];
            Ok(NativeDispatch::Call(NativeCall {
                function: trap,
                receiver: proxy_reference_value(state.handler),
                arguments: CallArguments::from_values(arguments),
                return_to,
                origin: state.origin.clone(),
                continuations: vec![NativeContinuation::ProxyDescriptor(Box::new(state))],
                pre_call: None,
                new_target: None,
                native_caller: None,
            }))
        }
        ProxyDescriptorStage::TrapCall => {
            let completion = completion.ok_or(EngineFault::RuntimeInvariant {
                message: "Proxy descriptor trap resumed without a value",
            })?;
            if matches!(completion, StoredValue::Undefined) {
                state.trap_descriptor = Some(None);
                return begin_proxy_descriptor_target_check(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                );
            }
            if completion.heap_reference().is_none() {
                return proxy_abrupt(
                    state.realm,
                    state.origin,
                    "Proxy descriptor trap returned a non-object",
                );
            }
            state.reader = Some(begin_descriptor_read(
                completion,
                state.realm,
                &state.origin,
            )?);
            state.stage = ProxyDescriptorStage::DescriptorRead;
            advance_proxy_descriptor(runtime, state, None, return_to, execution_budget)
        }
        ProxyDescriptorStage::DescriptorRead => {
            let outcome = advance_descriptor_read(
                runtime,
                state.reader.as_mut().ok_or(EngineFault::RuntimeInvariant {
                    message: "Proxy descriptor continuation lost its reader",
                })?,
                completion,
                state.realm,
                &state.origin,
                return_to,
                execution_budget,
            )?;
            match outcome {
                DescriptorReadOutcome::Complete(fields) => {
                    let descriptor =
                        complete_own_property_from_fields(fields, state.realm, &state.origin)?;
                    state.trap_descriptor = Some(Some(descriptor));
                    state.reader = None;
                    begin_proxy_descriptor_target_check(runtime, state, return_to, execution_budget)
                }
                DescriptorReadOutcome::Nested(dispatch) => continue_descriptor_nested(
                    *dispatch,
                    NativeContinuation::ProxyDescriptor(Box::new(state)),
                ),
            }
        }
        ProxyDescriptorStage::TargetDescriptor => {
            let completion = completion.ok_or(EngineFault::RuntimeInvariant {
                message: "Proxy target descriptor lookup resumed without a value",
            })?;
            if matches!(completion, StoredValue::Undefined) {
                state.target_descriptor = Some(None);
                return begin_proxy_descriptor_extensible_check(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                );
            }
            state.reader = Some(begin_descriptor_read(
                completion,
                state.realm,
                &state.origin,
            )?);
            state.stage = ProxyDescriptorStage::TargetDescriptorRead;
            advance_proxy_descriptor(runtime, state, None, return_to, execution_budget)
        }
        ProxyDescriptorStage::TargetDescriptorRead => {
            let outcome = advance_descriptor_read(
                runtime,
                state.reader.as_mut().ok_or(EngineFault::RuntimeInvariant {
                    message: "Proxy target descriptor continuation lost its reader",
                })?,
                completion,
                state.realm,
                &state.origin,
                return_to,
                execution_budget,
            )?;
            match outcome {
                DescriptorReadOutcome::Complete(fields) => {
                    state.target_descriptor = Some(Some(complete_own_property_from_fields(
                        fields,
                        state.realm,
                        &state.origin,
                    )?));
                    state.reader = None;
                    begin_proxy_descriptor_extensible_check(
                        runtime,
                        state,
                        return_to,
                        execution_budget,
                    )
                }
                DescriptorReadOutcome::Nested(dispatch) => continue_descriptor_nested(
                    *dispatch,
                    NativeContinuation::ProxyDescriptor(Box::new(state)),
                ),
            }
        }
        ProxyDescriptorStage::ExtensibleCheck => {
            let Some(StoredValue::Boolean(extensible)) = completion else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Proxy target extensibility check did not return a Boolean",
                }
                .into());
            };
            finish_proxy_get_own_descriptor(runtime, state, extensible)
        }
    }
}

fn finish_proxy_define_result(
    state: ProxyDefineContinuation,
    success: bool,
) -> Result<NativeDispatch, NativeFailure> {
    if !success {
        return match state.result {
            DefinePropertyResult::Boolean => {
                Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
            }
            DefinePropertyResult::Target => proxy_abrupt(
                state.realm,
                state.origin,
                "Proxy defineProperty trap rejected the definition",
            ),
        };
    }
    Ok(NativeDispatch::Immediate(match state.result {
        DefinePropertyResult::Boolean => StoredValue::Boolean(true),
        DefinePropertyResult::Target => proxy_reference_value(state.proxy),
    }))
}

fn begin_proxy_define_target_descriptor(
    runtime: &mut Runtime,
    mut state: ProxyDefineContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = ProxyDefineStage::TargetDescriptor;
    let dispatch = begin_internal_get_own_property(
        runtime,
        state.target,
        state.key.clone(),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_proxy_define_after(runtime, dispatch, state, return_to, execution_budget)
}

fn begin_proxy_define_extensible_check(
    runtime: &mut Runtime,
    mut state: ProxyDefineContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = ProxyDefineStage::ExtensibleCheck;
    let dispatch = begin_internal_is_extensible(
        runtime,
        state.target,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_proxy_define_after(runtime, dispatch, state, return_to, execution_budget)
}

#[allow(
    clippy::too_many_arguments,
    reason = "Proxy [[DefineOwnProperty]] carries the standard internal-method operands and continuation authority"
)]
pub(super) fn begin_internal_define_own_property(
    runtime: &mut Runtime,
    proxy: HeapReference,
    key: PropertyKey,
    definition: PropertyDefinition,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
    result: DefinePropertyResult,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(proxy_state) = runtime.proxy_state(proxy)?.copied() else {
        let base = proxy_reference_value(proxy);
        if let HeapReference::Object(object) = proxy
            && let Some(action) =
                typed_array_define_own_property_action(runtime, object, &key, &definition)?
        {
            match action {
                TypedArrayDefineAction::Ordinary => {}
                TypedArrayDefineAction::Rejected => {
                    return match result {
                        DefinePropertyResult::Boolean => {
                            Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
                        }
                        DefinePropertyResult::Target => proxy_abrupt(
                            realm,
                            origin,
                            "typed-array property definition was rejected",
                        ),
                    };
                }
                TypedArrayDefineAction::Complete => {
                    return Ok(NativeDispatch::Immediate(match result {
                        DefinePropertyResult::Target => base,
                        DefinePropertyResult::Boolean => StoredValue::Boolean(true),
                    }));
                }
                TypedArrayDefineAction::Store(index) => {
                    let value =
                        definition
                            .present_data_value()
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "typed-array define store lost its descriptor value",
                            })?;
                    return begin_typed_array_element_set(
                        runtime,
                        object,
                        TypedArrayPropertyKey::Index(index),
                        value.duplicate(),
                        TypedArraySetCompletion::Define(result),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
            }
        }
        if let HeapReference::Object(object) = proxy
            && runtime.module_namespace_is_deferred(object)
        {
            // ECMA-262 10.4.6.4 [[DefineOwnProperty]] resolves the exports
            // list through [[GetOwnProperty]], triggering deferred evaluation.
            ensure_deferred_namespace_access(
                runtime,
                object,
                &key,
                realm,
                origin.clone(),
            )?;
        }
        if let HeapReference::Object(object) = proxy
            && let Some(allowed) = runtime.module_namespace_define_export(
                object,
                &key,
                definition.requested_value().is_some(),
                definition.requested_writable(),
                definition.requested_enumerable(),
                definition.requested_configurable(),
            )?
        {
            return match result {
                DefinePropertyResult::Boolean => {
                    Ok(NativeDispatch::Immediate(StoredValue::Boolean(allowed)))
                }
                DefinePropertyResult::Target => {
                    if allowed {
                        Ok(NativeDispatch::Immediate(base))
                    } else {
                        proxy_abrupt(realm, origin, "namespace property definition was rejected")
                    }
                }
            };
        }
        if is_array_length_target(runtime, &base, &key)?
            && let Some(value) = definition.requested_value()
        {
            let conversion = array_length_define_target(
                base,
                JsString::from_utf8("length")?,
                value,
                ArrayLengthDefinition {
                    writable: definition.requested_writable(),
                    enumerable: definition.requested_enumerable(),
                    configurable: definition.requested_configurable(),
                    result,
                },
            );
            return begin_operator_primitive_conversion(
                runtime,
                value.duplicate(),
                OperatorPrimitiveHint::Number,
                conversion,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        let outcome = define_own_property(runtime, &base, key, &definition, execution_budget)?;
        return match outcome {
            PropertyDefinitionOutcome::Complete => Ok(NativeDispatch::Immediate(match result {
                DefinePropertyResult::Target => base,
                DefinePropertyResult::Boolean => StoredValue::Boolean(true),
            })),
            PropertyDefinitionOutcome::Failed(_)
                if matches!(result, DefinePropertyResult::Boolean) =>
            {
                Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
            }
            PropertyDefinitionOutcome::Failed(_) => {
                proxy_abrupt(realm, origin, "property definition was rejected")
            }
        };
    };
    let (Some(target), Some(handler)) = (proxy_state.target, proxy_state.handler) else {
        return proxy_abrupt(realm, origin, "revoked Proxy");
    };
    let descriptor = build_definition_object(runtime, realm, &definition)?;
    let state = ProxyDefineContinuation {
        proxy,
        target,
        handler,
        key,
        definition,
        descriptor,
        target_descriptor: None,
        reader: None,
        realm,
        origin: origin.clone(),
        result,
        stage: ProxyDefineStage::TrapLookup,
    };
    let trap_key = runtime.predefined_property_key(PredefinedAtom::DefineProperty);
    let dispatch = begin_internal_get(
        runtime,
        handler,
        proxy_reference_value(handler),
        trap_key,
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    continue_proxy_define_after(runtime, dispatch, state, return_to, execution_budget)
}

pub(super) fn advance_proxy_define(
    runtime: &mut Runtime,
    mut state: ProxyDefineContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ProxyDefineStage::TrapLookup => {
            if matches!(completion, StoredValue::Undefined | StoredValue::Null) {
                state.stage = ProxyDefineStage::TargetDefine;
                let dispatch = begin_internal_define_own_property(
                    runtime,
                    state.target,
                    state.key.clone(),
                    state.definition.duplicate(),
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                    DefinePropertyResult::Boolean,
                )?;
                return continue_proxy_define_after(
                    runtime,
                    dispatch,
                    state,
                    return_to,
                    execution_budget,
                );
            }
            let StoredValue::Function(trap) = completion else {
                return proxy_abrupt(
                    state.realm,
                    state.origin,
                    "Proxy defineProperty trap is not callable",
                );
            };
            state.stage = ProxyDefineStage::TrapCall;
            Ok(NativeDispatch::Call(NativeCall {
                function: trap,
                receiver: proxy_reference_value(state.handler),
                arguments: CallArguments::from_values(vec![
                    proxy_reference_value(state.target),
                    proxy_property_key_value(&state.key)?,
                    StoredValue::Object(state.descriptor),
                ]),
                return_to,
                origin: state.origin.clone(),
                continuations: vec![NativeContinuation::ProxyDefine(Box::new(state))],
                pre_call: None,
                new_target: None,
                native_caller: None,
            }))
        }
        ProxyDefineStage::TargetDefine => {
            let StoredValue::Boolean(success) = completion else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Proxy target definition did not return a Boolean",
                }
                .into());
            };
            finish_proxy_define_result(state, success)
        }
        ProxyDefineStage::TrapCall => {
            if !runtime.to_boolean(&completion)? {
                return finish_proxy_define_result(state, false);
            }
            begin_proxy_define_target_descriptor(runtime, state, return_to, execution_budget)
        }
        ProxyDefineStage::TargetDescriptor => {
            if matches!(completion, StoredValue::Undefined) {
                state.target_descriptor = Some(None);
                return begin_proxy_define_extensible_check(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                );
            }
            state.reader = Some(begin_descriptor_read(
                completion,
                state.realm,
                &state.origin,
            )?);
            state.stage = ProxyDefineStage::TargetDescriptorRead;
            advance_proxy_define(
                runtime,
                state,
                StoredValue::Undefined,
                return_to,
                execution_budget,
            )
        }
        ProxyDefineStage::TargetDescriptorRead => {
            let completion = (!matches!(completion, StoredValue::Undefined)).then_some(completion);
            match advance_descriptor_read(
                runtime,
                state.reader.as_mut().ok_or(EngineFault::RuntimeInvariant {
                    message: "Proxy define target descriptor lost its reader",
                })?,
                completion,
                state.realm,
                &state.origin,
                return_to,
                execution_budget,
            )? {
                DescriptorReadOutcome::Complete(fields) => {
                    state.target_descriptor = Some(Some(complete_own_property_from_fields(
                        fields,
                        state.realm,
                        &state.origin,
                    )?));
                    state.reader = None;
                    begin_proxy_define_extensible_check(runtime, state, return_to, execution_budget)
                }
                DescriptorReadOutcome::Nested(dispatch) => continue_descriptor_nested(
                    *dispatch,
                    NativeContinuation::ProxyDefine(Box::new(state)),
                ),
            }
        }
        ProxyDefineStage::ExtensibleCheck => {
            let StoredValue::Boolean(extensible) = completion else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Proxy define extensibility check did not return a Boolean",
                }
                .into());
            };
            let target = state
                .target_descriptor
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Proxy define validation lost its target descriptor",
                })?;
            let setting_config_false = state.definition.requested_configurable() == Some(false);
            let setting_writable_false = state.definition.requested_writable() == Some(false);
            let target_is_non_configurable_writable_data = matches!(
                target,
                Some(OwnProperty::Data { layout, .. })
                    if !layout.is_configurable() && layout.writable() == Some(true)
            );
            let compatible = match target {
                Some(target) => !matches!(
                    validate_and_apply_existing(&state.definition, target),
                    DefinitionDecision::Rejected
                ),
                None => !matches!(
                    validate_and_apply_new(&state.definition, extensible),
                    DefinitionDecision::Rejected
                ),
            };
            if !compatible
                || (target.is_none() && setting_config_false)
                || target
                    .as_ref()
                    .is_some_and(|target| target.layout().is_configurable() && setting_config_false)
                || (setting_writable_false && target_is_non_configurable_writable_data)
            {
                return proxy_abrupt(
                    state.realm,
                    state.origin,
                    "Proxy defineProperty trap accepted an incompatible descriptor",
                );
            }
            finish_proxy_define_result(state, true)
        }
    }
}

fn ordinary_own_keys(
    runtime: &mut Runtime,
    reference: HeapReference,
    execution_budget: &mut ExecutionBudget,
) -> Result<Vec<PropertyKey>, NativeFailure> {
    let (snapshot, work) = runtime.try_own_key_snapshot(reference, 0, KeyPhases::ALL)?;
    execution_budget.charge_instructions(work)?;
    let mut keys = Vec::new();
    keys.try_reserve_exact(snapshot.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: snapshot.len(),
        })?;
    for index in 0..snapshot.len() {
        keys.push(
            snapshot
                .get(index)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Proxy own-key snapshot shrank during materialization",
                })?
                .key()
                .clone(),
        );
    }
    Ok(keys)
}

fn materialize_key_list(
    runtime: &mut Runtime,
    realm: RealmId,
    keys: &[PropertyKey],
) -> Result<StoredValue, NativeFailure> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(keys.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: keys.len(),
        })?;
    for key in keys {
        values.push(proxy_property_key_value(key)?);
    }
    Ok(StoredValue::Object(runtime.allocate_array(realm, values)?))
}

pub(super) fn generated_key_list(
    runtime: &mut Runtime,
    value: StoredValue,
) -> Result<Vec<PropertyKey>, NativeFailure> {
    let StoredValue::Object(array) = value else {
        return Err(EngineFault::RuntimeInvariant {
            message: "internal [[OwnPropertyKeys]] did not return an Array",
        }
        .into());
    };
    let length = runtime
        .array_length(array)?
        .ok_or(EngineFault::RuntimeInvariant {
            message: "internal [[OwnPropertyKeys]] result is not an Array exotic",
        })?;
    let mut keys = Vec::new();
    keys.try_reserve_exact(length as usize)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: length as usize,
        })?;
    for raw in 0..length {
        let index = ArrayIndex::new(raw).ok_or(EngineFault::RuntimeInvariant {
            message: "internal own-key Array index exceeded the index domain",
        })?;
        let OwnProperty::Data { value, .. } = heap_own_property(
            runtime,
            HeapReference::Object(array),
            &PropertyKey::from_index(index),
        )?
        .ok_or(EngineFault::RuntimeInvariant {
            message: "internal own-key Array has a hole",
        })?
        else {
            return Err(EngineFault::RuntimeInvariant {
                message: "internal own-key Array element is not data",
            }
            .into());
        };
        keys.push(match value {
            StoredValue::String(name) => runtime.property_key_from_string(&name)?,
            StoredValue::Symbol(symbol) => runtime.property_key_from_symbol(&symbol)?,
            _ => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "internal own-key Array contains a non-key",
                }
                .into());
            }
        });
    }
    Ok(keys)
}

pub(super) fn begin_internal_own_keys(
    runtime: &mut Runtime,
    proxy: HeapReference,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(proxy_state) = runtime.proxy_state(proxy)?.copied() else {
        if let HeapReference::Object(object) = proxy
            && runtime.module_namespace_is_deferred(object)
        {
            // ECMA-262 10.4.6.6 [[OwnPropertyKeys]] step 1: the exports list
            // triggers deferred evaluation for every key kind.
            match runtime.ensure_deferred_namespace_evaluation(object, None) {
                Ok(()) => {}
                Err(failure) => {
                    return Err(deferred_namespace_evaluation_abrupt(
                        realm, origin, failure,
                    )?);
                }
            }
        }
        let keys = ordinary_own_keys(runtime, proxy, execution_budget)?;
        return Ok(NativeDispatch::Immediate(materialize_key_list(
            runtime, realm, &keys,
        )?));
    };
    let (Some(target), Some(handler)) = (proxy_state.target, proxy_state.handler) else {
        return proxy_abrupt(realm, origin, "revoked Proxy");
    };
    let state = ProxyOwnKeysContinuation {
        proxy,
        target,
        handler,
        result: None,
        length: 0,
        next_index: 0,
        trap_keys: Vec::new(),
        trap_key_set: HashSet::new(),
        target_keys: Vec::new(),
        next_target_key: 0,
        non_configurable_keys: Vec::new(),
        target_extensible: None,
        realm,
        origin: origin.clone(),
        stage: ProxyOwnKeysStage::TrapLookup,
    };
    let trap_key = runtime.predefined_property_key(PredefinedAtom::OwnKeys);
    let dispatch = begin_internal_get(
        runtime,
        handler,
        proxy_reference_value(handler),
        trap_key,
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    continue_proxy_own_keys_after(runtime, dispatch, state, return_to, execution_budget)
}

fn begin_proxy_own_keys_index(
    runtime: &mut Runtime,
    mut state: ProxyOwnKeysContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = ProxyOwnKeysStage::ResultIndexStart;
    advance_proxy_own_keys(
        runtime,
        state,
        StoredValue::Undefined,
        return_to,
        execution_budget,
    )
}

pub(super) fn finish_proxy_own_keys_length(
    runtime: &mut Runtime,
    mut state: ProxyOwnKeysContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let length = number_to_length(operator_to_number(value, state.realm, &state.origin)?);
    state.length = usize::try_from(length).map_err(|_| ExecutionError::LimitExceeded {
        resource: RuntimeResource::FrameValues,
        limit: usize_to_u64(usize::MAX),
        observed: length,
    })?;
    check_execution_limit(
        RuntimeResource::FrameValues,
        runtime.limits.max_active_frame_values,
        usize_to_u64(state.length),
    )?;
    state
        .trap_keys
        .try_reserve_exact(state.length)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: state.length,
        })?;
    state
        .trap_key_set
        .try_reserve(state.length)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: state.length,
        })?;
    begin_proxy_own_keys_index(runtime, state, return_to, execution_budget)
}

fn proxy_own_keys_validate(
    runtime: &mut Runtime,
    state: ProxyOwnKeysContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    for key in &state.non_configurable_keys {
        if !state.trap_key_set.contains(key) {
            return proxy_abrupt(
                state.realm,
                state.origin,
                "Proxy ownKeys trap omitted a non-configurable target key",
            );
        }
    }
    if state.target_extensible == Some(false)
        && (state.trap_keys.len() != state.target_keys.len()
            || state
                .target_keys
                .iter()
                .any(|key| !state.trap_key_set.contains(key)))
    {
        return proxy_abrupt(
            state.realm,
            state.origin,
            "Proxy ownKeys trap disagreed with a non-extensible target",
        );
    }
    Ok(NativeDispatch::Immediate(materialize_key_list(
        runtime,
        state.realm,
        &state.trap_keys,
    )?))
}

pub(super) fn advance_proxy_own_keys(
    runtime: &mut Runtime,
    mut state: ProxyOwnKeysContinuation,
    mut completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        match state.stage {
            ProxyOwnKeysStage::TrapLookup => {
                if matches!(completion, StoredValue::Undefined | StoredValue::Null) {
                    return begin_internal_own_keys(
                        runtime,
                        state.target,
                        state.realm,
                        return_to,
                        state.origin,
                        execution_budget,
                    );
                }
                let StoredValue::Function(trap) = completion else {
                    return proxy_abrupt(
                        state.realm,
                        state.origin,
                        "Proxy ownKeys trap is not callable",
                    );
                };
                state.stage = ProxyOwnKeysStage::TrapCall;
                return Ok(NativeDispatch::Call(NativeCall {
                    function: trap,
                    receiver: proxy_reference_value(state.handler),
                    arguments: CallArguments::from_values(vec![proxy_reference_value(
                        state.target,
                    )]),
                    return_to,
                    origin: state.origin.clone(),
                    continuations: vec![NativeContinuation::ProxyOwnKeys(Box::new(state))],
                    pre_call: None,
                    new_target: None,
                    native_caller: None,
                }));
            }
            ProxyOwnKeysStage::TrapCall => {
                let Some(result) = completion.heap_reference() else {
                    return proxy_abrupt(
                        state.realm,
                        state.origin,
                        "Proxy ownKeys trap returned a non-object",
                    );
                };
                state.result = Some(result);
                state.stage = ProxyOwnKeysStage::ResultLength;
                let length_key = runtime.predefined_property_key(PredefinedAtom::Length);
                let dispatch = begin_internal_get(
                    runtime,
                    result,
                    proxy_reference_value(result),
                    length_key,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                match dispatch {
                    NativeDispatch::Immediate(value) => {
                        completion = value;
                    }
                    dispatch => {
                        return continue_proxy_own_keys_after(
                            runtime,
                            dispatch,
                            state,
                            return_to,
                            execution_budget,
                        );
                    }
                }
            }
            ProxyOwnKeysStage::ResultLength => {
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    completion,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::ProxyOwnKeysLength(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            ProxyOwnKeysStage::ResultIndexStart => {
                if state.next_index >= state.length {
                    state.stage = ProxyOwnKeysStage::TargetKeys;
                    let dispatch = begin_internal_own_keys(
                        runtime,
                        state.target,
                        state.realm,
                        return_to,
                        state.origin.clone(),
                        execution_budget,
                    )?;
                    match dispatch {
                        NativeDispatch::Immediate(value) => {
                            completion = value;
                            continue;
                        }
                        dispatch => {
                            return continue_proxy_own_keys_after(
                                runtime,
                                dispatch,
                                state,
                                return_to,
                                execution_budget,
                            );
                        }
                    }
                }
                let raw =
                    u32::try_from(state.next_index).map_err(|_| ExecutionError::LimitExceeded {
                        resource: RuntimeResource::FrameValues,
                        limit: u64::from(u32::MAX),
                        observed: usize_to_u64(state.next_index),
                    })?;
                let key = PropertyKey::from_index(ArrayIndex::new(raw).ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "Proxy ownKeys result index exceeded the array-index domain",
                    },
                )?);
                state.stage = ProxyOwnKeysStage::ResultIndex;
                let result = state.result.ok_or(EngineFault::RuntimeInvariant {
                    message: "Proxy ownKeys result object was lost",
                })?;
                let dispatch = begin_internal_get(
                    runtime,
                    result,
                    proxy_reference_value(result),
                    key,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                match dispatch {
                    NativeDispatch::Immediate(value) => {
                        completion = value;
                    }
                    dispatch => {
                        return continue_proxy_own_keys_after(
                            runtime,
                            dispatch,
                            state,
                            return_to,
                            execution_budget,
                        );
                    }
                }
            }
            ProxyOwnKeysStage::ResultIndex => {
                let key = match &completion {
                    StoredValue::String(name) => runtime.property_key_from_string(name)?,
                    StoredValue::Symbol(symbol) => runtime.property_key_from_symbol(symbol)?,
                    _ => {
                        return proxy_abrupt(
                            state.realm,
                            state.origin,
                            "Proxy ownKeys trap returned a non-property key",
                        );
                    }
                };
                if !state.trap_key_set.insert(key.clone()) {
                    return proxy_abrupt(
                        state.realm,
                        state.origin,
                        "Proxy ownKeys trap returned duplicate keys",
                    );
                }
                state.trap_keys.push(key);
                state.next_index = state.next_index.saturating_add(1);
                state.stage = ProxyOwnKeysStage::ResultIndexStart;
            }
            ProxyOwnKeysStage::TargetKeys => {
                state.target_keys = generated_key_list(runtime, completion)?;
                state.stage = ProxyOwnKeysStage::TargetExtensible;
                let dispatch = begin_internal_is_extensible(
                    runtime,
                    state.target,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                match dispatch {
                    NativeDispatch::Immediate(value) => {
                        completion = value;
                    }
                    dispatch => {
                        return continue_proxy_own_keys_after(
                            runtime,
                            dispatch,
                            state,
                            return_to,
                            execution_budget,
                        );
                    }
                }
            }
            ProxyOwnKeysStage::TargetExtensible => {
                let StoredValue::Boolean(extensible) = completion else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "Proxy ownKeys target extensibility was not Boolean",
                    }
                    .into());
                };
                state.target_extensible = Some(extensible);
                state.stage = ProxyOwnKeysStage::TargetDescriptorStart;
            }
            ProxyOwnKeysStage::TargetDescriptorStart => {
                if state.next_target_key >= state.target_keys.len() {
                    return proxy_own_keys_validate(runtime, state);
                }
                let key = state.target_keys[state.next_target_key].clone();
                state.stage = ProxyOwnKeysStage::TargetDescriptor;
                let dispatch = begin_internal_get_own_property(
                    runtime,
                    state.target,
                    key,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                match dispatch {
                    NativeDispatch::Immediate(value) => {
                        completion = value;
                    }
                    dispatch => {
                        return continue_proxy_own_keys_after(
                            runtime,
                            dispatch,
                            state,
                            return_to,
                            execution_budget,
                        );
                    }
                }
            }
            ProxyOwnKeysStage::TargetDescriptor => {
                if let StoredValue::Object(descriptor) = completion {
                    let configurable_key =
                        runtime.predefined_property_key(PredefinedAtom::Configurable);
                    let Some(OwnProperty::Data {
                        value: StoredValue::Boolean(configurable),
                        ..
                    }) = heap_own_property(
                        runtime,
                        HeapReference::Object(descriptor),
                        &configurable_key,
                    )?
                    else {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "internal property descriptor lacks configurable",
                        }
                        .into());
                    };
                    if !configurable {
                        state
                            .non_configurable_keys
                            .push(state.target_keys[state.next_target_key].clone());
                    }
                } else if !matches!(completion, StoredValue::Undefined) {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "internal target descriptor is neither object nor undefined",
                    }
                    .into());
                }
                state.next_target_key = state.next_target_key.saturating_add(1);
                state.stage = ProxyOwnKeysStage::TargetDescriptorStart;
            }
        }
    }
}

fn optional_reference_value(reference: Option<HeapReference>) -> StoredValue {
    reference.map_or(StoredValue::Null, proxy_reference_value)
}

fn begin_proxy_meta(
    runtime: &mut Runtime,
    proxy: HeapReference,
    kind: ProxyMetaKind,
    requested_prototype: Option<Option<HeapReference>>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = runtime
        .proxy_state(proxy)?
        .copied()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Proxy meta internal method target is not a Proxy",
        })?;
    let (Some(target), Some(handler)) = (state.target, state.handler) else {
        return proxy_abrupt(realm, origin, "revoked Proxy");
    };
    let trap = match kind {
        ProxyMetaKind::GetPrototypeOf => PredefinedAtom::GetPrototypeOf,
        ProxyMetaKind::SetPrototypeOf => PredefinedAtom::SetPrototypeOf,
        ProxyMetaKind::IsExtensible => PredefinedAtom::IsExtensible,
        ProxyMetaKind::PreventExtensions => PredefinedAtom::PreventExtensions,
    };
    let continuation = ProxyMetaContinuation {
        proxy,
        target,
        handler,
        requested_prototype,
        trap_result: None,
        realm,
        origin: origin.clone(),
        kind,
        stage: ProxyMetaStage::TrapLookup,
    };
    let dispatch = begin_internal_get(
        runtime,
        handler,
        proxy_reference_value(handler),
        runtime.predefined_property_key(trap),
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    continue_proxy_meta_after(runtime, dispatch, continuation, return_to, execution_budget)
}

pub(super) fn begin_internal_get_prototype_of(
    runtime: &mut Runtime,
    reference: HeapReference,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if runtime.proxy_state(reference)?.is_some() {
        return begin_proxy_meta(
            runtime,
            reference,
            ProxyMetaKind::GetPrototypeOf,
            None,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    Ok(NativeDispatch::Immediate(optional_reference_value(
        runtime.object_record(reference)?.prototype(),
    )))
}

pub(super) fn begin_internal_set_prototype_of(
    runtime: &mut Runtime,
    reference: HeapReference,
    prototype: Option<HeapReference>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if runtime.proxy_state(reference)?.is_some() {
        return begin_proxy_meta(
            runtime,
            reference,
            ProxyMetaKind::SetPrototypeOf,
            Some(prototype),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    Ok(NativeDispatch::Immediate(StoredValue::Boolean(matches!(
        runtime.set_prototype_of(reference, prototype)?,
        SetPrototypeOutcome::Complete
    ))))
}

pub(super) fn begin_internal_is_extensible(
    runtime: &mut Runtime,
    reference: HeapReference,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if runtime.proxy_state(reference)?.is_some() {
        return begin_proxy_meta(
            runtime,
            reference,
            ProxyMetaKind::IsExtensible,
            None,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    Ok(NativeDispatch::Immediate(StoredValue::Boolean(
        runtime.is_extensible(reference)?,
    )))
}

pub(super) fn begin_internal_prevent_extensions(
    runtime: &mut Runtime,
    reference: HeapReference,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if runtime.proxy_state(reference)?.is_some() {
        return begin_proxy_meta(
            runtime,
            reference,
            ProxyMetaKind::PreventExtensions,
            None,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if let HeapReference::Object(object) = reference
        && runtime.typed_array_is_fixed_length(object)? == Some(false)
    {
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
    }
    runtime.prevent_extensions(reference)?;
    Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)))
}

pub(super) fn advance_proxy_meta(
    runtime: &mut Runtime,
    mut state: ProxyMetaContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ProxyMetaStage::TrapLookup => {
            if matches!(completion, StoredValue::Undefined | StoredValue::Null) {
                return match state.kind {
                    ProxyMetaKind::GetPrototypeOf => begin_internal_get_prototype_of(
                        runtime,
                        state.target,
                        state.realm,
                        return_to,
                        state.origin,
                        execution_budget,
                    ),
                    ProxyMetaKind::SetPrototypeOf => begin_internal_set_prototype_of(
                        runtime,
                        state.target,
                        state
                            .requested_prototype
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "Proxy [[SetPrototypeOf]] lost its requested prototype",
                            })?,
                        state.realm,
                        return_to,
                        state.origin,
                        execution_budget,
                    ),
                    ProxyMetaKind::IsExtensible => begin_internal_is_extensible(
                        runtime,
                        state.target,
                        state.realm,
                        return_to,
                        state.origin,
                        execution_budget,
                    ),
                    ProxyMetaKind::PreventExtensions => begin_internal_prevent_extensions(
                        runtime,
                        state.target,
                        state.realm,
                        return_to,
                        state.origin,
                        execution_budget,
                    ),
                };
            }
            let StoredValue::Function(trap) = completion else {
                return proxy_abrupt(state.realm, state.origin, "Proxy meta trap is not callable");
            };
            let mut arguments = vec![proxy_reference_value(state.target)];
            if state.kind == ProxyMetaKind::SetPrototypeOf {
                arguments.push(optional_reference_value(state.requested_prototype.ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "Proxy [[SetPrototypeOf]] lost its trap prototype",
                    },
                )?));
            }
            state.stage = ProxyMetaStage::TrapCall;
            Ok(NativeDispatch::Call(NativeCall {
                function: trap,
                receiver: proxy_reference_value(state.handler),
                arguments: CallArguments::from_values(arguments),
                return_to,
                origin: state.origin.clone(),
                continuations: vec![NativeContinuation::ProxyMeta(Box::new(state))],
                pre_call: None,
                new_target: None,
                native_caller: None,
            }))
        }
        ProxyMetaStage::TrapCall => {
            match state.kind {
                ProxyMetaKind::GetPrototypeOf => {
                    if !matches!(completion, StoredValue::Null)
                        && completion.heap_reference().is_none()
                    {
                        return proxy_abrupt(
                            state.realm,
                            state.origin,
                            "Proxy getPrototypeOf trap returned a non-object",
                        );
                    }
                    state.trap_result = Some(completion);
                }
                ProxyMetaKind::SetPrototypeOf | ProxyMetaKind::PreventExtensions => {
                    let result = runtime.to_boolean(&completion)?;
                    if !result {
                        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
                    }
                    state.trap_result = Some(StoredValue::Boolean(true));
                }
                ProxyMetaKind::IsExtensible => {
                    state.trap_result =
                        Some(StoredValue::Boolean(runtime.to_boolean(&completion)?));
                }
            }
            state.stage = ProxyMetaStage::ExtensibleCheck;
            let dispatch = begin_internal_is_extensible(
                runtime,
                state.target,
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            )?;
            continue_proxy_meta_after(runtime, dispatch, state, return_to, execution_budget)
        }
        ProxyMetaStage::ExtensibleCheck => {
            let StoredValue::Boolean(target_extensible) = completion else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Proxy extensibility check did not return a Boolean",
                }
                .into());
            };
            match state.kind {
                ProxyMetaKind::IsExtensible => {
                    let StoredValue::Boolean(trap_result) =
                        state
                            .trap_result
                            .take()
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "Proxy [[IsExtensible]] lost its trap result",
                            })?
                    else {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "Proxy [[IsExtensible]] trap result is not Boolean",
                        }
                        .into());
                    };
                    if trap_result != target_extensible {
                        return proxy_abrupt(
                            state.realm,
                            state.origin,
                            "Proxy isExtensible trap disagreed with its target",
                        );
                    }
                    Ok(NativeDispatch::Immediate(StoredValue::Boolean(trap_result)))
                }
                ProxyMetaKind::PreventExtensions => {
                    if target_extensible {
                        return proxy_abrupt(
                            state.realm,
                            state.origin,
                            "Proxy preventExtensions trap left its target extensible",
                        );
                    }
                    Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)))
                }
                ProxyMetaKind::GetPrototypeOf | ProxyMetaKind::SetPrototypeOf => {
                    if target_extensible {
                        return Ok(NativeDispatch::Immediate(state.trap_result.take().ok_or(
                            EngineFault::RuntimeInvariant {
                                message: "Proxy prototype trap lost its result",
                            },
                        )?));
                    }
                    state.stage = ProxyMetaStage::TargetPrototypeCheck;
                    let dispatch = begin_internal_get_prototype_of(
                        runtime,
                        state.target,
                        state.realm,
                        return_to,
                        state.origin.clone(),
                        execution_budget,
                    )?;
                    continue_proxy_meta_after(runtime, dispatch, state, return_to, execution_budget)
                }
            }
        }
        ProxyMetaStage::TargetPrototypeCheck => {
            let expected = match state.kind {
                ProxyMetaKind::GetPrototypeOf => {
                    state
                        .trap_result
                        .take()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "Proxy [[GetPrototypeOf]] lost its trap result",
                        })?
                }
                ProxyMetaKind::SetPrototypeOf => {
                    optional_reference_value(state.requested_prototype.ok_or(
                        EngineFault::RuntimeInvariant {
                            message: "Proxy [[SetPrototypeOf]] lost its requested prototype",
                        },
                    )?)
                }
                ProxyMetaKind::IsExtensible | ProxyMetaKind::PreventExtensions => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "non-prototype Proxy method reached prototype comparison",
                    }
                    .into());
                }
            };
            if !expected.same_value(&completion) {
                return proxy_abrupt(
                    state.realm,
                    state.origin,
                    "Proxy prototype trap disagreed with a non-extensible target",
                );
            }
            Ok(NativeDispatch::Immediate(match state.kind {
                ProxyMetaKind::GetPrototypeOf => expected,
                ProxyMetaKind::SetPrototypeOf => StoredValue::Boolean(true),
                ProxyMetaKind::IsExtensible | ProxyMetaKind::PreventExtensions => unreachable!(),
            }))
        }
    }
}

fn finish_proxy_boolean(
    state: ProxyBooleanContinuation,
    result: bool,
) -> Result<NativeDispatch, NativeFailure> {
    match state.completion {
        ProxyBooleanCompletion::Write { strict: true } if !result => {
            proxy_abrupt(state.realm, state.origin, "Proxy set trap returned false")
        }
        ProxyBooleanCompletion::Write { .. } => {
            Ok(NativeDispatch::Immediate(StoredValue::Undefined))
        }
        ProxyBooleanCompletion::Delete { strict: true } if !result => proxy_abrupt(
            state.realm,
            state.origin,
            "Proxy delete trap returned false",
        ),
        ProxyBooleanCompletion::Boolean | ProxyBooleanCompletion::Delete { .. } => {
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(result)))
        }
    }
}

fn begin_proxy_boolean(
    runtime: &mut Runtime,
    proxy: HeapReference,
    key: PropertyKey,
    value: Option<StoredValue>,
    receiver: Option<StoredValue>,
    kind: ProxyBooleanKind,
    completion: ProxyBooleanCompletion,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = runtime
        .proxy_state(proxy)?
        .copied()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Proxy Boolean internal method target is not a Proxy",
        })?;
    let (Some(target), Some(handler)) = (state.target, state.handler) else {
        return proxy_abrupt(realm, origin, "revoked Proxy");
    };
    let trap = match kind {
        ProxyBooleanKind::Has => PredefinedAtom::Has,
        ProxyBooleanKind::Delete => PredefinedAtom::DeleteProperty,
        ProxyBooleanKind::Set => PredefinedAtom::SetProperty,
    };
    let continuation = ProxyBooleanContinuation {
        proxy,
        target,
        handler,
        key,
        value,
        receiver,
        realm,
        origin: origin.clone(),
        kind,
        stage: ProxyBooleanStage::TrapLookup,
        completion,
        trap_result: None,
    };
    let dispatch = begin_internal_get(
        runtime,
        handler,
        proxy_reference_value(handler),
        runtime.predefined_property_key(trap),
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    continue_proxy_boolean_after(runtime, dispatch, continuation, return_to, execution_budget)
}

pub(super) fn begin_internal_has(
    runtime: &mut Runtime,
    reference: HeapReference,
    key: PropertyKey,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut current = reference;
    let mut remaining = runtime
        .functions
        .len()
        .saturating_add(runtime.objects.len())
        .saturating_add(1);
    loop {
        if remaining == 0 {
            return Err(EngineFault::RuntimeInvariant {
                message: "[[HasProperty]] prototype walk exceeded the live heap",
            }
            .into());
        }
        remaining -= 1;
        if runtime.proxy_state(current)?.is_some() {
            return begin_proxy_boolean(
                runtime,
                current,
                key,
                None,
                None,
                ProxyBooleanKind::Has,
                ProxyBooleanCompletion::Boolean,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        if let HeapReference::Object(object) = current
            && runtime.module_namespace_is_deferred(object)
        {
            // ECMA-262 10.4.6.5 [[HasProperty]] step 2: the exports list
            // triggers deferred evaluation for string keys (except "then").
            ensure_deferred_namespace_access(
                runtime,
                object,
                &key,
                realm,
                origin.clone(),
            )?;
        }
        if let HeapReference::Object(object) = current
            && let TypedArrayOwnProperty::IntegerIndexed(property) =
                runtime.typed_array_own_property(object, &key)?
        {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(
                property.is_some(),
            )));
        }
        if heap_own_property(runtime, current, &key)?.is_some() {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)));
        }
        let Some(prototype) = runtime.object_record(current)?.prototype() else {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
        };
        current = prototype;
    }
}

pub(super) fn begin_internal_delete(
    runtime: &mut Runtime,
    reference: HeapReference,
    key: PropertyKey,
    strict: bool,
    boolean_result: bool,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let completion = if boolean_result {
        ProxyBooleanCompletion::Boolean
    } else {
        ProxyBooleanCompletion::Delete { strict }
    };
    if runtime.proxy_state(reference)?.is_some() {
        return begin_proxy_boolean(
            runtime,
            reference,
            key,
            None,
            None,
            ProxyBooleanKind::Delete,
            completion,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if let HeapReference::Object(object) = reference
        && runtime.module_namespace_is_deferred(object)
    {
        // ECMA-262 10.4.6.7 [[Delete]] step 2: the exports list triggers
        // deferred evaluation for string keys (except "then").
        ensure_deferred_namespace_access(
            runtime,
            object,
            &key,
            realm,
            origin.clone(),
        )?;
    }
    let target = proxy_reference_value(reference);
    let result = match delete_static_property(runtime, &target, &key)? {
        PropertyDeleteOutcome::Deleted => true,
        PropertyDeleteOutcome::Refused => false,
        PropertyDeleteOutcome::Failed(_) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "object-valued internal [[Delete]] failed conversion",
            }
            .into());
        }
    };
    if !result && strict && !boolean_result {
        return Err(NativeFailure::Abrupt(property_exception_at(
            realm,
            origin,
            None,
            PropertyFailure::NotDeletable,
        )?));
    }
    finish_proxy_boolean(
        ProxyBooleanContinuation {
            proxy: reference,
            target: reference,
            handler: reference,
            key,
            value: None,
            receiver: None,
            realm,
            origin,
            kind: ProxyBooleanKind::Delete,
            stage: ProxyBooleanStage::TrapCall,
            completion,
            trap_result: None,
        },
        result,
    )
}

fn begin_ordinary_set_receiver(
    runtime: &mut Runtime,
    target: HeapReference,
    receiver: HeapReference,
    key: PropertyKey,
    name: JsString,
    value: StoredValue,
    strict: bool,
    boolean_result: bool,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = OrdinarySetReceiverContinuation {
        target,
        receiver,
        key: key.clone(),
        name,
        value,
        strict,
        boolean_result,
        realm,
        origin: origin.clone(),
        stage: OrdinarySetReceiverStage::Descriptor,
    };
    let dispatch = begin_internal_get_own_property(
        runtime,
        receiver,
        key,
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    continue_ordinary_set_receiver_after(runtime, dispatch, state, return_to, execution_budget)
}

pub(super) fn advance_ordinary_set_receiver(
    runtime: &mut Runtime,
    mut state: OrdinarySetReceiverContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        OrdinarySetReceiverStage::Descriptor => {
            let current = internal_complete_own_property(runtime, &completion)?;
            let definition = match current {
                Some(OwnProperty::Accessor { .. }) => {
                    return finish_ordinary_set_receiver(state, false);
                }
                Some(OwnProperty::Data { layout, .. }) if layout.writable() != Some(true) => {
                    return finish_ordinary_set_receiver(state, false);
                }
                Some(OwnProperty::Data { .. }) => PropertyDefinition::data(
                    Requested::Present(state.value.duplicate()),
                    Requested::Absent,
                ),
                None => PropertyDefinition::data(
                    Requested::Present(state.value.duplicate()),
                    Requested::Present(true),
                )
                .with_enumerable(Requested::Present(true))
                .with_configurable(Requested::Present(true)),
            };
            state.stage = OrdinarySetReceiverStage::Define;
            let dispatch = begin_internal_define_own_property(
                runtime,
                state.receiver,
                state.key.clone(),
                definition,
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
                DefinePropertyResult::Boolean,
            )?;
            continue_ordinary_set_receiver_after(
                runtime,
                dispatch,
                state,
                return_to,
                execution_budget,
            )
        }
        OrdinarySetReceiverStage::Define => {
            let StoredValue::Boolean(success) = completion else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "ordinary [[Set]] receiver definition returned a non-Boolean",
                }
                .into());
            };
            finish_ordinary_set_receiver(state, success)
        }
    }
}

fn finish_ordinary_set_receiver(
    state: OrdinarySetReceiverContinuation,
    success: bool,
) -> Result<NativeDispatch, NativeFailure> {
    if state.boolean_result {
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(success)));
    }
    if success || !state.strict {
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    }
    Err(NativeFailure::Abrupt(property_exception_at(
        state.realm,
        state.origin,
        Some(&state.name),
        PropertyFailure::ReadOnly,
    )?))
}

#[derive(Clone, Copy)]
enum IntegerIndexedSetAction {
    Complete,
    RejectImmutable,
    Store {
        object: ObjectId,
        key: TypedArrayPropertyKey,
    },
}

/// Applies the typed-array prefix of `[[Set]]` at every object reached by an
/// ordinary prototype walk. `None` means the valid alternate-receiver case
/// must continue through `OrdinarySet` with the virtual element descriptor.
fn integer_indexed_set_action(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
    receiver: &StoredValue,
) -> Result<Option<IntegerIndexedSetAction>, NativeFailure> {
    let HeapReference::Object(object) = reference else {
        return Ok(None);
    };
    let Some(key) = runtime.typed_array_property_key(object, key)? else {
        return Ok(None);
    };
    if key == TypedArrayPropertyKey::Ordinary {
        return Ok(None);
    }
    if runtime.is_typed_array_backing_buffer_immutable(object)? {
        return Ok(Some(IntegerIndexedSetAction::RejectImmutable));
    }
    if receiver.strict_equals(&StoredValue::Object(object)) {
        return Ok(Some(IntegerIndexedSetAction::Store { object, key }));
    }
    let valid = match key {
        TypedArrayPropertyKey::Index(index) => {
            runtime.typed_array_read_index(object, index)?.is_some()
        }
        TypedArrayPropertyKey::Invalid => false,
        TypedArrayPropertyKey::Ordinary => unreachable!("filtered above"),
    };
    Ok((!valid).then_some(IntegerIndexedSetAction::Complete))
}

#[allow(
    clippy::too_many_arguments,
    reason = "Proxy [[Set]] keeps target, receiver, key, value, completion mode, and resume authority explicit"
)]
pub(super) fn begin_internal_set(
    runtime: &mut Runtime,
    reference: HeapReference,
    key: PropertyKey,
    name: JsString,
    value: StoredValue,
    receiver: StoredValue,
    strict: bool,
    boolean_result: bool,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let completion = if boolean_result {
        ProxyBooleanCompletion::Boolean
    } else {
        ProxyBooleanCompletion::Write { strict }
    };
    let mut current = reference;
    let mut remaining = runtime
        .functions
        .len()
        .saturating_add(runtime.objects.len())
        .saturating_add(1);
    let (reference, target_own) = loop {
        if remaining == 0 {
            return Err(EngineFault::RuntimeInvariant {
                message: "ordinary [[Set]] prototype walk exceeded the live heap",
            }
            .into());
        }
        remaining -= 1;
        if runtime.proxy_state(current)?.is_some() {
            return begin_proxy_boolean(
                runtime,
                current,
                key,
                Some(value),
                Some(receiver),
                ProxyBooleanKind::Set,
                completion,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        if let Some(action) = integer_indexed_set_action(runtime, current, &key, &receiver)? {
            match action {
                IntegerIndexedSetAction::Complete => {
                    return Ok(NativeDispatch::Immediate(if boolean_result {
                        StoredValue::Boolean(true)
                    } else {
                        StoredValue::Undefined
                    }));
                }
                IntegerIndexedSetAction::RejectImmutable => {
                    if boolean_result {
                        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
                    }
                    if !strict {
                        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
                    }
                    return Err(NativeFailure::Abrupt(property_exception_at(
                        realm,
                        origin,
                        Some(&name),
                        PropertyFailure::ReadOnly,
                    )?));
                }
                IntegerIndexedSetAction::Store { object, key } => {
                    return begin_typed_array_element_set(
                        runtime,
                        object,
                        key,
                        value,
                        if boolean_result {
                            TypedArraySetCompletion::ReflectSet
                        } else {
                            TypedArraySetCompletion::LanguageWrite
                        },
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
            }
        }
        let own = heap_own_property(runtime, current, &key)?;
        if own.is_some() {
            break (current, own);
        }
        let Some(prototype) = runtime.object_record(current)?.prototype() else {
            break (current, None);
        };
        current = prototype;
    };
    let receiver_needs_definition = match &target_own {
        None => true,
        Some(OwnProperty::Data { layout, .. }) => layout.writable() == Some(true),
        Some(OwnProperty::Accessor { .. }) => false,
    };
    if let Some(receiver_reference) = receiver.heap_reference()
        && runtime.proxy_state(receiver_reference)?.is_some()
        && receiver_needs_definition
    {
        return begin_ordinary_set_receiver(
            runtime,
            reference,
            receiver_reference,
            key,
            name,
            value,
            strict,
            boolean_result,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    let failure_key = key.clone();
    let failure_name = name.clone();
    let failure_receiver = receiver.duplicate();
    let target = proxy_reference_value(reference);
    let dispatch = reflect_set_property(
        runtime,
        realm,
        target,
        key,
        name,
        value,
        receiver,
        return_to,
        origin.clone(),
        execution_budget,
    )?;
    match dispatch {
        NativeDispatch::Immediate(StoredValue::Boolean(result)) => {
            if boolean_result {
                return Ok(NativeDispatch::Immediate(StoredValue::Boolean(result)));
            }
            if result || !strict {
                return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
            }
            let failure =
                ordinary_set_failure(runtime, reference, &failure_key, &failure_receiver)?;
            Err(NativeFailure::Abrupt(property_exception_at(
                realm,
                origin,
                Some(&failure_name),
                failure,
            )?))
        }
        NativeDispatch::Call(mut call) => {
            if !boolean_result {
                prepend_native_continuations(&mut call, vec![NativeContinuation::ProxyWrite])?;
            }
            Ok(NativeDispatch::Call(call))
        }
        dispatch => Ok(dispatch),
    }
}

fn ordinary_set_failure(
    runtime: &Runtime,
    target: HeapReference,
    key: &PropertyKey,
    receiver: &StoredValue,
) -> Result<PropertyFailure, NativeFailure> {
    if let Some(own) = heap_own_property(runtime, target, key)? {
        match own {
            OwnProperty::Data { layout, .. } if layout.writable() != Some(true) => {
                return Ok(PropertyFailure::ReadOnly);
            }
            OwnProperty::Accessor { setter: None, .. } => {
                return Ok(PropertyFailure::NoSetter);
            }
            OwnProperty::Data { .. }
            | OwnProperty::Accessor {
                setter: Some(_), ..
            } => {}
        }
    }
    let Some(receiver) = receiver.heap_reference() else {
        return Ok(PropertyFailure::NotObject);
    };
    if let Some(own) = heap_own_property(runtime, receiver, key)? {
        return Ok(match own {
            OwnProperty::Data { layout, .. } if layout.writable() != Some(true) => {
                PropertyFailure::ReadOnly
            }
            OwnProperty::Data { .. } | OwnProperty::Accessor { .. } => {
                PropertyFailure::NotConfigurable
            }
        });
    }
    if !runtime.object_record(receiver)?.is_extensible() {
        return Ok(PropertyFailure::NonExtensible);
    }
    Ok(PropertyFailure::ReadOnly)
}

pub(super) fn advance_proxy_boolean(
    runtime: &mut Runtime,
    mut state: ProxyBooleanContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ProxyBooleanStage::TrapLookup => {
            if matches!(completion, StoredValue::Undefined | StoredValue::Null) {
                return match state.kind {
                    ProxyBooleanKind::Has => begin_internal_has(
                        runtime,
                        state.target,
                        state.key,
                        state.realm,
                        return_to,
                        state.origin,
                        execution_budget,
                    ),
                    ProxyBooleanKind::Delete => begin_internal_delete(
                        runtime,
                        state.target,
                        state.key,
                        matches!(
                            state.completion,
                            ProxyBooleanCompletion::Delete { strict: true }
                        ),
                        matches!(state.completion, ProxyBooleanCompletion::Boolean),
                        state.realm,
                        return_to,
                        state.origin,
                        execution_budget,
                    ),
                    ProxyBooleanKind::Set => {
                        let value = state.value.take().ok_or(EngineFault::RuntimeInvariant {
                            message: "Proxy [[Set]] lost its value",
                        })?;
                        let receiver =
                            state.receiver.take().ok_or(EngineFault::RuntimeInvariant {
                                message: "Proxy [[Set]] lost its receiver",
                            })?;
                        let key_value = proxy_property_key_value(&state.key)?;
                        let name = computed_property_operand(runtime, &key_value)?.name;
                        begin_internal_set(
                            runtime,
                            state.target,
                            state.key,
                            name,
                            value,
                            receiver,
                            matches!(
                                state.completion,
                                ProxyBooleanCompletion::Write { strict: true }
                            ),
                            matches!(state.completion, ProxyBooleanCompletion::Boolean),
                            state.realm,
                            return_to,
                            state.origin,
                            execution_budget,
                        )
                    }
                };
            }
            let StoredValue::Function(trap) = completion else {
                return proxy_abrupt(
                    state.realm,
                    state.origin,
                    "Proxy internal-method trap is not callable",
                );
            };
            let mut arguments = vec![
                proxy_reference_value(state.target),
                proxy_property_key_value(&state.key)?,
            ];
            if state.kind == ProxyBooleanKind::Set {
                arguments.push(
                    state
                        .value
                        .as_ref()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "Proxy [[Set]] lost its trap value",
                        })?
                        .duplicate(),
                );
                arguments.push(
                    state
                        .receiver
                        .as_ref()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "Proxy [[Set]] lost its trap receiver",
                        })?
                        .duplicate(),
                );
            }
            state.stage = ProxyBooleanStage::TrapCall;
            Ok(NativeDispatch::Call(NativeCall {
                function: trap,
                receiver: proxy_reference_value(state.handler),
                arguments: CallArguments::from_values(arguments),
                return_to,
                origin: state.origin.clone(),
                continuations: vec![NativeContinuation::ProxyBoolean(Box::new(state))],
                pre_call: None,
                new_target: None,
                native_caller: None,
            }))
        }
        ProxyBooleanStage::TrapCall => {
            let result = runtime.to_boolean(&completion)?;
            let needs_target_check = match state.kind {
                ProxyBooleanKind::Has => !result,
                ProxyBooleanKind::Delete | ProxyBooleanKind::Set => result,
            };
            if !needs_target_check {
                return finish_proxy_boolean(state, result);
            }
            state.trap_result = Some(result);
            state.stage = ProxyBooleanStage::TargetDescriptor;
            let dispatch = begin_internal_get_own_property(
                runtime,
                state.target,
                state.key.clone(),
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            )?;
            continue_proxy_boolean_after(runtime, dispatch, state, return_to, execution_budget)
        }
        ProxyBooleanStage::TargetDescriptor => {
            let own = if matches!(completion, StoredValue::Undefined) {
                None
            } else {
                let mut reader = begin_descriptor_read(completion, state.realm, &state.origin)?;
                match advance_descriptor_read(
                    runtime,
                    &mut reader,
                    None,
                    state.realm,
                    &state.origin,
                    return_to,
                    execution_budget,
                )? {
                    DescriptorReadOutcome::Complete(fields) => Some(
                        complete_own_property_from_fields(fields, state.realm, &state.origin)?,
                    ),
                    DescriptorReadOutcome::Nested(_) => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "generated internal descriptor unexpectedly suspended",
                        }
                        .into());
                    }
                }
            };
            let result = state.trap_result.ok_or(EngineFault::RuntimeInvariant {
                message: "Proxy Boolean invariant check lost its trap result",
            })?;
            match state.kind {
                ProxyBooleanKind::Set => {
                    if let Some(own) = own {
                        match own {
                            OwnProperty::Data { layout, value }
                                if !layout.is_configurable()
                                    && layout.writable() == Some(false) =>
                            {
                                let requested =
                                    state.value.as_ref().ok_or(EngineFault::RuntimeInvariant {
                                        message: "Proxy [[Set]] lost its invariant value",
                                    })?;
                                if !requested.same_value(&value) {
                                    return proxy_abrupt(
                                        state.realm,
                                        state.origin,
                                        "Proxy set trap violated a frozen data property",
                                    );
                                }
                            }
                            OwnProperty::Accessor {
                                layout,
                                setter: None,
                                ..
                            } if !layout.is_configurable() => {
                                return proxy_abrupt(
                                    state.realm,
                                    state.origin,
                                    "Proxy set trap violated a setterless property",
                                );
                            }
                            OwnProperty::Data { .. } | OwnProperty::Accessor { .. } => {}
                        }
                    }
                    finish_proxy_boolean(state, result)
                }
                ProxyBooleanKind::Has | ProxyBooleanKind::Delete => {
                    let Some(own) = own else {
                        return finish_proxy_boolean(state, result);
                    };
                    if !own.layout().is_configurable() {
                        return proxy_abrupt(
                            state.realm,
                            state.origin,
                            "Proxy Boolean trap violated a non-configurable target property",
                        );
                    }
                    state.stage = ProxyBooleanStage::TargetExtensible;
                    let dispatch = begin_internal_is_extensible(
                        runtime,
                        state.target,
                        state.realm,
                        return_to,
                        state.origin.clone(),
                        execution_budget,
                    )?;
                    continue_proxy_boolean_after(
                        runtime,
                        dispatch,
                        state,
                        return_to,
                        execution_budget,
                    )
                }
            }
        }
        ProxyBooleanStage::TargetExtensible => {
            let StoredValue::Boolean(extensible) = completion else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Proxy Boolean target extensibility was not Boolean",
                }
                .into());
            };
            if !extensible {
                return proxy_abrupt(
                    state.realm,
                    state.origin,
                    "Proxy Boolean trap violated a non-extensible target",
                );
            }
            let result = state.trap_result.ok_or(EngineFault::RuntimeInvariant {
                message: "Proxy Boolean extensibility check lost its trap result",
            })?;
            finish_proxy_boolean(state, result)
        }
    }
}

pub(super) fn begin_proxy_function_call(
    runtime: &mut Runtime,
    proxy: FunctionId,
    receiver: StoredValue,
    arguments: CallArguments,
    new_target: Option<FunctionId>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = runtime
        .proxy_state(HeapReference::Function(proxy))?
        .copied()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Proxy call target is not a callable Proxy",
        })?;
    if new_target.is_some() && !state.constructable {
        return proxy_abrupt(state.realm, origin, "Proxy target is not a constructor");
    }
    let (Some(HeapReference::Function(target)), Some(handler)) = (state.target, state.handler)
    else {
        return proxy_abrupt(state.realm, origin, "revoked Proxy");
    };
    let trap_atom = if new_target.is_some() {
        PredefinedAtom::Construct
    } else {
        PredefinedAtom::Apply
    };
    let continuation = ProxyCallContinuation {
        proxy,
        target,
        handler,
        receiver,
        arguments,
        new_target,
        realm: state.realm,
        origin: origin.clone(),
        stage: ProxyCallStage::TrapLookup,
    };
    let dispatch = begin_internal_get(
        runtime,
        handler,
        proxy_reference_value(handler),
        runtime.predefined_property_key(trap_atom),
        state.realm,
        return_to,
        origin,
        execution_budget,
    )?;
    continue_proxy_call_after(runtime, dispatch, continuation, return_to, execution_budget)
}

pub(super) fn advance_proxy_call(
    runtime: &mut Runtime,
    mut state: ProxyCallContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    _execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ProxyCallStage::TrapLookup => {
            if matches!(completion, StoredValue::Undefined | StoredValue::Null) {
                return Ok(NativeDispatch::Call(NativeCall {
                    function: state.target,
                    receiver: state.receiver,
                    arguments: state.arguments,
                    return_to,
                    origin: state.origin,
                    continuations: Vec::new(),
                    pre_call: None,
                    new_target: state.new_target,
                    native_caller: None,
                }));
            }
            let StoredValue::Function(trap) = completion else {
                return proxy_abrupt(state.realm, state.origin, "Proxy call trap is not callable");
            };
            let argument_values = state.arguments.into_remaining_values();
            let arguments_array = runtime.allocate_array(state.realm, argument_values)?;
            state.arguments = CallArguments::empty();
            let mut trap_arguments = vec![
                StoredValue::Function(state.target),
                StoredValue::Object(arguments_array),
            ];
            if let Some(new_target) = state.new_target {
                trap_arguments.push(StoredValue::Function(new_target));
            } else {
                trap_arguments.insert(1, state.receiver.duplicate());
            }
            state.stage = ProxyCallStage::TrapCall;
            Ok(NativeDispatch::Call(NativeCall {
                function: trap,
                receiver: proxy_reference_value(state.handler),
                arguments: CallArguments::from_values(trap_arguments),
                return_to,
                origin: state.origin.clone(),
                continuations: vec![NativeContinuation::ProxyCall(Box::new(state))],
                pre_call: None,
                new_target: None,
                native_caller: None,
            }))
        }
        ProxyCallStage::TrapCall => {
            if state.new_target.is_some() && completion.heap_reference().is_none() {
                return proxy_abrupt(
                    state.realm,
                    state.origin,
                    "Proxy construct trap returned a non-object",
                );
            }
            Ok(NativeDispatch::Immediate(completion))
        }
    }
}

pub(super) fn begin_internal_get(
    runtime: &mut Runtime,
    reference: HeapReference,
    receiver: StoredValue,
    key: PropertyKey,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut current = reference;
    let mut remaining = runtime
        .functions
        .len()
        .saturating_add(runtime.objects.len())
        .saturating_add(1);
    loop {
        if remaining == 0 {
            return Err(EngineFault::RuntimeInvariant {
                message: "[[Get]] prototype walk exceeded the live heap",
            }
            .into());
        }
        remaining -= 1;
        if runtime.proxy_state(current)?.is_some() {
            return begin_proxy_get(
                runtime,
                current,
                receiver,
                key,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        if let HeapReference::Object(object) = current
            && runtime.module_namespace_is_deferred(object)
        {
            // ECMA-262 10.4.6.2 [[Get]] step 2: GetModuleExportsList triggers
            // deferred evaluation for string keys (except "then").
            ensure_deferred_namespace_access(
                runtime,
                object,
                &key,
                realm,
                origin.clone(),
            )?;
        }
        if let HeapReference::Object(object) = current
            && runtime.module_namespace_export_is_uninitialized(object, &key)?
        {
            return Err(namespace_uninitialized_error(realm, origin)?);
        }
        match heap_own_property(runtime, current, &key)? {
            Some(OwnProperty::Data { value, .. }) => {
                return Ok(NativeDispatch::Immediate(value));
            }
            Some(OwnProperty::Accessor {
                getter: Some(function),
                ..
            }) => {
                return Ok(NativeDispatch::Call(NativeCall {
                    function,
                    receiver,
                    arguments: CallArguments::empty(),
                    return_to,
                    origin,
                    continuations: Vec::new(),
                    pre_call: None,
                    new_target: None,
                    native_caller: None,
                }));
            }
            Some(OwnProperty::Accessor { getter: None, .. }) => {
                return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
            }
            None => {}
        }
        let Some(prototype) = runtime.object_record(current)?.prototype() else {
            return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
        };
        current = prototype;
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the generic Get boundary keeps the primitive fallback, Proxy-aware heap dispatch, source origin, and execution authority explicit"
)]
pub(super) fn begin_value_get(
    runtime: &mut Runtime,
    base: &StoredValue,
    key: PropertyKey,
    property_name: Option<&JsString>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(reference) = base.heap_reference() {
        return begin_internal_get(
            runtime,
            reference,
            base.duplicate(),
            key,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    match read_static_property(runtime, realm, base, &key)? {
        PropertyReadOutcome::Value(value) => Ok(NativeDispatch::Immediate(value)),
        PropertyReadOutcome::Getter { function, receiver } => {
            Ok(NativeDispatch::Call(NativeCall {
                function,
                receiver,
                arguments: CallArguments::empty(),
                return_to,
                origin,
                continuations: Vec::new(),
                pre_call: None,
                new_target: None,
                native_caller: None,
            }))
        }
        PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            realm,
            origin,
            property_name,
            failure,
        )?)),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the generic HasProperty boundary preserves primitive virtual properties and Proxy-aware prototype traversal"
)]
pub(super) fn begin_value_has(
    runtime: &mut Runtime,
    base: &StoredValue,
    key: PropertyKey,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let reference = match base {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Boolean(_) => HeapReference::Object(runtime.realm_boolean_prototype(realm)?),
        StoredValue::Number(_) => HeapReference::Object(runtime.realm_number_prototype(realm)?),
        StoredValue::BigInt(_) => HeapReference::Object(runtime.realm_bigint_prototype(realm)?),
        StoredValue::Symbol(_) => HeapReference::Object(runtime.realm_symbol_prototype(realm)?),
        StoredValue::String(value) => {
            if key
                .as_index()
                .is_some_and(|index| index.get() < value.len())
                || key.as_atom().and_then(crate::Atom::predefined_atom)
                    == Some(PredefinedAtom::Length)
            {
                return Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)));
            }
            HeapReference::Object(runtime.realm_string_prototype(realm)?)
        }
        StoredValue::Undefined | StoredValue::Null => {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
        }
    };
    begin_internal_has(
        runtime,
        reference,
        key,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_proxy_get(
    runtime: &mut Runtime,
    proxy: HeapReference,
    receiver: StoredValue,
    key: PropertyKey,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = runtime
        .proxy_state(proxy)?
        .copied()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Proxy [[Get]] target is not a Proxy exotic object",
        })?;
    let (Some(target), Some(handler)) = (state.target, state.handler) else {
        return proxy_abrupt(realm, origin, "revoked Proxy");
    };
    let continuation = ProxyGetContinuation {
        proxy,
        target,
        handler,
        key,
        receiver,
        realm,
        origin: origin.clone(),
        stage: ProxyGetStage::TrapLookup,
    };
    let trap_key = runtime.predefined_property_key(PredefinedAtom::Get);
    let dispatch = begin_internal_get(
        runtime,
        handler,
        proxy_reference_value(handler),
        trap_key,
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    continue_proxy_get_after(runtime, dispatch, continuation, return_to, execution_budget)
}

pub(super) fn advance_proxy_get(
    runtime: &mut Runtime,
    mut state: ProxyGetContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ProxyGetStage::TrapLookup => {
            if matches!(completion, StoredValue::Undefined | StoredValue::Null) {
                return begin_internal_get(
                    runtime,
                    state.target,
                    state.receiver,
                    state.key,
                    state.realm,
                    return_to,
                    state.origin,
                    execution_budget,
                );
            }
            let StoredValue::Function(trap) = completion else {
                return proxy_abrupt(state.realm, state.origin, "Proxy get trap is not callable");
            };
            let key = proxy_property_key_value(&state.key)?;
            let arguments = vec![
                proxy_reference_value(state.target),
                key,
                state.receiver.duplicate(),
            ];
            state.stage = ProxyGetStage::TrapCall;
            Ok(NativeDispatch::Call(NativeCall {
                function: trap,
                receiver: proxy_reference_value(state.handler),
                arguments: CallArguments::from_values(arguments),
                return_to,
                origin: state.origin.clone(),
                continuations: vec![NativeContinuation::ProxyGet(Box::new(state))],
                pre_call: None,
                new_target: None,
                native_caller: None,
            }))
        }
        ProxyGetStage::TrapCall => {
            // 10.5.8 requires the trap result to agree with any frozen target
            // property. Proxy targets that are themselves exotic are handled
            // by the internal-method dispatcher as that operation is added;
            // ordinary targets already enforce the complete invariant here.
            if runtime.proxy_state(state.target)?.is_none()
                && let Some(target) = heap_own_property(runtime, state.target, &state.key)?
            {
                match target {
                    OwnProperty::Data { layout, value }
                        if !layout.is_configurable() && layout.writable() == Some(false) =>
                    {
                        if !completion.same_value(&value) {
                            return proxy_abrupt(
                                state.realm,
                                state.origin,
                                "Proxy get trap violated a frozen data property",
                            );
                        }
                    }
                    OwnProperty::Accessor {
                        layout,
                        getter: None,
                        ..
                    } if !layout.is_configurable() => {
                        if !matches!(completion, StoredValue::Undefined) {
                            return proxy_abrupt(
                                state.realm,
                                state.origin,
                                "Proxy get trap violated a getterless property",
                            );
                        }
                    }
                    OwnProperty::Data { .. } | OwnProperty::Accessor { .. } => {}
                }
            }
            Ok(NativeDispatch::Immediate(completion))
        }
    }
}

fn proxy_type_error(
    realm: RealmId,
    origin: JsStackFrame,
    message: &'static str,
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

fn require_proxy_component(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<HeapReference, NativeFailure> {
    value.heap_reference().ok_or_else(|| {
        NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("not an object").unwrap_or_else(|_| JsString::empty()),
            },
            origin: origin.clone(),
        })
    })
}

pub(super) fn begin_proxy_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    mut inputs: CallInputs,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    if inputs.new_target.is_none() {
        return proxy_type_error(realm, origin, "Proxy constructor requires 'new'");
    }
    let target =
        require_proxy_component(inputs.arguments.take_first_or_undefined(), realm, &origin)?;
    let handler =
        require_proxy_component(inputs.arguments.take_first_or_undefined(), realm, &origin)?;
    let constructable = match target {
        HeapReference::Function(function) => function_is_constructor(runtime, function)?,
        HeapReference::Object(_) => false,
    };
    Ok(NativeDispatch::Immediate(runtime.allocate_proxy(
        realm,
        target,
        handler,
        constructable,
    )?))
}

pub(super) fn begin_proxy_revocable(
    runtime: &mut Runtime,
    realm: RealmId,
    mut arguments: CallArguments,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let target = require_proxy_component(arguments.take_first_or_undefined(), realm, &origin)?;
    let handler = require_proxy_component(arguments.take_first_or_undefined(), realm, &origin)?;
    let constructable = match target {
        HeapReference::Function(function) => function_is_constructor(runtime, function)?,
        HeapReference::Object(_) => false,
    };
    let proxy = runtime.allocate_proxy(realm, target, handler, constructable)?;
    let reference = proxy
        .heap_reference()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Proxy allocation did not produce an object",
        })?;
    let revoker = runtime.allocate_proxy_revoker(realm, reference)?;
    let result = runtime.allocate_proxy_revocable_result(realm, proxy, revoker)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
}
