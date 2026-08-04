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

/// Applies `ToObject` for legacy `Object.prototype` entry points.
fn legacy_to_object(
    runtime: &mut Runtime,
    realm: RealmId,
    value: StoredValue,
    origin: &JsStackFrame,
) -> Result<StoredValue, NativeFailure> {
    if matches!(value, StoredValue::Undefined | StoredValue::Null) {
        return Err(NativeFailure::Abrupt(type_error(
            realm,
            Some(origin),
            "Object.prototype",
            "cannot convert to object",
        )?));
    }
    match object_constructor(runtime, realm, Some(value))? {
        NativeDispatch::Immediate(object) => Ok(object),
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. }
        | NativeDispatch::Frame(_)
        | NativeDispatch::Call(_) => Err(EngineFault::RuntimeInvariant {
            message: "ToObject produced a non-immediate result",
        }
        .into()),
    }
}

/// `get Object.prototype.__proto__`.
pub(super) fn object_prototype_proto_getter(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let object = legacy_to_object(runtime, realm, receiver, origin)?;
    let reference = object
        .heap_reference()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Object.prototype.__proto__ getter lost its boxed receiver",
        })?;
    Ok(NativeDispatch::Immediate(heap_reference_value(
        runtime.object_record(reference)?.prototype(),
    )))
}

/// `set Object.prototype.__proto__`.
pub(super) fn object_prototype_proto_setter(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: &StoredValue,
    requested: &StoredValue,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    // RequireObjectCoercible precedes validation of the requested prototype.
    if matches!(receiver, StoredValue::Undefined | StoredValue::Null) {
        return Err(NativeFailure::Abrupt(type_error(
            realm,
            Some(origin),
            "__proto__",
            "not an object",
        )?));
    }
    let prototype = match requested {
        StoredValue::Null => None,
        StoredValue::Function(function) => Some(HeapReference::Function(*function)),
        StoredValue::Object(object) => Some(HeapReference::Object(*object)),
        StoredValue::Undefined
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
        }
    };
    let Some(reference) = receiver.heap_reference() else {
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    };
    match runtime.set_prototype_of(reference, prototype)? {
        SetPrototypeOutcome::Complete => Ok(NativeDispatch::Immediate(StoredValue::Undefined)),
        SetPrototypeOutcome::NonExtensible => Err(NativeFailure::Abrupt(type_error(
            realm,
            Some(origin),
            "__proto__",
            "object is not extensible",
        )?)),
        SetPrototypeOutcome::CyclicPrototype => Err(NativeFailure::Abrupt(type_error(
            realm,
            Some(origin),
            "__proto__",
            "circular prototype chain",
        )?)),
    }
}

