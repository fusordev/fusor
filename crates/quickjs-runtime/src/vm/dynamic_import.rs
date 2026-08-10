//! `import()` evaluation through the host module-loading boundary.
//!
//! This module owns the observable front half of `EvaluateImportCall`:
//! a fresh intrinsic Promise, `ToString(specifier)`, the `options.with` Get,
//! enumerable own import-attribute reads, and rejection of every abrupt
//! completion after the argument expressions have been evaluated. Source Text
//! Module graph loading is deliberately not synthesized here; until a typed
//! module-record loader is installed, the host boundary rejects the Promise.

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
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let promise = runtime.allocate_intrinsic_promise(realm)?;
    let state = DynamicImportContinuation {
        promise,
        options,
        specifier: None,
        stage: DynamicImportStage::AwaitWith,
        realm,
        origin: origin.clone(),
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
    if matches!(state.options, StoredValue::Undefined) {
        return reject_dynamic_import_message(
            runtime,
            &state,
            "dynamic module loading is not configured",
        );
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
            if matches!(completion, StoredValue::Undefined) {
                return reject_dynamic_import_message(
                    runtime,
                    &state,
                    "dynamic module loading is not configured",
                );
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

    if attributes.is_empty() {
        reject_dynamic_import_message(runtime, state, "dynamic module loading is not configured")
    } else {
        // No host module loader is installed yet, so its supported import
        // attribute key set is empty. AllImportAttributesSupported therefore
        // rejects before HostLoadImportedModule.
        reject_dynamic_import_message(runtime, state, "unsupported dynamic import attribute")
    }
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
