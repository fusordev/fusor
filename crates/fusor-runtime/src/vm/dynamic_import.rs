//! `import()` evaluation through the host module-loading boundary.
//!
//! This module owns the observable front half of `EvaluateImportCall`:
//! a fresh intrinsic Promise, `ToString(specifier)`, the `options.with` Get,
//! enumerable own import-attribute reads, and rejection of every abrupt
//! completion after the argument expressions have been evaluated. Once the
//! import attributes are known, `HostLoadImportedModule` crosses the typed
//! host boundary: the runtime parks a `PendingDynamicImport` record (referrer
//! key, specifier, attributes, Promise) and the host later completes it
//! through [`complete_dynamic_import_load`] / [`reject_dynamic_import_load`]
//! (`ContinueDynamicImport` / `FinishDynamicImport`), which link and evaluate
//! the registered graph and settle the Promise with the module namespace
//! object or the corresponding error.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DynamicImportStage {
    AwaitWith,
    AwaitAttributes,
}

pub(super) struct DynamicImportContinuation {
    promise: ObjectId,
    options: StoredValue,
    specifier: Option<JsString>,
    stage: DynamicImportStage,
    realm: RealmId,
    origin: JsStackFrame,
    referrer: Option<crate::ids::ModuleRecordId>,
    /// `import.defer()` phase: the import settles with a deferred namespace
    /// and defers evaluation of the loaded graph.
    deferred: bool,
}

impl DynamicImportContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(u64::from(self.options.heap_reference().is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.promise)));
        trace_stored_value_root(&self.options, mark);
    }
}

pub(super) fn begin_dynamic_import(
    runtime: &mut Runtime,
    specifier: StoredValue,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    referrer: Option<crate::ids::ModuleRecordId>,
    execution_budget: &mut ExecutionBudget,
    deferred: bool,
) -> Result<NativeDispatch, NativeFailure> {
    let promise = runtime.allocate_intrinsic_promise(realm)?;
    let state = DynamicImportContinuation {
        promise,
        options,
        specifier: None,
        stage: DynamicImportStage::AwaitWith,
        realm,
        origin: origin.clone(),
        referrer,
        deferred,
    };
    begin_operator_primitive_conversion(
        runtime,
        specifier,
        OperatorPrimitiveHint::String,
        OperatorPrimitiveTarget::DynamicImportSpecifier(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_dynamic_import_specifier(
    runtime: &mut Runtime,
    mut state: DynamicImportContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let specifier = match operator_primitive_to_string(value, state.realm, &state.origin) {
        Ok(specifier) => specifier,
        Err(NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending)) => {
            return reject_dynamic_import_pending(runtime, &state, pending);
        }
        Err(NativeFailure::Execution(error)) => return Err(NativeFailure::Execution(error)),
    };
    state.specifier = Some(specifier);
    begin_dynamic_import_options(runtime, state, return_to, execution_budget)
}

fn begin_dynamic_import_options(
    runtime: &mut Runtime,
    mut state: DynamicImportContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    // No options argument: the attribute set is empty and the import crosses
    // HostLoadImportedModule immediately.
    if matches!(state.options, StoredValue::Undefined) {
        return park_dynamic_import_load(runtime, &state, Vec::new());
    }
    let Some(reference) = state.options.heap_reference() else {
        return reject_dynamic_import_message(
            runtime,
            &state,
            "dynamic import options must be an object",
        );
    };

    charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
    state.stage = DynamicImportStage::AwaitWith;
    let dispatch = begin_internal_get(
        runtime,
        reference,
        state.options.duplicate(),
        runtime.predefined_property_key(PredefinedAtom::With),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    );
    continue_dynamic_import_after(runtime, dispatch, state, return_to, execution_budget)
}

pub(super) fn advance_dynamic_import(
    runtime: &mut Runtime,
    mut state: DynamicImportContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        DynamicImportStage::AwaitWith => {
            // `options.with === undefined` carries no attributes.
            if matches!(completion, StoredValue::Undefined) {
                return park_dynamic_import_load(runtime, &state, Vec::new());
            }
            if completion.heap_reference().is_none() {
                return reject_dynamic_import_message(
                    runtime,
                    &state,
                    "dynamic import attributes must be an object",
                );
            }
            state.stage = DynamicImportStage::AwaitAttributes;
            let dispatch = begin_enumerable_own_properties(
                runtime,
                state.realm,
                Some(completion),
                EnumerableOwnPropertiesKind::KeyAndValue,
                return_to,
                state.origin.clone(),
                execution_budget,
            );
            continue_dynamic_import_after(runtime, dispatch, state, return_to, execution_budget)
        }
        DynamicImportStage::AwaitAttributes => {
            finish_dynamic_import_attributes(runtime, &state, &completion, execution_budget)
        }
    }
}

fn continue_dynamic_import_after(
    runtime: &mut Runtime,
    dispatch: Result<NativeDispatch, NativeFailure>,
    state: DynamicImportContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let dispatch = match dispatch {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending)) => {
            return reject_dynamic_import_pending(runtime, &state, pending);
        }
        Err(NativeFailure::Execution(error)) => return Err(NativeFailure::Execution(error)),
    };
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_dynamic_import(runtime, state, value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::DynamicImport(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::DynamicImport(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "dynamic import operation produced a structured result",
        }
        .into()),
    }
}