/// Starts `__defineGetter__` or `__defineSetter__` in specification order.
pub(super) fn begin_legacy_define_accessor(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    kind: LegacyAccessorKind,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let CallInputs {
        receiver,
        mut arguments,
        ..
    } = inputs;
    let target = legacy_to_object(runtime, realm, receiver, &origin)?;
    let key = arguments.take_first_or_undefined();
    let accessor = arguments.take_first_or_undefined();
    if !matches!(accessor, StoredValue::Function(_)) {
        return Err(NativeFailure::Abrupt(type_error(
            realm,
            Some(&origin),
            "legacy accessor definition",
            "not a function",
        )?));
    }
    begin_property_key_conversion(
        runtime,
        key,
        PropertyKeyTarget::LegacyDefineAccessor {
            target,
            accessor,
            kind,
            realm,
        },
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

/// Starts `__lookupGetter__` or `__lookupSetter__` in specification order.
pub(super) fn begin_legacy_lookup_accessor(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    kind: LegacyAccessorKind,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let CallInputs {
        receiver,
        mut arguments,
        ..
    } = inputs;
    let target = legacy_to_object(runtime, realm, receiver, &origin)?;
    let key = arguments.take_first_or_undefined();
    begin_property_key_conversion(
        runtime,
        key,
        PropertyKeyTarget::LegacyLookupAccessor { target, kind },
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) struct LegacyAccessorDefinition {
    pub(super) target: StoredValue,
    pub(super) accessor: StoredValue,
    pub(super) kind: LegacyAccessorKind,
    pub(super) realm: RealmId,
    pub(super) key: PropertyKey,
    pub(super) name: JsString,
}

/// Completes a legacy accessor definition after `ToPropertyKey`.
pub(super) fn finish_legacy_define_accessor(
    runtime: &mut Runtime,
    definition: LegacyAccessorDefinition,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(accessor) = definition.accessor else {
        return Err(EngineFault::RuntimeInvariant {
            message: "legacy accessor definition lost its validated function",
        }
        .into());
    };
    let property_definition = match definition.kind {
        LegacyAccessorKind::Getter => {
            PropertyDefinition::accessor(Requested::Present(Some(accessor)), Requested::Absent)
        }
        LegacyAccessorKind::Setter => {
            PropertyDefinition::accessor(Requested::Absent, Requested::Present(Some(accessor)))
        }
    }
    .with_enumerable(Requested::Present(true))
    .with_configurable(Requested::Present(true));
    match define_own_property(
        runtime,
        &definition.target,
        definition.key,
        &property_definition,
        execution_budget,
    )? {
        PropertyDefinitionOutcome::Complete => {
            Ok(NativeDispatch::Immediate(StoredValue::Undefined))
        }
        PropertyDefinitionOutcome::Failed(failure) => {
            Err(NativeFailure::Abrupt(property_exception_at(
                definition.realm,
                origin.clone(),
                Some(&definition.name),
                failure,
            )?))
        }
    }
}

/// Completes a legacy accessor lookup after `ToPropertyKey`.
pub(super) fn finish_legacy_lookup_accessor(
    runtime: &Runtime,
    target: &StoredValue,
    kind: LegacyAccessorKind,
    key: &PropertyKey,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut current = target
        .heap_reference()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "legacy accessor lookup lost its boxed receiver",
        })?;
    let mut remaining = runtime
        .functions
        .len()
        .saturating_add(runtime.objects.len())
        .saturating_add(1);
    loop {
        if remaining == 0 {
            return Err(EngineFault::RuntimeInvariant {
                message: "ordinary prototype chain contains a cycle",
            }
            .into());
        }
        remaining -= 1;
        execution_budget.charge_instructions(1)?;
        if let Some(property) = heap_own_property(runtime, current, key)? {
            let function = match property {
                OwnProperty::Accessor { getter, setter, .. } => match kind {
                    LegacyAccessorKind::Getter => getter,
                    LegacyAccessorKind::Setter => setter,
                },
                OwnProperty::Data { .. } => None,
            };
            return Ok(NativeDispatch::Immediate(
                function.map_or(StoredValue::Undefined, StoredValue::Function),
            ));
        }
        let Some(prototype) = runtime.object_record(current)?.prototype() else {
            return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
        };
        current = prototype;
    }
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

/// Applies `Object.create`, delegating its optional descriptor map to the
/// shared resumable `ObjectDefineProperties` operation after allocation.
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
    let target = StoredValue::Object(object);
    let descriptors = arguments.take_first_or_undefined();
    if matches!(descriptors, StoredValue::Undefined) {
        return Ok(NativeDispatch::Immediate(target));
    }
    begin_define_properties(
        runtime,
        realm,
        target,
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
    /// `Object.getOwnPropertySymbols`: every own symbol-keyed property.
    AllSymbolKeys,
}

/// Which ECMA-262 `EnumerableOwnProperties` result projection is requested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EnumerableOwnPropertiesKind {
    /// Append each selected property's value, as `Object.values` does.
    Value,
    /// Append a fresh `[key, value]` Array, as `Object.entries` does.
    KeyAndValue,
}

/// One suspended `EnumerableOwnProperties` scan.
pub(super) struct EnumerableOwnPropertiesContinuation {
    target: StoredValue,
    snapshot: ForInSnapshot,
    next: usize,
    elements: Vec<StoredValue>,
    current_key: Option<PropertyKey>,
    kind: EnumerableOwnPropertiesKind,
    realm: RealmId,
    origin: JsStackFrame,
}

impl EnumerableOwnPropertiesContinuation {
    /// Values held across a getter call and charged to the suspended frame.
    pub(super) fn retained_values(&self) -> u64 {
        1_u64
            .saturating_add(usize_to_u64(self.elements.len()))
            .saturating_add(u64::from(self.current_key.is_some()))
    }

    /// Keeps the source and all collected values alive across getter re-entry.
    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        for element in &self.elements {
            trace_stored_value_root(element, mark);
        }
    }
}

