/*
 * JavaScript Object constructor semantics derived from QuickJS.
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

//! The `Object` constructor and its reflection statics.
//!
//! Only operations the current profile can honor completely are installed.
//! Anything absent stays absent so it fails closed as a missing property
//! rather than behaving incorrectly.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// How a static reflection method treats a primitive argument.
///
/// ECMAScript 2015 relaxed most `Object` statics to accept primitives and
/// answer as though the argument had been wrapped, which the pinned oracle
/// implements too: `Object.isExtensible(5)` is `false` and
/// `Object.keys(5)` is empty rather than a `TypeError`.
#[derive(Clone, Copy)]
enum PrimitivePolicy {
    /// Return the argument unchanged, as `preventExtensions` and
    /// `setPrototypeOf` do.
    ReturnArgument,
    /// Answer as though the primitive were a frozen, non-extensible wrapper.
    TreatAsSealed,
    /// Answer with no own keys.
    NoKeys,
    /// Answer with the intrinsic prototype the primitive's wrapper would
    /// inherit, as `getPrototypeOf` does.
    PrototypeLookup,
}

/// Resolves a static reflection method's argument to a heap reference.
///
/// Returns `Ok(None)` when the argument is a primitive that the policy answers
/// without touching the heap.
fn reflection_target(
    runtime: &mut Runtime,
    realm: RealmId,
    value: &StoredValue,
    policy: PrimitivePolicy,
    origin: Option<&JsStackFrame>,
    method: &str,
) -> Result<Option<HeapReference>, NativeFailure> {
    match value {
        StoredValue::Function(function) => Ok(Some(HeapReference::Function(*function))),
        StoredValue::Object(object) => Ok(Some(HeapReference::Object(*object))),
        StoredValue::Undefined | StoredValue::Null => match policy {
            // Even the permissive statics reject `null` and `undefined`,
            // because `ToObject` fails for them.
            PrimitivePolicy::PrototypeLookup
            | PrimitivePolicy::TreatAsSealed
            | PrimitivePolicy::NoKeys
            | PrimitivePolicy::ReturnArgument => Err(nullish_reflection_failure(
                realm, origin, method, value, policy,
            )?),
        },
        StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let _ = runtime;
            match policy {
                PrimitivePolicy::PrototypeLookup
                | PrimitivePolicy::ReturnArgument
                | PrimitivePolicy::TreatAsSealed
                | PrimitivePolicy::NoKeys => Ok(None),
            }
        }
    }
}

/// Builds the failure for a `null` or `undefined` reflection argument.
///
/// `Object.getPrototypeOf(null)` reports `not an object`, while
/// `Object.keys(null)` reports the `ToObject` failure `cannot convert to
/// object`. The pinned oracle distinguishes the two.
fn nullish_reflection_failure(
    realm: RealmId,
    origin: Option<&JsStackFrame>,
    method: &str,
    value: &StoredValue,
    policy: PrimitivePolicy,
) -> Result<NativeFailure, NativeFailure> {
    let _ = value;
    let message = match policy {
        PrimitivePolicy::NoKeys => "cannot convert to object",
        PrimitivePolicy::PrototypeLookup
        | PrimitivePolicy::ReturnArgument
        | PrimitivePolicy::TreatAsSealed => "not an object",
    };
    Ok(NativeFailure::Abrupt(type_error(
        realm, origin, method, message,
    )?))
}

fn type_error(
    realm: RealmId,
    origin: Option<&JsStackFrame>,
    method: &str,
    message: &str,
) -> Result<PendingException, NativeFailure> {
    let Some(origin) = origin else {
        return Err(NativeFailure::Execution(
            EngineFault::RuntimeInvariant {
                message: "host Object reflection error has no source origin",
            }
            .into(),
        ));
    };
    let _ = method;
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    })
}

/// `Object(value)` and `new Object(value)`.
///
/// A `null` or `undefined` argument produces a fresh ordinary object; every
/// other value is coerced with `ToObject`, so an object argument is returned
/// unchanged (`quickjs.c:39790-39812`).
pub(super) fn object_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
) -> Result<NativeDispatch, NativeFailure> {
    let value = argument.unwrap_or(StoredValue::Undefined);
    let object = match value {
        StoredValue::Undefined | StoredValue::Null => StoredValue::Object(
            runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?,
        ),
        value @ (StoredValue::Function(_) | StoredValue::Object(_)) => value,
        StoredValue::Boolean(value) => {
            StoredValue::Object(runtime.allocate_boxed_boolean(realm, value)?)
        }
        StoredValue::BigInt(value) => {
            StoredValue::Object(runtime.allocate_boxed_bigint(realm, value)?)
        }
        StoredValue::Number(value) => {
            StoredValue::Object(runtime.allocate_boxed_number(realm, value)?)
        }
        StoredValue::String(value) => {
            StoredValue::Object(runtime.allocate_boxed_string(realm, value)?)
        }
        StoredValue::Symbol(value) => {
            StoredValue::Object(runtime.allocate_boxed_symbol(realm, value)?)
        }
    };
    Ok(NativeDispatch::Immediate(object))
}

/// `Object.getPrototypeOf(target)`.
pub(super) fn get_prototype_of(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    origin: Option<&JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let value = argument.unwrap_or(StoredValue::Undefined);
    // A primitive answers with its wrapper's prototype, which is the intrinsic
    // prototype for its type.
    let Some(reference) = reflection_target(
        runtime,
        realm,
        &value,
        PrimitivePolicy::PrototypeLookup,
        origin,
        "getPrototypeOf",
    )?
    else {
        let prototype = primitive_prototype(runtime, realm, &value)?;
        return Ok(NativeDispatch::Immediate(prototype));
    };
    let prototype = runtime.object_record(reference)?.prototype();
    Ok(NativeDispatch::Immediate(heap_reference_value(prototype)))
}

/// Returns the intrinsic prototype a primitive's wrapper would inherit.
fn primitive_prototype(
    runtime: &Runtime,
    realm: RealmId,
    value: &StoredValue,
) -> Result<StoredValue, NativeFailure> {
    let prototype = match value {
        StoredValue::Boolean(_) => HeapReference::Object(runtime.realm_boolean_prototype(realm)?),
        StoredValue::Number(_) => HeapReference::Object(runtime.realm_number_prototype(realm)?),
        StoredValue::BigInt(_) => HeapReference::Object(runtime.realm_bigint_prototype(realm)?),
        StoredValue::String(_) => HeapReference::Object(runtime.realm_string_prototype(realm)?),
        StoredValue::Symbol(_) => HeapReference::Object(runtime.realm_symbol_prototype(realm)?),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Function(_)
        | StoredValue::Object(_) => {
            return Err(NativeFailure::Execution(
                EngineFault::RuntimeInvariant {
                    message: "primitive prototype lookup received a non-primitive",
                }
                .into(),
            ));
        }
    };
    Ok(heap_reference_value(Some(prototype)))
}

fn heap_reference_value(reference: Option<HeapReference>) -> StoredValue {
    match reference {
        None => StoredValue::Null,
        Some(HeapReference::Function(function)) => StoredValue::Function(function),
        Some(HeapReference::Object(object)) => StoredValue::Object(object),
    }
}

/// `Object.setPrototypeOf(target, prototype)`.
///
/// The target is returned unchanged. A primitive target is a no-op, while a
/// prototype that is neither an object nor `null` is a `TypeError`.
pub(super) fn set_prototype_of(
    runtime: &mut Runtime,
    realm: RealmId,
    mut arguments: CallArguments,
    origin: Option<&JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let target = arguments.take_first_or_undefined();
    let requested = arguments.take_first_or_undefined();
    let prototype = match requested {
        StoredValue::Null => None,
        StoredValue::Function(function) => Some(HeapReference::Function(function)),
        StoredValue::Object(object) => Some(HeapReference::Object(object)),
        StoredValue::Undefined
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Err(NativeFailure::Abrupt(type_error(
                realm,
                origin,
                "setPrototypeOf",
                "not an object",
            )?));
        }
    };
    let Some(reference) = reflection_target(
        runtime,
        realm,
        &target,
        PrimitivePolicy::ReturnArgument,
        origin,
        "setPrototypeOf",
    )?
    else {
        return Ok(NativeDispatch::Immediate(target));
    };
    match runtime.set_prototype_of(reference, prototype)? {
        SetPrototypeOutcome::Complete => Ok(NativeDispatch::Immediate(target)),
        SetPrototypeOutcome::NonExtensible => Err(NativeFailure::Abrupt(type_error(
            realm,
            origin,
            "setPrototypeOf",
            "object is not extensible",
        )?)),
        SetPrototypeOutcome::CyclicPrototype => Err(NativeFailure::Abrupt(type_error(
            realm,
            origin,
            "setPrototypeOf",
            "circular prototype chain",
        )?)),
    }
}

/// `Object.seal(target)` and `Object.freeze(target)`.
pub(super) fn set_integrity_level(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    level: IntegrityLevel,
    origin: Option<&JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    let Some(reference) = reflection_target(
        runtime,
        realm,
        &target,
        PrimitivePolicy::ReturnArgument,
        origin,
        "seal",
    )?
    else {
        return Ok(NativeDispatch::Immediate(target));
    };
    runtime.set_integrity_level(reference, level)?;
    Ok(NativeDispatch::Immediate(target))
}

/// `Object.isSealed(target)` and `Object.isFrozen(target)`.
///
/// A primitive is vacuously sealed and frozen, which the oracle reports as
/// `Object.isFrozen(5) === true`.
pub(super) fn test_integrity_level(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    level: IntegrityLevel,
    origin: Option<&JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    let Some(reference) = reflection_target(
        runtime,
        realm,
        &target,
        PrimitivePolicy::TreatAsSealed,
        origin,
        "isSealed",
    )?
    else {
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)));
    };
    Ok(NativeDispatch::Immediate(StoredValue::Boolean(
        runtime.tests_integrity_level(reference, level)?,
    )))
}

/// `Object.preventExtensions(target)`.
pub(super) fn prevent_extensions(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    origin: Option<&JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    let Some(reference) = reflection_target(
        runtime,
        realm,
        &target,
        PrimitivePolicy::ReturnArgument,
        origin,
        "preventExtensions",
    )?
    else {
        return Ok(NativeDispatch::Immediate(target));
    };
    runtime.prevent_extensions(reference)?;
    Ok(NativeDispatch::Immediate(target))
}

/// `Object.isExtensible(target)`.
///
/// A primitive is never extensible, which the oracle reports as
/// `Object.isExtensible(5) === false`.
pub(super) fn is_extensible(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    origin: Option<&JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    let Some(reference) = reflection_target(
        runtime,
        realm,
        &target,
        PrimitivePolicy::TreatAsSealed,
        origin,
        "isExtensible",
    )?
    else {
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
    };
    Ok(NativeDispatch::Immediate(StoredValue::Boolean(
        runtime.is_extensible(reference)?,
    )))
}

/// Whether a key listing reports only enumerable properties.
#[derive(Clone, Copy)]
pub(super) enum KeyListing {
    /// `Object.keys`: own enumerable string-keyed properties.
    EnumerableOnly,
    /// `Object.getOwnPropertyNames`: every own string-keyed property.
    AllStringKeys,
}

/// `Object.keys(target)` and `Object.getOwnPropertyNames(target)`.
pub(super) fn own_property_names(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    listing: KeyListing,
    origin: Option<&JsStackFrame>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    let Some(reference) = reflection_target(
        runtime,
        realm,
        &target,
        PrimitivePolicy::NoKeys,
        origin,
        "keys",
    )?
    else {
        // A primitive other than a string has no own keys. A primitive string
        // is boxed so its index keys and `length` are reported.
        let elements = match &target {
            StoredValue::String(value) => string_key_values(value, listing)?,
            StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::Symbol(_) => Vec::new(),
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Function(_)
            | StoredValue::Object(_) => {
                return Err(NativeFailure::Execution(
                    EngineFault::RuntimeInvariant {
                        message: "primitive key listing received a non-primitive",
                    }
                    .into(),
                ));
            }
        };
        let array = runtime.allocate_array(realm, elements)?;
        return Ok(NativeDispatch::Immediate(StoredValue::Object(array)));
    };
    let (snapshot, work) = runtime.try_own_key_snapshot(reference, 0, KeyPhases::STRING_KEYS)?;
    execution_budget.charge_instructions(work)?;
    let mut elements = Vec::new();
    elements
        .try_reserve_exact(snapshot.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: snapshot.len(),
        })?;
    for index in 0..snapshot.len() {
        let candidate = snapshot.get(index).ok_or(EngineFault::RuntimeInvariant {
            message: "own-key snapshot shrank during listing",
        })?;
        if matches!(listing, KeyListing::EnumerableOnly) && !candidate.enumerable() {
            continue;
        }
        elements.push(StoredValue::String(property_key_string(candidate.key())?));
    }
    let array = runtime.allocate_array(realm, elements)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(array)))
}

/// Builds the own key list of a primitive string.
///
/// The index keys are enumerable; `length` is not, so it appears only in the
/// `getOwnPropertyNames` listing.
fn string_key_values(
    value: &JsString,
    listing: KeyListing,
) -> Result<Vec<StoredValue>, NativeFailure> {
    let length = value.len();
    let mut elements = Vec::new();
    let reserved = usize::try_from(length)
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    elements
        .try_reserve_exact(reserved)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: reserved,
        })?;
    for index in 0..length {
        elements.push(StoredValue::String(index_string(index)?));
    }
    if matches!(listing, KeyListing::AllStringKeys) {
        elements.push(StoredValue::String(JsString::from_utf8("length")?));
    }
    Ok(elements)
}

/// Renders one own key as the string `Object.keys` reports.
fn property_key_string(key: &PropertyKey) -> Result<JsString, NativeFailure> {
    if let Some(index) = key.as_index() {
        return index_string(index.get());
    }
    let atom = key.as_atom().ok_or(EngineFault::RuntimeInvariant {
        message: "own string key is neither an index nor an atom",
    })?;
    atom.description()
        .cloned()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "own string key atom has no description",
        })
        .map_err(NativeFailure::from)
}

fn index_string(index: u32) -> Result<JsString, NativeFailure> {
    JsNumber::from_u32(index)
        .to_radix_string(10)
        .map_err(NativeFailure::from)
}
