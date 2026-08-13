//! Host-driven ECMAScript property internal methods.
//!
//! The embedding property API ([`crate::Object::get`] and friends) enters the
//! engine here. Every operation runs through the same dispatch machinery the
//! interpreter uses, so getters, setters, proxies, and
//! `ValidateAndApplyPropertyDescriptor` behave exactly as they do for
//! JavaScript code. Operations execute under [`ExecutionLimits::default()`]
//! with no dynamic-function compiler available.

use std::sync::Arc;

use super::*;

/// Constructs the stack-frame origin attributed to host property operations.
///
/// Engine-raised `TypeError`s (for example a failed write to a non-writable
/// property) report this frame as their source.
fn host_property_origin(operation: &'static str) -> JsStackFrame {
    JsStackFrame::new(
        FunctionTemplateId::new(0),
        BytecodePc::ZERO,
        Arc::from("<host property access>"),
        Arc::from(operation),
        SourceByteSpan::new(0, 0),
    )
}

/// Converts an internal dispatch failure into a structured execution error,
/// materializing engine-raised exceptions exactly like an interpreter entry
/// point does.
fn execution_from_native_failure(
    runtime: &mut Runtime,
    failure: NativeFailure,
) -> ExecutionError {
    match failure {
        NativeFailure::Execution(error) => error,
        NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending) => {
            match finish_exception(runtime, pending, Vec::new()) {
                Ok(exception) => ExecutionError::Exception(exception),
                Err(error) => error,
            }
        }
    }
}

/// Executes ECMA-262 `Get(O, P)` with receiver `O` and returns the rooted
/// stored completion value.
pub(crate) fn host_get_property(
    runtime: &mut Runtime,
    reference: HeapReference,
    key: PropertyKey,
    realm: RealmId,
) -> Result<StoredValue, ExecutionError> {
    let mut execution_budget = ExecutionBudget::new(ExecutionLimits::default());
    let origin = host_property_origin("get");
    let receiver = proxy_reference_value(reference);
    let dispatch = begin_internal_get(
        runtime,
        reference,
        receiver,
        key,
        realm,
        None,
        origin,
        &mut execution_budget,
    )
    .map_err(|failure| execution_from_native_failure(runtime, failure))?;
    let dispatch =
        resolve_native_dispatch(runtime, dispatch, &[], 0, 0, None, &mut execution_budget);
    execute_root_dispatch_with_budget(
        runtime,
        dispatch,
        Vec::new(),
        None,
        &mut execution_budget,
    )
}

/// Executes ECMA-262 `Set(O, P, V, O)` with strict semantics: a failed write
/// (non-writable property, absent setter, Proxy trap rejection) raises a
/// `TypeError` instead of failing silently.
pub(crate) fn host_set_property(
    runtime: &mut Runtime,
    reference: HeapReference,
    key: PropertyKey,
    value: StoredValue,
    realm: RealmId,
) -> Result<(), ExecutionError> {
    let mut execution_budget = ExecutionBudget::new(ExecutionLimits::default());
    let origin = host_property_origin("set");
    let name = property_key_name(&key).unwrap_or_else(JsString::empty);
    let receiver = proxy_reference_value(reference);
    let dispatch = begin_internal_set(
        runtime,
        reference,
        key,
        name,
        value,
        receiver,
        true,
        false,
        realm,
        None,
        origin,
        &mut execution_budget,
    )
    .map_err(|failure| execution_from_native_failure(runtime, failure))?;
    let dispatch =
        resolve_native_dispatch(runtime, dispatch, &[], 0, 0, None, &mut execution_budget);
    execute_root_dispatch_with_budget(
        runtime,
        dispatch,
        Vec::new(),
        None,
        &mut execution_budget,
    )
    .map(|_completion| ())
}

/// Executes ECMA-262 `HasProperty(O, P)` and returns the Boolean result.
pub(crate) fn host_has_property(
    runtime: &mut Runtime,
    reference: HeapReference,
    key: PropertyKey,
    realm: RealmId,
) -> Result<bool, ExecutionError> {
    let mut execution_budget = ExecutionBudget::new(ExecutionLimits::default());
    let origin = host_property_origin("has");
    let dispatch = begin_internal_has(
        runtime,
        reference,
        key,
        realm,
        None,
        origin,
        &mut execution_budget,
    )
    .map_err(|failure| execution_from_native_failure(runtime, failure))?;
    let dispatch =
        resolve_native_dispatch(runtime, dispatch, &[], 0, 0, None, &mut execution_budget);
    let completion = execute_root_dispatch_with_budget(
        runtime,
        dispatch,
        Vec::new(),
        None,
        &mut execution_budget,
    )?;
    let StoredValue::Boolean(result) = completion else {
        return Err(EngineFault::RuntimeInvariant {
            message: "[[HasProperty]] completed with a non-Boolean value",
        }
        .into());
    };
    Ok(result)
}