/// Which observable operation an `Object.assign` continuation is awaiting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ObjectAssignStage {
    /// Select and snapshot the next non-nullish source.
    NextSource,
    /// Recheck and read the next key of the current source.
    NextKey,
    /// Await a source accessor getter.
    AwaitGet,
    /// Await a target setter or an Array `length` conversion/write.
    AwaitSet,
}

/// One suspended `Object.assign` traversal.
pub(super) struct ObjectAssignContinuation {
    target: StoredValue,
    sources: Vec<StoredValue>,
    next_source: usize,
    source: Option<StoredValue>,
    keys: Vec<PropertyKey>,
    next_key: usize,
    current_key: Option<PropertyKey>,
    realm: RealmId,
    stage: ObjectAssignStage,
    origin: JsStackFrame,
}

impl ObjectAssignContinuation {
    /// Values and key slots retained across getter, setter, and conversion
    /// re-entry.
    pub(super) fn retained_values(&self) -> u64 {
        1_u64
            .saturating_add(usize_to_u64(self.sources.len()))
            .saturating_add(u64::from(self.source.is_some()))
            .saturating_add(usize_to_u64(self.keys.len()))
            .saturating_add(u64::from(self.current_key.is_some()))
    }

    /// Traces the target, all pending sources, and the active source.
    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        for source in &self.sources {
            trace_stored_value_root(source, mark);
        }
        if let Some(source) = &self.source {
            trace_stored_value_root(source, mark);
        }
    }
}

enum ObjectAssignSet {
    Continue(Box<ObjectAssignContinuation>),
    Suspend(Box<NativeDispatch>),
}

/// `Object.keys(target)`, `Object.getOwnPropertyNames(target)`, and
/// `Object.getOwnPropertySymbols(target)`.
pub(super) fn own_property_keys(
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
        // is boxed so its index keys and `length` are reported by the string
        // projections; no primitive wrapper has own Symbol keys.
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
    let phases = match listing {
        KeyListing::EnumerableOnly | KeyListing::AllStringKeys => KeyPhases::STRING_KEYS,
        KeyListing::AllSymbolKeys => KeyPhases::SYMBOL_KEYS,
    };
    let (snapshot, work) = runtime.try_own_key_snapshot(reference, 0, phases)?;
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
        elements.push(match listing {
            KeyListing::EnumerableOnly | KeyListing::AllStringKeys => {
                StoredValue::String(property_key_string(candidate.key())?)
            }
            KeyListing::AllSymbolKeys => StoredValue::Symbol(
                candidate
                    .key()
                    .as_atom()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "own Symbol key is not atom-backed",
                    })?
                    .clone(),
            ),
        });
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
    if matches!(listing, KeyListing::AllSymbolKeys) {
        return Ok(Vec::new());
    }
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

/// Starts `Object.values` or `Object.entries`.
pub(super) fn begin_enumerable_own_properties(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    kind: EnumerableOwnPropertiesKind,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    if matches!(target, StoredValue::Undefined | StoredValue::Null) {
        return Err(NativeFailure::Abrupt(type_error(
            realm,
            Some(&origin),
            "EnumerableOwnProperties",
            "cannot convert to object",
        )?));
    }
    let Some(reference) = target.heap_reference() else {
        return primitive_enumerable_own_properties(runtime, realm, target, kind, execution_budget);
    };

    // The key list is fixed before any getter runs. It includes hidden String
    // keys because each descriptor's *current* enumerability is rechecked when
    // that key is reached; Symbols are excluded by the abstract operation.
    let (snapshot, work) = runtime.try_own_key_snapshot(reference, 0, KeyPhases::STRING_KEYS)?;
    execution_budget.charge_instructions(work)?;
    let mut elements = Vec::new();
    elements
        .try_reserve_exact(snapshot.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: snapshot.len(),
        })?;
    advance_enumerable_own_properties(
        runtime,
        EnumerableOwnPropertiesContinuation {
            target,
            snapshot,
            next: 0,
            elements,
            current_key: None,
            kind,
            realm,
            origin,
        },
        None,
        return_to,
        execution_budget,
    )
}