fn finish_dynamic_import_attributes(
    runtime: &mut Runtime,
    state: &DynamicImportContinuation,
    entries: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(entries) = entries else {
        return Err(EngineFault::RuntimeInvariant {
            message: "EnumerableOwnProperties did not return an Array",
        }
        .into());
    };
    let length = runtime
        .array_length(*entries)?
        .ok_or(EngineFault::RuntimeInvariant {
            message: "EnumerableOwnProperties result is not an Array",
        })?;
    execution_budget.charge_instructions(u64::from(length).saturating_add(1))?;
    let mut attributes = Vec::new();
    attributes.try_reserve_exact(length as usize).map_err(|_| {
        ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: length as usize,
        }
    })?;
    for index in 0..length {
        let entry = read_dense_array_value(runtime, *entries, index)?;
        let StoredValue::Object(entry) = entry else {
            return Err(EngineFault::RuntimeInvariant {
                message: "EnumerableOwnProperties entry is not an Array",
            }
            .into());
        };
        let key = read_dense_array_value(runtime, entry, 0)?;
        let value = read_dense_array_value(runtime, entry, 1)?;
        let StoredValue::String(key) = key else {
            return Err(EngineFault::RuntimeInvariant {
                message: "EnumerableOwnProperties key is not a String",
            }
            .into());
        };
        let StoredValue::String(value) = value else {
            return reject_dynamic_import_message(
                runtime,
                state,
                "dynamic import attribute values must be strings",
            );
        };
        attributes.push((key, value));
    }
    attributes.sort_unstable_by(|left, right| left.0.cmp(&right.0));

    // The host module loader supports `type: "json"` and `type: "text"` (and
    // the empty set); every other attribute key is rejected before
    // HostLoadImportedModule (AllImportAttributesSupported).
    let supported = attributes.iter().all(|(key, value)| {
        matches!(key.to_utf8_lossy().ok().as_deref(), Some("type"))
            && matches!(
                value.to_utf8_lossy().ok().as_deref(),
                Some("json") | Some("text")
            )
    });
    if supported {
        park_dynamic_import_load(runtime, state, attributes)
    } else {
        reject_dynamic_import_message(runtime, state, "unsupported dynamic import attribute")
    }
}

fn park_dynamic_import_load(
    runtime: &mut Runtime,
    state: &DynamicImportContinuation,
    attributes: Vec<(JsString, JsString)>,
) -> Result<NativeDispatch, NativeFailure> {
    let referrer = state
        .referrer
        .and_then(|module| runtime.modules.get(module))
        .map(|record| record.key.clone());
    let specifier = state
        .specifier
        .clone()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "dynamic import parking lost its specifier",
        })?;
    runtime
        .park_dynamic_import(
            state.realm,
            referrer,
            specifier,
            attributes,
            state.promise,
            state.deferred,
        )
        .map_err(NativeFailure::Execution)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(
        state.promise,
    )))
}

fn read_dense_array_value(
    runtime: &Runtime,
    array: ObjectId,
    index: u32,
) -> Result<StoredValue, NativeFailure> {
    let index = ArrayIndex::new(index).ok_or(EngineFault::RuntimeInvariant {
        message: "dynamic import attribute array index exceeded the array-index domain",
    })?;
    Ok(read_heap_property(
        runtime,
        HeapReference::Object(array),
        &PropertyKey::from_index(index),
    )?)
}

pub(super) fn resume_dynamic_import_abrupt(
    runtime: &mut Runtime,
    state: &DynamicImportContinuation,
    pending: PendingException,
) -> Result<NativeDispatch, NativeFailure> {
    reject_dynamic_import_pending(runtime, state, pending)
}