/// Executes ECMA-262 `O.[[Delete]](P)` and returns the Boolean result: `false`
/// reports a non-configurable property (or a rejected Proxy trap) without
/// raising, matching `Reflect.deleteProperty` semantics.
pub(crate) fn host_delete_property(
    runtime: &mut Runtime,
    reference: HeapReference,
    key: PropertyKey,
    realm: RealmId,
) -> Result<bool, ExecutionError> {
    let mut execution_budget = ExecutionBudget::new(ExecutionLimits::default());
    let origin = host_property_origin("delete");
    let dispatch = begin_internal_delete(
        runtime,
        reference,
        key,
        false,
        true,
        realm,
        None,
        origin,
        &mut execution_budget,
    )
    .map_err(|failure| execution_from_native_failure(runtime, failure))?;
    let dispatch =
        resolve_native_dispatch(runtime, dispatch, &[], 0, 0, None, &mut execution_budget);
    let completion = execute_root_dispatch_with_budget(
        runtime,
        dispatch,
        Vec::new(),
        None,
        &mut execution_budget,
    )?;
    let StoredValue::Boolean(result) = completion else {
        return Err(EngineFault::RuntimeInvariant {
            message: "[[Delete]] completed with a non-Boolean value",
        }
        .into());
    };
    Ok(result)
}

/// Executes ECMA-262 `O.[[DefineOwnProperty]](P, desc)` and returns the
/// Boolean result: `false` reports a rejected definition (non-configurable
/// incompatibility, non-extensible creation, or a `false` Proxy trap result).
///
/// The descriptor fields are host-supplied; getter and setter callability is
/// validated exactly like the JavaScript `Object.defineProperty` descriptor
/// read (non-callable, non-`undefined` accessors raise a `TypeError`).
#[allow(
    clippy::too_many_arguments,
    reason = "the host descriptor carries every ValidateAndApplyPropertyDescriptor field explicitly"
)]
pub(crate) fn host_define_own_property(
    runtime: &mut Runtime,
    reference: HeapReference,
    key: PropertyKey,
    value: Option<StoredValue>,
    writable: Option<bool>,
    get: Option<StoredValue>,
    set: Option<StoredValue>,
    enumerable: Option<bool>,
    configurable: Option<bool>,
    realm: RealmId,
) -> Result<bool, ExecutionError> {
    let mut execution_budget = ExecutionBudget::new(ExecutionLimits::default());
    let origin = host_property_origin("define");
    let definition = host_property_definition(
        value,
        writable,
        get,
        set,
        enumerable,
        configurable,
        realm,
        &origin,
    )
    .map_err(|failure| execution_from_native_failure(runtime, failure))?;
    let dispatch = begin_internal_define_own_property(
        runtime,
        reference,
        key,
        definition,
        realm,
        None,
        origin,
        &mut execution_budget,
        DefinePropertyResult::Boolean,
    )
    .map_err(|failure| execution_from_native_failure(runtime, failure))?;
    let dispatch =
        resolve_native_dispatch(runtime, dispatch, &[], 0, 0, None, &mut execution_budget);
    let completion = execute_root_dispatch_with_budget(
        runtime,
        dispatch,
        Vec::new(),
        None,
        &mut execution_budget,
    )?;
    let StoredValue::Boolean(result) = completion else {
        return Err(EngineFault::RuntimeInvariant {
            message: "[[DefineOwnProperty]] completed with a non-Boolean value",
        }
        .into());
    };
    Ok(result)
}

/// Executes ECMA-262 `O.[[OwnPropertyKeys]]()` and returns the keys in
/// specification order (integer indices ascending, then string keys and
/// symbol keys in creation order; a Proxy reports its validated trap order).
pub(crate) fn host_own_property_keys(
    runtime: &mut Runtime,
    reference: HeapReference,
    realm: RealmId,
) -> Result<Vec<PropertyKey>, ExecutionError> {
    let mut execution_budget = ExecutionBudget::new(ExecutionLimits::default());
    let origin = host_property_origin("ownKeys");
    if runtime.proxy_state(reference)?.is_none() {
        // Fast path for ordinary objects: snapshot the key list directly
        // without materializing (and reading back) a JavaScript Array.
        if let HeapReference::Object(object) = reference
            && runtime.module_namespace_is_deferred(object)
        {
            // ECMA-262 10.4.6.6 [[OwnPropertyKeys]] step 1: the exports list
            // triggers deferred evaluation for every key kind.
            if let Err(failure) = runtime.ensure_deferred_namespace_evaluation(object, None) {
                let abrupt = match deferred_namespace_evaluation_abrupt(realm, origin, failure) {
                    Ok(abrupt) => abrupt,
                    Err(abrupt) => abrupt,
                };
                return Err(execution_from_native_failure(runtime, abrupt));
            }
        }
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
                        message: "host own-key snapshot shrank during collection",
                    })?
                    .key()
                    .clone(),
            );
        }
        return Ok(keys);
    }
    let dispatch = begin_internal_own_keys(
        runtime,
        reference,
        realm,
        None,
        origin,
        &mut execution_budget,
    )
    .map_err(|failure| execution_from_native_failure(runtime, failure))?;
    let dispatch =
        resolve_native_dispatch(runtime, dispatch, &[], 0, 0, None, &mut execution_budget);
    let completion = execute_root_dispatch_with_budget(
        runtime,
        dispatch,
        Vec::new(),
        None,
        &mut execution_budget,
    )?;
    generated_key_list(runtime, completion)
        .map_err(|failure| execution_from_native_failure(runtime, failure))
}