/// Resumes the left-to-right descriptor/read loop after an accessor getter.
pub(super) fn advance_enumerable_own_properties(
    runtime: &mut Runtime,
    mut state: EnumerableOwnPropertiesContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        if let Some(key) = state.current_key.take() {
            let value = take_enumerable_completion(&mut completion)?;
            let element = enumerable_result_element(runtime, state.realm, state.kind, &key, value)?;
            state.elements.push(element);
        }

        let Some(candidate) = state.snapshot.get(state.next).cloned() else {
            let array = runtime.allocate_array(state.realm, state.elements)?;
            return Ok(NativeDispatch::Immediate(StoredValue::Object(array)));
        };
        state.next = state.next.saturating_add(1);
        execution_budget.charge_instructions(1)?;

        let reference = state
            .target
            .heap_reference()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "EnumerableOwnProperties lost its object target",
            })?;
        // A prior getter may have deleted the property or changed its
        // enumerability, so this is deliberately not the descriptor captured
        // in the key snapshot.
        charge_heap_property_lookup(runtime, &state.target, execution_budget)?;
        let Some(own) = own_property_of(runtime, reference, candidate.key())? else {
            continue;
        };
        if !own.layout().is_enumerable() {
            continue;
        }

        charge_heap_property_lookup(runtime, &state.target, execution_budget)?;
        match read_heap_property_for_receiver(
            runtime,
            reference,
            state.target.duplicate(),
            candidate.key(),
        )? {
            PropertyReadOutcome::Value(value) => {
                let element = enumerable_result_element(
                    runtime,
                    state.realm,
                    state.kind,
                    candidate.key(),
                    value,
                )?;
                state.elements.push(element);
            }
            PropertyReadOutcome::Getter { function, receiver } => {
                state.current_key = Some(candidate.key().clone());
                let origin = state.origin.clone();
                return Ok(NativeDispatch::Call(NativeCall {
                    function,
                    receiver,
                    arguments: CallArguments::empty(),
                    return_to,
                    origin,
                    continuations: enumerable_own_properties_continuation(state)?,
                    pre_call: None,
                    new_target: None,
                    native_caller: None,
                }));
            }
            PropertyReadOutcome::Failed(failure) => {
                return Err(NativeFailure::Abrupt(property_exception_at(
                    state.realm,
                    state.origin,
                    None,
                    failure,
                )?));
            }
        }
    }
}

/// Produces the immediate primitive result after `ToObject`.
fn primitive_enumerable_own_properties(
    runtime: &mut Runtime,
    realm: RealmId,
    target: StoredValue,
    kind: EnumerableOwnPropertiesKind,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::String(value) = target else {
        execution_budget.charge_instructions(1)?;
        let array = runtime.allocate_array(realm, Vec::new())?;
        return Ok(NativeDispatch::Immediate(StoredValue::Object(array)));
    };
    let length = value.len();
    execution_budget.charge_instructions(u64::from(length).saturating_add(1))?;
    let capacity = usize::try_from(length).unwrap_or(usize::MAX);
    let mut elements = Vec::new();
    elements
        .try_reserve_exact(capacity)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: capacity,
        })?;
    for raw_index in 0..length {
        let index = ArrayIndex::new(raw_index).ok_or(EngineFault::RuntimeInvariant {
            message: "primitive String enumeration reached the non-index u32 maximum",
        })?;
        let key = PropertyKey::from_index(index);
        let element = enumerable_result_element(
            runtime,
            realm,
            kind,
            &key,
            StoredValue::String(value.slice(raw_index..raw_index.saturating_add(1))?),
        )?;
        elements.push(element);
    }
    let array = runtime.allocate_array(realm, elements)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(array)))
}