fn reject_dynamic_import_message(
    runtime: &mut Runtime,
    state: &DynamicImportContinuation,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    let pending = PendingException {
        realm: state.realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin: state.origin.clone(),
    };
    reject_dynamic_import_pending(runtime, state, pending)
}

fn reject_dynamic_import_pending(
    runtime: &mut Runtime,
    state: &DynamicImportContinuation,
    pending: PendingException,
) -> Result<NativeDispatch, NativeFailure> {
    let reason = pending_exception_value(runtime, pending)?;
    reject_promise(runtime, state.promise, reason)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(
        state.promise,
    )))
}

// ---- Host completion (`ContinueDynamicImport` / `FinishDynamicImport`) ----

/// Completes a parked dynamic `import()` after the host registered the loaded
/// module graph under `root`.
///
/// This is the host-driven `FinishDynamicImport`: the graph is linked and
/// evaluated at completion time. A synchronously evaluated root settles the
/// Promise immediately — evaluation errors reject with the escaping exception,
/// link errors with a `SyntaxError`, and a successful evaluation fulfills with
/// the module namespace exotic object. A root left in the evaluating-async
/// status (top-level await) instead attaches reactions to its
/// [[TopLevelCapability]] and settles the import Promise when the asynchronous
/// evaluation completes. Promise reactions queue as ordinary Promise jobs and
/// drain at the next host-job checkpoint; they never run inline here.
///
/// Only internal runtime failures (allocation, invariant violations) surface
/// as `Err`; every spec-level failure settles the Promise instead.
pub(crate) fn complete_dynamic_import_load(
    runtime: &mut Runtime,
    import: crate::runtime::PendingDynamicImport,
    root: &crate::runtime::ModuleKey,
    limits: ExecutionLimits,
    compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
) -> Result<(), ExecutionError> {
    let record = import.record;
    let realm = record.realm;
    let promise = record.promise;
    let deferred = record.deferred;
    let Some(module) = runtime.registered_module(realm, root) else {
        let message = format!("module '{root}' is not registered");
        return reject_parked_import(runtime, realm, promise, ExceptionKind::TypeError, &message);
    };
    if let Err(error) = crate::runtime::modules::link_module(runtime, realm, module) {
        return reject_parked_import(
            runtime,
            realm,
            promise,
            ExceptionKind::SyntaxError,
            error.message(),
        );
    }
    // ECMA-262 ContinueDynamicImport phase ~defer~: link, then evaluate only
    // the asynchronous transitive dependencies (SafePerformPromiseAll over
    // their Evaluate() promises), and fulfill with the module's deferred
    // namespace object. The module itself stays unevaluated until its
    // namespace is first accessed.
    if deferred {
        let mut async_deps: Vec<crate::ids::ModuleRecordId> = Vec::new();
        let mut async_seen = Vec::new();
        crate::runtime::modules::gather_async_transitive_dependencies(
            runtime,
            module,
            &mut async_seen,
            &mut async_deps,
        );
        let mut capabilities = Vec::new();
        for dep in async_deps {
            match crate::runtime::modules::evaluate_module(runtime, realm, dep, limits, compiler) {
                Ok(()) => {
                    let capability = crate::runtime::modules::module_top_level_capability(
                        runtime, dep,
                    )
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "deferred import async dep lacks its top-level capability",
                    })?;
                    capabilities.push(capability);
                }
                Err(error) => {
                    let reason = module_error_rejection_value(runtime, realm, &error)?;
                    return settle_parked_import(runtime, promise, reason, true);
                }
            }
        }
        if capabilities.is_empty() {
            let namespace =
                crate::runtime::modules::get_or_create_namespace_phase(runtime, module, true)
                    .map_err(|_| EngineFault::RuntimeInvariant {
                        message: "deferred namespace creation failed after load",
                    })?;
            return settle_parked_import(runtime, promise, StoredValue::Object(namespace), false);
        }
        runtime
            .deferred_import_waiters
            .insert(promise, capabilities.len() as u32);
        for capability in capabilities {
            perform_targeted_promise_reactions(
                runtime,
                capability,
                crate::object::PromiseReactionTarget::ImportDeferDeps { promise, module },
            )
            .map_err(native_failure_to_execution)?;
        }
        return Ok(());
    }
    if let Err(error) =
        crate::runtime::modules::evaluate_module(runtime, realm, module, limits, compiler)
    {
        let reason = module_error_rejection_value(runtime, realm, &error)?;
        return settle_parked_import(runtime, promise, reason, true);
    }
    if crate::runtime::modules::module_is_evaluating_async(runtime, module) {
        // The root evaluates asynchronously: settle the import Promise when the
        // module's [[TopLevelCapability]] settles (ECMA-262 FinishDynamicImport
        // waiting on Evaluate()'s returned Promise).
        let capability = crate::runtime::modules::module_top_level_capability(runtime, module)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "evaluating-async module lacks its top-level capability",
            })?;
        return perform_targeted_promise_reactions(
            runtime,
            capability,
            crate::object::PromiseReactionTarget::FinishDynamicImport { promise, module },
        )
        .map_err(native_failure_to_execution);
    }
    let namespace =
        crate::runtime::modules::get_or_create_namespace(runtime, module).map_err(|_| {
            EngineFault::RuntimeInvariant {
                message: "namespace creation failed after successful module evaluation",
            }
        })?;
    settle_parked_import(runtime, promise, StoredValue::Object(namespace), false)
}

