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

/// Applies `Object.create(prototype, descriptors)`.
///
/// The descriptors argument runs the same `ObjectDefineProperties` that
/// `Object.defineProperties` does, on the freshly created object, so a
/// descriptor read can enter an accessor and the whole operation is resumable
/// (`quickjs.c:40095-40110`). An absent or `undefined` argument creates the
/// object with no own property.
#[allow(
    clippy::too_many_arguments,
    reason = "object creation carries the same runtime, realm, operand, resume, origin, and budget authority as every other resumable native operation"
)]
pub(super) fn object_create(
    runtime: &mut Runtime,
    realm: RealmId,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let requested = arguments.take_first_or_undefined();
    // Only `null` and an object are prototypes; the oracle reports every other
    // argument as `not a prototype`, including an absent one.
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
                Some(&origin),
                "create",
                "not a prototype",
            )?));
        }
    };
    let object = runtime.allocate_ordinary_object_with_optional_prototype(prototype)?;
    let descriptors = arguments.take_first_or_undefined();
    if matches!(descriptors, StoredValue::Undefined) {
        return Ok(NativeDispatch::Immediate(StoredValue::Object(object)));
    }
    begin_define_properties(
        runtime,
        realm,
        StoredValue::Object(object),
        descriptors,
        return_to,
        origin,
        execution_budget,
    )
}

/// Applies `Object.prototype.isPrototypeOf`.
///
/// The walk starts at the *candidate's* prototype, not the candidate itself, so
/// a receiver never precedes itself and `p.isPrototypeOf(p)` is `false`. A
/// primitive candidate has no chain of its own, so the answer is `false` without
/// consulting its wrapper prototype, which the pinned oracle confirms:
/// `({}).isPrototypeOf(1)` is `false`.
pub(super) fn object_prototype_is_prototype_of(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: &StoredValue,
    candidate: &StoredValue,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    // `ToObject(this)` runs first, so a nullish receiver throws even when the
    // candidate would have answered `false`.
    if matches!(receiver, StoredValue::Undefined | StoredValue::Null) {
        return Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("cannot convert to object")?,
            },
            origin: origin.clone(),
        }));
    }
    let Some(target) = receiver.heap_reference() else {
        // A primitive receiver is not on any prototype chain.
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
    };
    let Some(start) = candidate.heap_reference() else {
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
    };

    let mut current = runtime.object_record(start)?.prototype();
    while let Some(reference) = current {
        // The walk is bounded by the same budget every prototype lookup uses, so
        // a long chain cannot run unaccounted.
        execution_budget.charge_instructions(1)?;
        if reference == target {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)));
        }
        current = runtime.object_record(reference)?.prototype();
    }
    Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
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

pub(super) fn heap_reference_value(reference: Option<HeapReference>) -> StoredValue {
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

/// `Object.getOwnPropertySymbols(target)`.
///
/// This is the symbol-only half of `[[OwnPropertyKeys]]`, so it shares the same
/// snapshot as `Object.keys` with only the symbol phase enabled
/// (`JS_GPN_SYMBOL_MASK`, `quickjs.c:40270-40276`). A primitive other than
/// `null` and `undefined` answers empty rather than throwing, because a boxed
/// wrapper never carries a symbol key, and a nullish target reports the
/// `ToObject` failure the way `Object.keys` does.
pub(super) fn own_property_symbols(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
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
        "getOwnPropertySymbols",
    )?
    else {
        let array = runtime.allocate_array(realm, Vec::new())?;
        return Ok(NativeDispatch::Immediate(StoredValue::Object(array)));
    };
    let (snapshot, work) = runtime.try_own_key_snapshot(reference, 0, KeyPhases::SYMBOL_KEYS)?;
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
            message: "own-key snapshot shrank during a symbol listing",
        })?;
        let atom = candidate
            .key()
            .as_atom()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "symbol-phase own key is not an atom",
            })?;
        elements.push(StoredValue::Symbol(atom.clone()));
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