/// Applies the `value` or `key+value` projection for one selected property.
fn enumerable_result_element(
    runtime: &mut Runtime,
    realm: RealmId,
    kind: EnumerableOwnPropertiesKind,
    key: &PropertyKey,
    value: StoredValue,
) -> Result<StoredValue, NativeFailure> {
    if matches!(kind, EnumerableOwnPropertiesKind::Value) {
        return Ok(value);
    }
    let mut pair = Vec::new();
    pair.try_reserve_exact(2)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 2,
        })?;
    pair.push(StoredValue::String(property_key_string(key)?));
    pair.push(value);
    Ok(StoredValue::Object(runtime.allocate_array(realm, pair)?))
}

/// Builds the one-element continuation list used by a selected getter.
fn enumerable_own_properties_continuation(
    state: EnumerableOwnPropertiesContinuation,
) -> Result<Vec<NativeContinuation>, NativeFailure> {
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::EnumerableOwnProperties(Box::new(state)));
    Ok(continuations)
}

fn take_enumerable_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        EngineFault::RuntimeInvariant {
            message: "EnumerableOwnProperties resumed without a getter completion",
        }
        .into()
    })
}

/// Starts `Object.assign(target, ...sources)`.
pub(super) fn begin_object_assign(
    runtime: &mut Runtime,
    realm: RealmId,
    target: StoredValue,
    sources: Vec<StoredValue>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    // `ToObject(target)` precedes every source operation. Reuse the Object
    // constructor's primitive boxing after rejecting the nullish cases for
    // which `Object(target)` would instead allocate a fresh object.
    if matches!(target, StoredValue::Undefined | StoredValue::Null) {
        return Err(NativeFailure::Abrupt(type_error(
            realm,
            Some(&origin),
            "assign",
            "cannot convert to object",
        )?));
    }
    let target = match object_constructor(runtime, realm, Some(target))? {
        NativeDispatch::Immediate(target) => target,
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. }
        | NativeDispatch::Frame(_)
        | NativeDispatch::Call(_) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Object target coercion produced a non-immediate result",
            }
            .into());
        }
    };
    if sources.is_empty() {
        return Ok(NativeDispatch::Immediate(target));
    }
    advance_object_assign(
        runtime,
        ObjectAssignContinuation {
            target,
            sources,
            next_source: 0,
            source: None,
            keys: Vec::new(),
            next_key: 0,
            current_key: None,
            realm,
            stage: ObjectAssignStage::NextSource,
            origin,
        },
        None,
        return_to,
        execution_budget,
    )
}

/// Resumes the iterative source/key/Get/Set traversal.
pub(super) fn advance_object_assign(
    runtime: &mut Runtime,
    mut state: ObjectAssignContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            ObjectAssignStage::AwaitGet => {
                let key = state
                    .current_key
                    .take()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Object.assign getter resumed without a pending key",
                    })?;
                let value = take_object_assign_completion(&mut completion, "getter")?;
                match object_assign_set(runtime, state, key, value, return_to, execution_budget)? {
                    ObjectAssignSet::Continue(next) => state = *next,
                    ObjectAssignSet::Suspend(dispatch) => return Ok(*dispatch),
                }
            }
            ObjectAssignStage::AwaitSet => {
                let _ = take_object_assign_completion(&mut completion, "setter")?;
                state.stage = ObjectAssignStage::NextKey;
            }
            ObjectAssignStage::NextSource => {
                if state.next_source >= state.sources.len() {
                    return Ok(NativeDispatch::Immediate(state.target));
                }
                let next = std::mem::replace(
                    state.sources.get_mut(state.next_source).ok_or(
                        EngineFault::RuntimeInvariant {
                            message: "Object.assign source cursor escaped its source list",
                        },
                    )?,
                    StoredValue::Undefined,
                );
                state.next_source = state.next_source.saturating_add(1);
                if matches!(next, StoredValue::Undefined | StoredValue::Null) {
                    continue;
                }
                state.keys = object_assign_source_keys(runtime, &next, execution_budget)?;
                state.source = Some(next);
                state.next_key = 0;
                state.stage = ObjectAssignStage::NextKey;
            }
            ObjectAssignStage::NextKey => {
                let Some(key) = state.keys.get(state.next_key).cloned() else {
                    state.source = None;
                    state.keys.clear();
                    state.stage = ObjectAssignStage::NextSource;
                    continue;
                };
                state.next_key = state.next_key.saturating_add(1);
                execution_budget.charge_instructions(1)?;
                let source = state.source.as_ref().ok_or(EngineFault::RuntimeInvariant {
                    message: "Object.assign reached a key without an active source",
                })?;
                charge_object_assign_lookup(runtime, source, execution_budget)?;
                let Some(own) =
                    resolve_own_property(runtime, state.realm, source, &key, &state.origin)?
                else {
                    continue;
                };
                if !own.layout().is_enumerable() {
                    continue;
                }
                charge_object_assign_lookup(runtime, source, execution_budget)?;
                match read_static_property(runtime, state.realm, source, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        match object_assign_set(
                            runtime,
                            state,
                            key,
                            value,
                            return_to,
                            execution_budget,
                        )? {
                            ObjectAssignSet::Continue(next) => state = *next,
                            ObjectAssignSet::Suspend(dispatch) => return Ok(*dispatch),
                        }
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.current_key = Some(key);
                        state.stage = ObjectAssignStage::AwaitGet;
                        return object_assign_call(
                            function,
                            receiver,
                            Vec::new(),
                            state,
                            return_to,
                        );
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(NativeFailure::Abrupt(property_exception_at(
                            state.realm,
                            state.origin,
                            property_key_name(&key).as_ref(),
                            failure,
                        )?));
                    }
                }
            }
        }
    }
}