/// Rejects a parked dynamic `import()` with a `TypeError` carrying the host's
/// load or resolution failure message.
pub(crate) fn reject_dynamic_import_load(
    runtime: &mut Runtime,
    import: crate::runtime::PendingDynamicImport,
    message: &str,
) -> Result<(), ExecutionError> {
    reject_dynamic_import_load_kind(runtime, import, ExceptionKind::TypeError, message)
}

/// Rejects a parked dynamic `import()` with an engine-created error of `kind`
/// (ECMA-262 `FinishDynamicImport` onRejected): a requested module that failed
/// to parse or compile rejects with a `SyntaxError`, while a host load or
/// resolution failure rejects with a `TypeError`.
pub(crate) fn reject_dynamic_import_load_kind(
    runtime: &mut Runtime,
    import: crate::runtime::PendingDynamicImport,
    kind: ExceptionKind,
    message: &str,
) -> Result<(), ExecutionError> {
    let record = import.record;
    reject_parked_import(runtime, record.realm, record.promise, kind, message)
}

fn reject_parked_import(
    runtime: &mut Runtime,
    realm: RealmId,
    promise: ObjectId,
    kind: ExceptionKind,
    message: &str,
) -> Result<(), ExecutionError> {
    let object =
        runtime.materialize_error_object(realm, kind, JsString::from_utf8(message)?, None)?;
    settle_parked_import(runtime, promise, StoredValue::Object(object), true)
}

/// Builds the rejection value for a module link/evaluation failure.
///
/// An asynchronous evaluation failure rejects with its preserved rejection
/// value; an escaping JavaScript exception rejects with the original thrown
/// value; an engine-created error is re-materialized with its kind and
/// message; a phase-only error falls back to `SyntaxError` (link) or
/// `TypeError` (evaluation).
pub(crate) fn module_error_rejection_value(
    runtime: &mut Runtime,
    realm: RealmId,
    error: &crate::ModuleError,
) -> Result<StoredValue, ExecutionError> {
    if let Some(value) = error.rejection_value() {
        return Ok(value.stored()?.duplicate());
    }
    if let Some(exception) = error.exception() {
        if let Some(value) = exception.thrown_value() {
            return Ok(value.stored()?.duplicate());
        }
        if let (Some(kind), Some(message)) = (exception.kind(), exception.message()) {
            let object = runtime.materialize_error_object(realm, kind, message.clone(), None)?;
            return Ok(StoredValue::Object(object));
        }
    }
    let kind = match error.phase() {
        crate::ModuleErrorPhase::Link => ExceptionKind::SyntaxError,
        crate::ModuleErrorPhase::Evaluate => ExceptionKind::TypeError,
    };
    let object = runtime.materialize_error_object(
        realm,
        kind,
        JsString::from_utf8(error.message())?,
        None,
    )?;
    Ok(StoredValue::Object(object))
}

fn settle_parked_import(
    runtime: &mut Runtime,
    promise: ObjectId,
    value: StoredValue,
    reject: bool,
) -> Result<(), ExecutionError> {
    let settlement = if reject {
        reject_promise(runtime, promise, value)
    } else {
        fulfill_promise(runtime, promise, value)
    };
    settlement.map_err(native_failure_to_execution)
}

fn native_failure_to_execution(failure: NativeFailure) -> ExecutionError {
    match failure {
        NativeFailure::Execution(error) => error,
        NativeFailure::Abrupt(_) | NativeFailure::AbruptAfterTransient(_) => {
            EngineFault::RuntimeInvariant {
                message: "dynamic import promise settlement completed abruptly",
            }
            .into()
        }
    }
}
