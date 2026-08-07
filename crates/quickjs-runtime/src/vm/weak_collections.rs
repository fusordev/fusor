/*
 * JavaScript weak collection semantics derived from QuickJS.
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

//! Branded `WeakMap` and `WeakSet` operations with `CanBeHeldWeakly` validation.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use crate::object::{WeakKey, WeakMapState, WeakSetState};

pub(super) fn dispatch_weak_map_method(
    runtime: &mut Runtime,
    method: WeakMapMethod,
    receiver: StoredValue,
    mut arguments: CallArguments,
    context: MapMethodContext,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let MapMethodContext {
        realm,
        return_to,
        origin,
    } = context;
    let map = require_weak_map(runtime, &receiver, realm, &origin)?;
    execution_budget.charge_instructions(1)?;
    match method {
        WeakMapMethod::Set => {
            let key = arguments.take_first_or_undefined();
            require_weak_key(
                &key,
                realm,
                origin.clone(),
                "invalid value used as WeakMap key",
            )?;
            let value = arguments.take_first_or_undefined();
            runtime.weak_map_set(map, &key, value)?;
            Ok(NativeDispatch::Immediate(receiver))
        }
        WeakMapMethod::Get => {
            let key = arguments.take_first_or_undefined();
            if WeakKey::from_value(&key).is_none() {
                return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
            }
            let value = weak_map_state(runtime, map)?
                .get(&key)
                .map_or(StoredValue::Undefined, StoredValue::duplicate);
            Ok(NativeDispatch::Immediate(value))
        }
        WeakMapMethod::Has => {
            let key = arguments.take_first_or_undefined();
            let present = WeakKey::from_value(&key).is_some()
                && weak_map_state(runtime, map)?.contains_key(&key);
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(present)))
        }
        WeakMapMethod::Delete => {
            let key = arguments.take_first_or_undefined();
            if WeakKey::from_value(&key).is_none() {
                return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
            }
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(
                runtime.weak_map_delete(map, &key)?,
            )))
        }
        WeakMapMethod::GetOrInsert => {
            let key = arguments.take_first_or_undefined();
            require_weak_key(
                &key,
                realm,
                origin.clone(),
                "invalid value used as WeakMap key",
            )?;
            if let Some(value) = weak_map_state(runtime, map)?.get(&key) {
                return Ok(NativeDispatch::Immediate(value.duplicate()));
            }
            let value = arguments.take_first_or_undefined();
            runtime.weak_map_set(map, &key, value.duplicate())?;
            Ok(NativeDispatch::Immediate(value))
        }
        WeakMapMethod::GetOrInsertComputed => {
            let key = arguments.take_first_or_undefined();
            require_weak_key(
                &key,
                realm,
                origin.clone(),
                "invalid value used as WeakMap key",
            )?;
            let callback = arguments.take_first_or_undefined();
            let StoredValue::Function(callback) = callback else {
                return weak_collection_type_error(realm, origin, "not a function");
            };
            if let Some(value) = weak_map_state(runtime, map)?.get(&key) {
                return Ok(NativeDispatch::Immediate(value.duplicate()));
            }
            begin_map_computed_call(
                MapCollectionKind::WeakMap,
                map,
                key,
                callback,
                origin,
                return_to,
            )
        }
    }
}

pub(super) fn dispatch_weak_set_method(
    runtime: &mut Runtime,
    method: WeakSetMethod,
    receiver: StoredValue,
    mut arguments: CallArguments,
    context: SetMethodContext,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let SetMethodContext {
        realm,
        return_to: _,
        origin,
    } = context;
    let set = require_weak_set(runtime, &receiver, realm, &origin)?;
    execution_budget.charge_instructions(1)?;
    let value = arguments.take_first_or_undefined();
    match method {
        WeakSetMethod::Add => {
            require_weak_key(&value, realm, origin, "invalid value used as WeakSet key")?;
            runtime.weak_set_add(set, &value)?;
            Ok(NativeDispatch::Immediate(receiver))
        }
        WeakSetMethod::Has => {
            let present = WeakKey::from_value(&value).is_some()
                && weak_set_state(runtime, set)?.contains(&value);
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(present)))
        }
        WeakSetMethod::Delete => {
            if WeakKey::from_value(&value).is_none() {
                return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
            }
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(
                runtime.weak_set_delete(set, &value)?,
            )))
        }
    }
}

fn require_weak_key(
    value: &StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<(), NativeFailure> {
    if WeakKey::from_value(value).is_some() {
        return Ok(());
    }
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}

fn require_weak_map(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<ObjectId, NativeFailure> {
    let StoredValue::Object(map) = receiver else {
        return weak_collection_brand_error(realm, origin.clone(), "not a WeakMap object");
    };
    let object = runtime
        .objects
        .get(*map)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "WeakMap object",
            index: map.index(),
            generation: map.generation(),
        })?;
    if object.weak_map_state().is_none() {
        return weak_collection_brand_error(realm, origin.clone(), "not a WeakMap object");
    }
    Ok(*map)
}

fn require_weak_set(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<ObjectId, NativeFailure> {
    let StoredValue::Object(set) = receiver else {
        return weak_collection_brand_error(realm, origin.clone(), "not a WeakSet object");
    };
    let object = runtime
        .objects
        .get(*set)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "WeakSet object",
            index: set.index(),
            generation: set.generation(),
        })?;
    if object.weak_set_state().is_none() {
        return weak_collection_brand_error(realm, origin.clone(), "not a WeakSet object");
    }
    Ok(*set)
}

fn weak_map_state(runtime: &Runtime, map: ObjectId) -> Result<&WeakMapState, NativeFailure> {
    runtime
        .objects
        .get(map)
        .and_then(crate::object::HeapObject::weak_map_state)
        .ok_or_else(|| {
            EngineFault::StaleHeapEdge {
                edge: "WeakMap object",
                index: map.index(),
                generation: map.generation(),
            }
            .into()
        })
}

fn weak_set_state(runtime: &Runtime, set: ObjectId) -> Result<&WeakSetState, NativeFailure> {
    runtime
        .objects
        .get(set)
        .and_then(crate::object::HeapObject::weak_set_state)
        .ok_or_else(|| {
            EngineFault::StaleHeapEdge {
                edge: "WeakSet object",
                index: set.index(),
                generation: set.generation(),
            }
            .into()
        })
}

fn weak_collection_brand_error<T>(
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

fn weak_collection_type_error(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    weak_collection_brand_error(realm, origin, message)
}