/// Snapshots one source's `[[OwnPropertyKeys]]` result before any value Get.
fn object_assign_source_keys(
    runtime: &Runtime,
    source: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<Vec<PropertyKey>, NativeFailure> {
    if let Some(reference) = source.heap_reference() {
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
                        message: "Object.assign own-key snapshot shrank",
                    })?
                    .key()
                    .clone(),
            );
        }
        return Ok(keys);
    }
    if let StoredValue::String(value) = source {
        let keys = primitive_string_own_keys(runtime, value)?;
        execution_budget.charge_instructions(usize_to_u64(keys.len()).saturating_add(1))?;
        return Ok(keys);
    }
    execution_budget.charge_instructions(1)?;
    Ok(Vec::new())
}

/// Applies the strict `Set(to, key, value, true)` step.
fn object_assign_set(
    runtime: &mut Runtime,
    mut state: ObjectAssignContinuation,
    key: PropertyKey,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<ObjectAssignSet, NativeFailure> {
    let name = property_key_name(&key);
    if is_array_length_target(runtime, &state.target, &key)? {
        let conversion = array_length_write_target(
            state.target.duplicate(),
            name.clone().ok_or(EngineFault::RuntimeInvariant {
                message: "Array length assignment has no String property name",
            })?,
            true,
            false,
            &value,
        );
        state.stage = ObjectAssignStage::AwaitSet;
        let realm = state.realm;
        let origin = state.origin.clone();
        let dispatch = begin_operator_primitive_conversion(
            runtime,
            value,
            OperatorPrimitiveHint::Number,
            conversion,
            realm,
            return_to,
            origin,
            execution_budget,
        )?;
        return object_assign_after_nested(dispatch, state);
    }

    match write_static_property(
        runtime,
        state.realm,
        &state.target,
        key,
        value,
        true,
        execution_budget,
    )? {
        PropertyWriteOutcome::Complete => {
            state.stage = ObjectAssignStage::NextKey;
            Ok(ObjectAssignSet::Continue(Box::new(state)))
        }
        PropertyWriteOutcome::Setter {
            function,
            receiver,
            value,
        } => {
            let mut arguments = Vec::new();
            arguments
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: 1,
                })?;
            arguments.push(value);
            state.stage = ObjectAssignStage::AwaitSet;
            Ok(ObjectAssignSet::Suspend(Box::new(object_assign_call(
                function, receiver, arguments, state, return_to,
            )?)))
        }
        PropertyWriteOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            state.realm,
            state.origin,
            name.as_ref(),
            failure,
        )?)),
    }
}