/// Applies `Object.prototype.toLocaleString`.
///
/// This is a bare forward to the receiver's own `toString`, resolved through the
/// prototype chain, with *no* argument passed along: the locale arguments the
/// name suggests belong to the `Intl` layer, and the base implementation ignores
/// them (`quickjs.c:40470-40480`). A nullish receiver therefore fails on the
/// `toString` lookup rather than on a `ToObject` of its own.
pub(super) fn begin_object_prototype_to_locale_string(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let key = runtime.predefined_property_key(PredefinedAtom::ToString);
    charge_heap_property_lookup(runtime, &receiver, execution_budget)?;
    let method = match read_static_property(runtime, realm, &receiver, &key)? {
        PropertyReadOutcome::Value(value) => value,
        // The lookup itself can enter an accessor, whose result is the method
        // to call, so the forward suspends on it.
        PropertyReadOutcome::Getter {
            function,
            receiver: getter_receiver,
        } => {
            let mut continuations = Vec::new();
            continuations
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 1,
                })?;
            continuations.push(NativeContinuation::ToLocaleString(
                ToLocaleStringContinuation {
                    receiver,
                    realm,
                    origin: origin.clone(),
                },
            ));
            return Ok(NativeDispatch::Call(NativeCall {
                function,
                receiver: getter_receiver,
                arguments: CallArguments::empty(),
                return_to,
                origin,
                continuations,
                pre_call: None,
                new_target: None,
                native_caller: None,
            }));
        }
        PropertyReadOutcome::Failed(failure) => {
            return Err(NativeFailure::Abrupt(property_exception_at(
                realm,
                origin,
                Some(&JsString::from_utf8("toString")?),
                failure,
            )?));
        }
    };
    call_object_prototype_to_locale_string(
        ToLocaleStringContinuation {
            receiver,
            realm,
            origin,
        },
        &method,
        return_to,
    )
}

/// Calls the resolved `toString`, which must be callable.
pub(super) fn call_object_prototype_to_locale_string(
    state: ToLocaleStringContinuation,
    method: &StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let ToLocaleStringContinuation {
        receiver,
        realm,
        origin,
    } = state;
    let StoredValue::Function(function) = method else {
        return Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("not a function")?,
            },
            origin,
        }));
    };
    let function = *function;
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

/// Applies the `Object.prototype.__proto__` getter.
///
/// The getter is `Object.getPrototypeOf` with the receiver in place of the
/// argument, so a primitive answers with its wrapper's prototype and a nullish
/// receiver throws (`quickjs.c:40640-40650`).
pub(super) fn object_prototype_proto_getter(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    origin: Option<&JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    get_prototype_of(runtime, realm, Some(receiver), origin)
}

/// Applies the `Object.prototype.__proto__` setter.
///
/// Unlike `Object.setPrototypeOf`, a non-object prototype is *ignored* rather
/// than rejected, because the setter's argument is not a validated operand:
/// `({}).__proto__ = 5` leaves the prototype untouched and completes normally
/// (`quickjs.c:40652-40666`). A refused change still throws, since the setter
/// runs as an ordinary strict `Set`.
pub(super) fn object_prototype_proto_setter(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    origin: Option<&JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let requested = arguments.take_first_or_undefined();
    let prototype = match requested {
        StoredValue::Null => None,
        StoredValue::Function(function) => Some(HeapReference::Function(function)),
        StoredValue::Object(object) => Some(HeapReference::Object(object)),
        // Any other value is silently ignored.
        StoredValue::Undefined
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
        }
    };
    // A primitive receiver has no prototype slot to write, and completes
    // without throwing.
    let Some(reference) = reflection_target(
        runtime,
        realm,
        receiver,
        PrimitivePolicy::ReturnArgument,
        origin,
        "__proto__",
    )?
    else {
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    };
    match runtime.set_prototype_of(reference, prototype)? {
        SetPrototypeOutcome::Complete => Ok(NativeDispatch::Immediate(StoredValue::Undefined)),
        SetPrototypeOutcome::NonExtensible => Err(NativeFailure::Abrupt(type_error(
            realm,
            origin,
            "__proto__",
            "object is not extensible",
        )?)),
        SetPrototypeOutcome::CyclicPrototype => Err(NativeFailure::Abrupt(type_error(
            realm,
            origin,
            "__proto__",
            "circular prototype chain",
        )?)),
    }
}

