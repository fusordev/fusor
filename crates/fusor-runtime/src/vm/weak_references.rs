/*
 * JavaScript weak-reference semantics derived from QuickJS.
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

//! Branded weak-reference operations and resumable constructors.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use crate::object::{HeapObject, WeakKey};

pub(super) fn begin_weak_ref_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: Option<FunctionId>,
    target: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = new_target else {
        return weak_reference_type_error(realm, origin, "constructor requires 'new'");
    };
    if WeakKey::from_value(&target).is_none() {
        return weak_reference_type_error(realm, origin, "invalid target");
    }
    begin_weak_reference_prototype_get(
        runtime,
        realm,
        new_target,
        IntrinsicGetContinuation::WeakRefConstructor { new_target, target },
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn begin_finalization_registry_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: Option<FunctionId>,
    cleanup_callback: &StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = new_target else {
        return weak_reference_type_error(realm, origin, "constructor requires 'new'");
    };
    let StoredValue::Function(cleanup_callback) = cleanup_callback else {
        return weak_reference_type_error(realm, origin, "argument must be a function");
    };
    begin_weak_reference_prototype_get(
        runtime,
        realm,
        new_target,
        IntrinsicGetContinuation::FinalizationRegistryConstructor {
            realm,
            new_target,
            cleanup_callback: *cleanup_callback,
        },
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_weak_reference_prototype_get(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: FunctionId,
    continuation: IntrinsicGetContinuation,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let receiver = StoredValue::Function(new_target);
    charge_heap_property_lookup(runtime, &receiver, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let dispatch = begin_internal_get(
        runtime,
        HeapReference::Function(new_target),
        receiver,
        key,
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    continue_intrinsic_get_after(runtime, dispatch, continuation, return_to, execution_budget)
}

pub(super) fn finish_weak_reference_constructor_get(
    runtime: &mut Runtime,
    continuation: IntrinsicGetContinuation,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    match continuation {
        IntrinsicGetContinuation::WeakRefConstructor { new_target, target } => {
            let prototype = weak_reference_prototype(
                runtime,
                new_target,
                requested,
                WeakReferenceConstructorKind::WeakRef,
            )?;
            runtime.keep_alive(target.duplicate())?;
            let object = runtime.allocate_weak_ref_object(prototype, &target)?;
            Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
        }
        IntrinsicGetContinuation::FinalizationRegistryConstructor {
            realm,
            new_target,
            cleanup_callback,
        } => {
            let prototype = weak_reference_prototype(
                runtime,
                new_target,
                requested,
                WeakReferenceConstructorKind::FinalizationRegistry,
            )?;
            let object = runtime.allocate_finalization_registry_object(
                prototype,
                realm,
                cleanup_callback,
            )?;
            Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
        }
        _ => Err(EngineFault::RuntimeInvariant {
            message: "non-weak-reference continuation reached weak-reference constructor finish",
        }
        .into()),
    }
}

#[derive(Clone, Copy)]
enum WeakReferenceConstructorKind {
    WeakRef,
    FinalizationRegistry,
}

fn weak_reference_prototype(
    runtime: &Runtime,
    new_target: FunctionId,
    requested: &StoredValue,
    kind: WeakReferenceConstructorKind,
) -> Result<HeapReference, NativeFailure> {
    Ok(match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        _ => {
            let target_realm = runtime.function_realm(new_target)?;
            HeapReference::Object(match kind {
                WeakReferenceConstructorKind::WeakRef => {
                    runtime.realm_weak_ref_prototype(target_realm)?
                }
                WeakReferenceConstructorKind::FinalizationRegistry => {
                    runtime.realm_finalization_registry_prototype(target_realm)?
                }
            })
        }
    })
}

pub(super) fn dispatch_weak_ref_deref(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget.charge_instructions(1)?;
    let weak_ref = require_weak_ref(runtime, receiver, realm, origin)?;
    let target = runtime
        .objects
        .get(weak_ref)
        .and_then(HeapObject::weak_ref_state)
        .and_then(|state| state.target())
        .and_then(|target| runtime.live_weak_target(target));
    let Some(target) = target else {
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    };
    runtime.keep_alive(target.duplicate())?;
    Ok(NativeDispatch::Immediate(target))
}

pub(super) fn dispatch_finalization_registry_method(
    runtime: &mut Runtime,
    method: FinalizationRegistryMethod,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    realm: RealmId,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget.charge_instructions(1)?;
    let registry = require_finalization_registry(runtime, receiver, realm, &origin)?;
    match method {
        FinalizationRegistryMethod::Register => {
            let target = arguments.take_first_or_undefined();
            if WeakKey::from_value(&target).is_none() {
                return weak_reference_type_error(realm, origin, "invalid target");
            }
            let held_value = arguments.take_first_or_undefined();
            if target.same_value(&held_value) {
                return weak_reference_type_error(realm, origin, "held value cannot be the target");
            }
            let unregister_token = arguments.take_first_or_undefined();
            let unregister_token = if matches!(unregister_token, StoredValue::Undefined) {
                None
            } else if WeakKey::from_value(&unregister_token).is_some() {
                Some(unregister_token)
            } else {
                return weak_reference_type_error(realm, origin, "invalid unregister token");
            };
            runtime.finalization_registry_register(
                registry,
                &target,
                held_value,
                unregister_token.as_ref(),
            )?;
            Ok(NativeDispatch::Immediate(StoredValue::Undefined))
        }
        FinalizationRegistryMethod::Unregister => {
            let token = arguments.take_first_or_undefined();
            if WeakKey::from_value(&token).is_none() {
                return weak_reference_type_error(realm, origin, "invalid unregister token");
            }
            let removed = runtime.finalization_registry_unregister(registry, &token)?;
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(removed)))
        }
    }
}

fn require_weak_ref(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<ObjectId, NativeFailure> {
    let StoredValue::Object(object) = receiver else {
        return weak_reference_type_error(realm, origin.clone(), "WeakRef object expected");
    };
    if runtime
        .objects
        .get(*object)
        .and_then(HeapObject::weak_ref_state)
        .is_none()
    {
        return weak_reference_type_error(realm, origin.clone(), "WeakRef object expected");
    }
    Ok(*object)
}

fn require_finalization_registry(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<ObjectId, NativeFailure> {
    let StoredValue::Object(object) = receiver else {
        return weak_reference_type_error(
            realm,
            origin.clone(),
            "FinalizationRegistry object expected",
        );
    };
    if runtime
        .objects
        .get(*object)
        .and_then(HeapObject::finalization_registry_state)
        .is_none()
    {
        return weak_reference_type_error(
            realm,
            origin.clone(),
            "FinalizationRegistry object expected",
        );
    }
    Ok(*object)
}

fn weak_reference_type_error<T>(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
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