/// Chains the assign state outside an Array length conversion continuation.
fn object_assign_after_nested(
    dispatch: NativeDispatch,
    state: ObjectAssignContinuation,
) -> Result<ObjectAssignSet, NativeFailure> {
    let mut outer = Vec::new();
    outer
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    outer.push(NativeContinuation::ObjectAssign(Box::new(state)));
    match dispatch {
        NativeDispatch::Immediate(_) => {
            let Some(NativeContinuation::ObjectAssign(mut state)) = outer.pop() else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Object.assign immediate continuation disappeared",
                }
                .into());
            };
            state.stage = ObjectAssignStage::NextKey;
            Ok(ObjectAssignSet::Continue(state))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(&mut frame, outer)?;
            Ok(ObjectAssignSet::Suspend(Box::new(NativeDispatch::Frame(
                frame,
            ))))
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(&mut call, outer)?;
            Ok(ObjectAssignSet::Suspend(Box::new(NativeDispatch::Call(
                call,
            ))))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Array length conversion produced a structured result",
        }
        .into()),
    }
}

/// Builds a getter or setter call with the assign continuation attached.
fn object_assign_call(
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
    state: ObjectAssignContinuation,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::ObjectAssign(Box::new(state)));
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

fn charge_object_assign_lookup(
    runtime: &Runtime,
    source: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    if source.heap_reference().is_some() {
        charge_heap_property_lookup(runtime, source, execution_budget)
    } else {
        execution_budget.charge_instructions(1).map_err(Into::into)
    }
}

pub(super) fn property_key_name(key: &PropertyKey) -> Option<JsString> {
    if let Some(index) = key.as_index() {
        return index_string(index.get()).ok();
    }
    key.as_atom().and_then(crate::Atom::description).cloned()
}

fn take_object_assign_completion(
    completion: &mut Option<StoredValue>,
    operation: &'static str,
) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        let message = if operation == "getter" {
            "Object.assign resumed without a getter completion"
        } else {
            "Object.assign resumed without a setter completion"
        };
        EngineFault::RuntimeInvariant { message }.into()
    })
}

/// `Object.getOwnPropertyDescriptors(O)`.
///
/// The ordinary-object profile has no Proxy `[[OwnPropertyKeys]]` or
/// `[[GetOwnProperty]]` hooks yet, so the complete admitted operation is
/// synchronous: snapshot every key, allocate the result, re-read each own
/// property, and materialize the descriptor when it remains present.
pub(super) fn get_own_property_descriptors(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    if matches!(target, StoredValue::Undefined | StoredValue::Null) {
        return Err(NativeFailure::Abrupt(type_error(
            realm,
            Some(origin),
            "getOwnPropertyDescriptors",
            "cannot convert to object",
        )?));
    }

    let keys = if let Some(reference) = target.heap_reference() {
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
                        message: "own-key snapshot shrank during descriptor aggregation",
                    })?
                    .key()
                    .clone(),
            );
        }
        keys
    } else if let StoredValue::String(value) = &target {
        let keys = primitive_string_own_keys(runtime, value)?;
        execution_budget.charge_instructions(usize_to_u64(keys.len()).saturating_add(1))?;
        keys
    } else {
        execution_budget.charge_instructions(1)?;
        Vec::new()
    };

    let result = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    let result_reference = HeapReference::Object(result);
    let result_property = PropertyLayout::data(true, true, true);
    for key in keys {
        let Some(own) = resolve_own_property(runtime, realm, &target, &key, origin)? else {
            continue;
        };
        let descriptor = build_descriptor_object(runtime, realm, own)?;
        runtime.append_data_property(
            result_reference,
            key,
            result_property,
            StoredValue::Object(descriptor),
        )?;
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
}

/// Builds the virtual own-key list produced by a primitive String wrapper.
pub(super) fn primitive_string_own_keys(
    runtime: &Runtime,
    value: &JsString,
) -> Result<Vec<PropertyKey>, NativeFailure> {
    let length = value.len();
    let capacity = usize::try_from(length)
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    let mut keys = Vec::new();
    keys.try_reserve_exact(capacity)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: capacity,
        })?;
    for index in 0..length {
        let index = ArrayIndex::new(index).ok_or(EngineFault::RuntimeInvariant {
            message: "primitive String key reached the non-index u32 maximum",
        })?;
        keys.push(PropertyKey::from_index(index));
    }
    keys.push(runtime.predefined_property_key(PredefinedAtom::Length));
    Ok(keys)
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