/// Starts `Object.prototype.__defineGetter__` or `__defineSetter__`.
///
/// The accessor is validated as callable *before* the key converts, so a
/// non-callable second argument reports `not a function` without running the
/// key's `toString` (`quickjs.c:40530-40560`). The receiver then converts with
/// `ToObject`, which is why a nullish one throws while any other primitive
/// completes without defining anything.
#[allow(
    clippy::too_many_arguments,
    reason = "a legacy accessor definer carries the same runtime, realm, operand, resume, origin, and budget authority as every other resumable native operation"
)]
pub(super) fn begin_legacy_accessor_definition(
    runtime: &mut Runtime,
    realm: RealmId,
    role: AccessorRole,
    receiver: StoredValue,
    key: StoredValue,
    accessor: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if !matches!(accessor, StoredValue::Function(_)) {
        return Err(NativeFailure::Abrupt(type_error(
            realm,
            Some(&origin),
            role.define_name(),
            "not a function",
        )?));
    }
    let target = match receiver {
        value @ (StoredValue::Function(_) | StoredValue::Object(_)) => value,
        StoredValue::Undefined | StoredValue::Null => {
            return Err(NativeFailure::Abrupt(type_error(
                realm,
                Some(&origin),
                role.define_name(),
                "cannot convert to object",
            )?));
        }
        StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
        }
    };
    begin_property_key_conversion(
        runtime,
        key,
        PropertyKeyTarget::LegacyAccessorDefinition {
            target,
            accessor,
            role,
            realm,
        },
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

/// Defines one legacy accessor once its key has been converted.
///
/// The property is enumerable and configurable, which is what separates this
/// from `Object.defineProperty`'s all-`false` defaults, and only the addressed
/// half is supplied so the other stays absent.
#[allow(
    clippy::too_many_arguments,
    reason = "the definition needs the runtime, realm, role, target, accessor, key, origin, and budget together"
)]
pub(super) fn finish_legacy_accessor_definition(
    runtime: &mut Runtime,
    realm: RealmId,
    role: AccessorRole,
    target: &StoredValue,
    accessor: &StoredValue,
    property: StaticPropertyOperand,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(function) = accessor else {
        return Err(EngineFault::RuntimeInvariant {
            message: "validated legacy accessor is not a function",
        }
        .into());
    };
    let requested = Requested::Present(Some(*function));
    let definition = match role {
        AccessorRole::Getter => PropertyDefinition::accessor(requested, Requested::Absent),
        AccessorRole::Setter => PropertyDefinition::accessor(Requested::Absent, requested),
    }
    .with_enumerable(Requested::Present(true))
    .with_configurable(Requested::Present(true));
    match define_own_property(runtime, target, property.key, &definition, execution_budget)? {
        PropertyDefinitionOutcome::Complete => {
            Ok(NativeDispatch::Immediate(StoredValue::Undefined))
        }
        PropertyDefinitionOutcome::Failed(failure) => Err(NativeFailure::Abrupt(
            property_exception_at(realm, origin.clone(), Some(&property.name), failure)?,
        )),
    }
}

/// Starts `Object.prototype.__lookupGetter__` or `__lookupSetter__`.
///
/// The receiver converts with `ToObject`, so a nullish one throws while any
/// other primitive answers through its wrapper prototype's chain
/// (`quickjs.c:40580-40620`).
#[allow(
    clippy::too_many_arguments,
    reason = "a legacy accessor lookup carries the same runtime, realm, operand, resume, origin, and budget authority as every other resumable native operation"
)]
pub(super) fn begin_legacy_accessor_lookup(
    runtime: &mut Runtime,
    realm: RealmId,
    role: AccessorRole,
    receiver: StoredValue,
    key: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(receiver, StoredValue::Undefined | StoredValue::Null) {
        return Err(NativeFailure::Abrupt(type_error(
            realm,
            Some(&origin),
            role.lookup_name(),
            "cannot convert to object",
        )?));
    }
    begin_property_key_conversion(
        runtime,
        key,
        PropertyKeyTarget::LegacyAccessorLookup {
            target: receiver,
            role,
            realm,
        },
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

/// Answers one legacy accessor lookup once its key has been converted.
///
/// The whole prototype chain is walked, and a data property answers `undefined`
/// rather than its value, because only an accessor's addressed half is reported.
pub(super) fn finish_legacy_accessor_lookup(
    runtime: &Runtime,
    realm: RealmId,
    role: AccessorRole,
    target: &StoredValue,
    property: &StaticPropertyOperand,
) -> Result<NativeDispatch, NativeFailure> {
    let start = match target {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        // A primitive's own chain starts at its wrapper prototype.
        StoredValue::Boolean(_) => HeapReference::Object(runtime.realm_boolean_prototype(realm)?),
        StoredValue::Number(_) => HeapReference::Object(runtime.realm_number_prototype(realm)?),
        StoredValue::BigInt(_) => HeapReference::Object(runtime.realm_bigint_prototype(realm)?),
        StoredValue::String(_) => HeapReference::Object(runtime.realm_string_prototype(realm)?),
        StoredValue::Symbol(_) => HeapReference::Object(runtime.realm_symbol_prototype(realm)?),
        StoredValue::Undefined | StoredValue::Null => {
            return Err(EngineFault::RuntimeInvariant {
                message: "legacy accessor lookup kept a nullish receiver",
            }
            .into());
        }
    };
    let answer = match lookup_heap_property(runtime, Some(start), &property.key)? {
        Some(OwnProperty::Accessor { getter, setter, .. }) => match role {
            AccessorRole::Getter => getter,
            AccessorRole::Setter => setter,
        },
        Some(OwnProperty::Data { .. }) | None => None,
    };
    Ok(NativeDispatch::Immediate(match answer {
        Some(function) => StoredValue::Function(function),
        None => StoredValue::Undefined,
    }))
}
