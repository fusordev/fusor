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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntegrityOperation {
    Set,
    Test,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntegrityStage {
    PreventExtensions,
    IsExtensible,
    OwnKeys,
    Descriptor,
    Define,
}

pub(super) struct IntegrityLevelContinuation {
    target: StoredValue,
    reference: HeapReference,
    keys: Vec<PropertyKey>,
    next: usize,
    level: IntegrityLevel,
    operation: IntegrityOperation,
    stage: IntegrityStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl IntegrityLevelContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(usize_to_u64(self.keys.len()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
    }
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

/// Applies `ToObject` for standard `Object.prototype` entry points.
fn object_prototype_to_object(
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

pub(super) fn advance_object_meta(
    state: ObjectMetaContinuation,
    completion: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Boolean(success) = *completion else {
        return Err(EngineFault::RuntimeInvariant {
            message: "object meta internal method did not return a Boolean",
        }
        .into());
    };
    if !success {
        return Err(NativeFailure::Abrupt(type_error(
            state.realm,
            Some(&state.origin),
            "Object meta operation",
            match state.failure {
                ObjectMetaFailure::NonExtensible => "object is not extensible",
                ObjectMetaFailure::ProxyTrap => "Proxy trap returned false",
            },
        )?));
    }
    Ok(NativeDispatch::Immediate(state.completion))
}

fn continue_object_meta_after(
    dispatch: NativeDispatch,
    state: ObjectMetaContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => advance_object_meta(state, &value),
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(&mut call, vec![NativeContinuation::ObjectMeta(state)])?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(&mut frame, vec![NativeContinuation::ObjectMeta(state)])?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "object meta internal method produced a structured result",
        }
        .into()),
    }
}

/// `Object.getPrototypeOf(target)`.
pub(super) fn get_prototype_of(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let value = argument.unwrap_or(StoredValue::Undefined);
    // A primitive answers with its wrapper's prototype, which is the intrinsic
    // prototype for its type.
    let Some(reference) = reflection_target(
        runtime,
        realm,
        &value,
        PrimitivePolicy::PrototypeLookup,
        Some(&origin),
        "getPrototypeOf",
    )?
    else {
        let prototype = primitive_prototype(runtime, realm, &value)?;
        return Ok(NativeDispatch::Immediate(prototype));
    };
    begin_internal_get_prototype_of(
        runtime,
        reference,
        realm,
        return_to,
        origin,
        execution_budget,
    )
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

pub(super) struct IsPrototypeOfContinuation {
    target: HeapReference,
    current: HeapReference,
    realm: RealmId,
    origin: JsStackFrame,
}

impl IsPrototypeOfContinuation {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(self.target));
        mark(CollectionRoot::Heap(self.current));
    }
}

enum IsPrototypeOfDispatch {
    Resume(IsPrototypeOfContinuation, StoredValue),
    Suspend(Box<NativeDispatch>),
}

fn continue_is_prototype_of_after(
    dispatch: NativeDispatch,
    state: IsPrototypeOfContinuation,
) -> Result<IsPrototypeOfDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => Ok(IsPrototypeOfDispatch::Resume(state, value)),
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::IsPrototypeOf(Box::new(state))],
            )?;
            Ok(IsPrototypeOfDispatch::Suspend(Box::new(
                NativeDispatch::Call(call),
            )))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::IsPrototypeOf(Box::new(state))],
            )?;
            Ok(IsPrototypeOfDispatch::Suspend(Box::new(
                NativeDispatch::Frame(frame),
            )))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "isPrototypeOf [[GetPrototypeOf]] produced a structured result",
        }
        .into()),
    }
}

pub(super) fn advance_is_prototype_of(
    runtime: &mut Runtime,
    mut state: IsPrototypeOfContinuation,
    mut completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        let Some(prototype) = completion.heap_reference() else {
            if matches!(completion, StoredValue::Null) {
                return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
            }
            return Err(EngineFault::RuntimeInvariant {
                message: "[[GetPrototypeOf]] returned neither object nor null",
            }
            .into());
        };
        if prototype == state.target {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)));
        }
        state.current = prototype;
        execution_budget.charge_instructions(1)?;
        let dispatch = begin_internal_get_prototype_of(
            runtime,
            prototype,
            state.realm,
            return_to,
            state.origin.clone(),
            execution_budget,
        )?;
        match continue_is_prototype_of_after(dispatch, state)? {
            IsPrototypeOfDispatch::Resume(next_state, next_completion) => {
                state = next_state;
                completion = next_completion;
            }
            IsPrototypeOfDispatch::Suspend(dispatch) => return Ok(*dispatch),
        }
    }
}

/// Applies `Object.prototype.isPrototypeOf`.
///
/// The candidate type check precedes `ToObject(this)`, then each step uses the
/// candidate's observable `[[GetPrototypeOf]]` internal method.
pub(super) fn object_prototype_is_prototype_of(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    candidate: &StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(start) = candidate.heap_reference() else {
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
    };
    let target = object_prototype_to_object(runtime, realm, receiver, &origin)?
        .heap_reference()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "isPrototypeOf lost its boxed receiver",
        })?;
    let state = IsPrototypeOfContinuation {
        target,
        current: start,
        realm,
        origin: origin.clone(),
    };
    execution_budget.charge_instructions(1)?;
    let dispatch = begin_internal_get_prototype_of(
        runtime,
        start,
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    match continue_is_prototype_of_after(dispatch, state)? {
        IsPrototypeOfDispatch::Resume(state, completion) => {
            advance_is_prototype_of(runtime, state, completion, return_to, execution_budget)
        }
        IsPrototypeOfDispatch::Suspend(dispatch) => Ok(*dispatch),
    }
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
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
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
                Some(&origin),
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
        Some(&origin),
        "setPrototypeOf",
    )?
    else {
        return Ok(NativeDispatch::Immediate(target));
    };
    let dispatch = begin_internal_set_prototype_of(
        runtime,
        reference,
        prototype,
        realm,
        return_to,
        origin.clone(),
        execution_budget,
    )?;
    continue_object_meta_after(
        dispatch,
        ObjectMetaContinuation {
            completion: target,
            failure: ObjectMetaFailure::NonExtensible,
            realm,
            origin,
        },
    )
}

enum IntegrityDispatch {
    Resume(IntegrityLevelContinuation, StoredValue),
    Suspend(Box<NativeDispatch>),
}

fn continue_integrity_after(
    dispatch: NativeDispatch,
    state: IntegrityLevelContinuation,
) -> Result<IntegrityDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => Ok(IntegrityDispatch::Resume(state, value)),
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::IntegrityLevel(Box::new(state))],
            )?;
            Ok(IntegrityDispatch::Suspend(Box::new(NativeDispatch::Call(
                call,
            ))))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::IntegrityLevel(Box::new(state))],
            )?;
            Ok(IntegrityDispatch::Suspend(Box::new(NativeDispatch::Frame(
                frame,
            ))))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "integrity-level internal method produced a structured result",
        }
        .into()),
    }
}

fn integrity_type_error(
    state: &IntegrityLevelContinuation,
) -> Result<NativeFailure, NativeFailure> {
    Ok(NativeFailure::Abrupt(type_error(
        state.realm,
        Some(&state.origin),
        "integrity level",
        "integrity operation was rejected",
    )?))
}

fn internal_descriptor_field(
    runtime: &Runtime,
    descriptor: ObjectId,
    atom: PredefinedAtom,
) -> Result<Option<StoredValue>, NativeFailure> {
    let key = runtime.predefined_property_key(atom);
    match heap_own_property(runtime, HeapReference::Object(descriptor), &key)? {
        Some(OwnProperty::Data { value, .. }) => Ok(Some(value)),
        Some(OwnProperty::Accessor { .. }) => Err(EngineFault::RuntimeInvariant {
            message: "internal descriptor field is not a data property",
        }
        .into()),
        None => Ok(None),
    }
}

fn internal_descriptor_flag(
    runtime: &Runtime,
    descriptor: ObjectId,
    atom: PredefinedAtom,
) -> Result<bool, NativeFailure> {
    let Some(StoredValue::Boolean(value)) = internal_descriptor_field(runtime, descriptor, atom)?
    else {
        return Err(EngineFault::RuntimeInvariant {
            message: "internal descriptor flag is missing or not Boolean",
        }
        .into());
    };
    Ok(value)
}

fn internal_descriptor_accessor(value: &StoredValue) -> Result<Option<FunctionId>, NativeFailure> {
    match value {
        StoredValue::Undefined => Ok(None),
        StoredValue::Function(function) => Ok(Some(*function)),
        StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Object(_) => Err(EngineFault::RuntimeInvariant {
            message: "internal descriptor accessor is neither callable nor undefined",
        }
        .into()),
    }
}

pub(super) fn internal_complete_own_property(
    runtime: &Runtime,
    completion: &StoredValue,
) -> Result<Option<OwnProperty>, NativeFailure> {
    let descriptor = match completion {
        StoredValue::Undefined => return Ok(None),
        StoredValue::Object(descriptor) => *descriptor,
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "internal own-property result is neither descriptor nor undefined",
            }
            .into());
        }
    };
    let enumerable = internal_descriptor_flag(runtime, descriptor, PredefinedAtom::Enumerable)?;
    let configurable = internal_descriptor_flag(runtime, descriptor, PredefinedAtom::Configurable)?;
    let value = internal_descriptor_field(runtime, descriptor, PredefinedAtom::Value)?;
    let writable = internal_descriptor_field(runtime, descriptor, PredefinedAtom::Writable)?;
    let getter = internal_descriptor_field(runtime, descriptor, PredefinedAtom::Get)?;
    let setter = internal_descriptor_field(runtime, descriptor, PredefinedAtom::SetProperty)?;
    match (value, writable, getter, setter) {
        (Some(value), Some(StoredValue::Boolean(writable)), None, None) => {
            Ok(Some(OwnProperty::Data {
                layout: PropertyLayout::data(writable, enumerable, configurable),
                value,
            }))
        }
        (None, None, Some(getter), Some(setter)) => Ok(Some(OwnProperty::Accessor {
            layout: PropertyLayout::accessor(enumerable, configurable),
            getter: internal_descriptor_accessor(&getter)?,
            setter: internal_descriptor_accessor(&setter)?,
        })),
        _ => Err(EngineFault::RuntimeInvariant {
            message: "internal descriptor is not complete",
        }
        .into()),
    }
}

fn finish_integrity_level(state: IntegrityLevelContinuation, result: bool) -> NativeDispatch {
    NativeDispatch::Immediate(match state.operation {
        IntegrityOperation::Set => state.target,
        IntegrityOperation::Test => StoredValue::Boolean(result),
    })
}

fn begin_integrity_key_operation(
    runtime: &mut Runtime,
    mut state: IntegrityLevelContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<IntegrityDispatch, NativeFailure> {
    if state.next >= state.keys.len() {
        return Ok(IntegrityDispatch::Suspend(Box::new(
            finish_integrity_level(state, true),
        )));
    }
    let key = state.keys[state.next].clone();
    let dispatch =
        if state.operation == IntegrityOperation::Set && state.level == IntegrityLevel::Sealed {
            state.stage = IntegrityStage::Define;
            begin_internal_define_own_property(
                runtime,
                state.reference,
                key,
                PropertyDefinition::generic().with_configurable(Requested::Present(false)),
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
                DefinePropertyResult::Boolean,
            )?
        } else {
            state.stage = IntegrityStage::Descriptor;
            begin_internal_get_own_property(
                runtime,
                state.reference,
                key,
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            )?
        };
    continue_integrity_after(dispatch, state)
}

#[allow(
    clippy::too_many_lines,
    reason = "the iterative SetIntegrityLevel/TestIntegrityLevel driver keeps the normative internal-method order in one typed state machine"
)]
pub(super) fn advance_integrity_level(
    runtime: &mut Runtime,
    mut state: IntegrityLevelContinuation,
    mut completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        let next = match state.stage {
            IntegrityStage::PreventExtensions => {
                let StoredValue::Boolean(success) = completion else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "[[PreventExtensions]] did not return a Boolean",
                    }
                    .into());
                };
                if !success {
                    return Err(integrity_type_error(&state)?);
                }
                state.stage = IntegrityStage::OwnKeys;
                let dispatch = begin_internal_own_keys(
                    runtime,
                    state.reference,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                continue_integrity_after(dispatch, state)?
            }
            IntegrityStage::IsExtensible => {
                let StoredValue::Boolean(extensible) = completion else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "[[IsExtensible]] did not return a Boolean",
                    }
                    .into());
                };
                if extensible {
                    return Ok(finish_integrity_level(state, false));
                }
                state.stage = IntegrityStage::OwnKeys;
                let dispatch = begin_internal_own_keys(
                    runtime,
                    state.reference,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                continue_integrity_after(dispatch, state)?
            }
            IntegrityStage::OwnKeys => {
                state.keys = generated_key_list(runtime, completion)?;
                state.next = 0;
                begin_integrity_key_operation(runtime, state, return_to, execution_budget)?
            }
            IntegrityStage::Descriptor => {
                let own = internal_complete_own_property(runtime, &completion)?;
                match state.operation {
                    IntegrityOperation::Test => {
                        if own.as_ref().is_some_and(|property| {
                            property.layout().is_configurable()
                                || (state.level == IntegrityLevel::Frozen
                                    && property.layout().writable() == Some(true))
                        }) {
                            return Ok(finish_integrity_level(state, false));
                        }
                        state.next = state.next.saturating_add(1);
                        begin_integrity_key_operation(runtime, state, return_to, execution_budget)?
                    }
                    IntegrityOperation::Set => {
                        let Some(own) = own else {
                            state.next = state.next.saturating_add(1);
                            let next = begin_integrity_key_operation(
                                runtime,
                                state,
                                return_to,
                                execution_budget,
                            )?;
                            match next {
                                IntegrityDispatch::Resume(next_state, next_completion) => {
                                    state = next_state;
                                    completion = next_completion;
                                    continue;
                                }
                                IntegrityDispatch::Suspend(dispatch) => return Ok(*dispatch),
                            }
                        };
                        let definition = if own.layout().writable().is_some() {
                            PropertyDefinition::data(Requested::Absent, Requested::Present(false))
                        } else {
                            PropertyDefinition::generic()
                        }
                        .with_configurable(Requested::Present(false));
                        state.stage = IntegrityStage::Define;
                        let dispatch = begin_internal_define_own_property(
                            runtime,
                            state.reference,
                            state.keys[state.next].clone(),
                            definition,
                            state.realm,
                            return_to,
                            state.origin.clone(),
                            execution_budget,
                            DefinePropertyResult::Boolean,
                        )?;
                        continue_integrity_after(dispatch, state)?
                    }
                }
            }
            IntegrityStage::Define => {
                let StoredValue::Boolean(success) = completion else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "[[DefineOwnProperty]] did not return a Boolean",
                    }
                    .into());
                };
                if !success {
                    return Err(integrity_type_error(&state)?);
                }
                state.next = state.next.saturating_add(1);
                begin_integrity_key_operation(runtime, state, return_to, execution_budget)?
            }
        };
        match next {
            IntegrityDispatch::Resume(next_state, next_completion) => {
                state = next_state;
                completion = next_completion;
            }
            IntegrityDispatch::Suspend(dispatch) => return Ok(*dispatch),
        }
    }
}

/// `Object.seal(target)` and `Object.freeze(target)`.
pub(super) fn set_integrity_level(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    level: IntegrityLevel,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    let Some(reference) = reflection_target(
        runtime,
        realm,
        &target,
        PrimitivePolicy::ReturnArgument,
        Some(&origin),
        "seal",
    )?
    else {
        return Ok(NativeDispatch::Immediate(target));
    };
    let state = IntegrityLevelContinuation {
        target,
        reference,
        keys: Vec::new(),
        next: 0,
        level,
        operation: IntegrityOperation::Set,
        stage: IntegrityStage::PreventExtensions,
        realm,
        origin: origin.clone(),
    };
    let dispatch = begin_internal_prevent_extensions(
        runtime,
        reference,
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    match continue_integrity_after(dispatch, state)? {
        IntegrityDispatch::Resume(state, completion) => {
            advance_integrity_level(runtime, state, completion, return_to, execution_budget)
        }
        IntegrityDispatch::Suspend(dispatch) => Ok(*dispatch),
    }
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
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    let Some(reference) = reflection_target(
        runtime,
        realm,
        &target,
        PrimitivePolicy::TreatAsSealed,
        Some(&origin),
        "isSealed",
    )?
    else {
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)));
    };
    let state = IntegrityLevelContinuation {
        target,
        reference,
        keys: Vec::new(),
        next: 0,
        level,
        operation: IntegrityOperation::Test,
        stage: IntegrityStage::IsExtensible,
        realm,
        origin: origin.clone(),
    };
    let dispatch = begin_internal_is_extensible(
        runtime,
        reference,
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    match continue_integrity_after(dispatch, state)? {
        IntegrityDispatch::Resume(state, completion) => {
            advance_integrity_level(runtime, state, completion, return_to, execution_budget)
        }
        IntegrityDispatch::Suspend(dispatch) => Ok(*dispatch),
    }
}

/// `Object.preventExtensions(target)`.
pub(super) fn prevent_extensions(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    let Some(reference) = reflection_target(
        runtime,
        realm,
        &target,
        PrimitivePolicy::ReturnArgument,
        Some(&origin),
        "preventExtensions",
    )?
    else {
        return Ok(NativeDispatch::Immediate(target));
    };
    let dispatch = begin_internal_prevent_extensions(
        runtime,
        reference,
        realm,
        return_to,
        origin.clone(),
        execution_budget,
    )?;
    continue_object_meta_after(
        dispatch,
        ObjectMetaContinuation {
            completion: target,
            failure: ObjectMetaFailure::ProxyTrap,
            realm,
            origin,
        },
    )
}

/// `Object.isExtensible(target)`.
///
/// A primitive is never extensible, which the oracle reports as
/// `Object.isExtensible(5) === false`.
pub(super) fn is_extensible(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    let Some(reference) = reflection_target(
        runtime,
        realm,
        &target,
        PrimitivePolicy::TreatAsSealed,
        Some(&origin),
        "isExtensible",
    )?
    else {
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
    };
    begin_internal_is_extensible(
        runtime,
        reference,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

/// Whether a key listing reports only enumerable properties.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum KeyListing {
    /// `Object.keys`: own enumerable string-keyed properties.
    EnumerableOnly,
    /// `Object.getOwnPropertyNames`: every own string-keyed property.
    AllStringKeys,
    /// `Object.getOwnPropertySymbols`: every own symbol-keyed property.
    AllSymbolKeys,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjectKeyListingStage {
    AwaitKeys,
    AwaitDescriptor,
}

pub(super) struct ObjectKeyListingContinuation {
    target: HeapReference,
    keys: Vec<PropertyKey>,
    next: usize,
    elements: Vec<StoredValue>,
    listing: KeyListing,
    realm: RealmId,
    origin: JsStackFrame,
    stage: ObjectKeyListingStage,
}

impl ObjectKeyListingContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64
            .saturating_add(usize_to_u64(self.keys.len()))
            .saturating_add(usize_to_u64(self.elements.len()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(self.target));
        for value in &self.elements {
            trace_stored_value_root(value, mark);
        }
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProxyEnumerableStage {
    Keys,
    Descriptor,
    Value,
}

pub(super) struct ProxyEnumerableContinuation {
    target: HeapReference,
    keys: Vec<PropertyKey>,
    next: usize,
    current_key: Option<PropertyKey>,
    elements: Vec<StoredValue>,
    kind: EnumerableOwnPropertiesKind,
    realm: RealmId,
    origin: JsStackFrame,
    stage: ProxyEnumerableStage,
}

impl ProxyEnumerableContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64
            .saturating_add(usize_to_u64(self.keys.len()))
            .saturating_add(usize_to_u64(self.elements.len()))
            .saturating_add(u64::from(self.current_key.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(self.target));
        for value in &self.elements {
            trace_stored_value_root(value, mark);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GetOwnPropertyDescriptorsStage {
    Keys,
    Descriptor,
}

/// One resumable `Object.getOwnPropertyDescriptors` traversal.
pub(super) struct GetOwnPropertyDescriptorsContinuation {
    target: HeapReference,
    keys: Vec<PropertyKey>,
    next: usize,
    current_key: Option<PropertyKey>,
    result: Option<ObjectId>,
    realm: RealmId,
    origin: JsStackFrame,
    stage: GetOwnPropertyDescriptorsStage,
}

impl GetOwnPropertyDescriptorsContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64
            .saturating_add(usize_to_u64(self.keys.len()))
            .saturating_add(u64::from(self.current_key.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(self.target));
        if let Some(result) = self.result {
            mark(CollectionRoot::Heap(HeapReference::Object(result)));
        }
    }
}

/// Which observable operation an `Object.assign` continuation is awaiting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ObjectAssignStage {
    /// Select and snapshot the next non-nullish source.
    NextSource,
    /// Await a Proxy source's `[[OwnPropertyKeys]]` result.
    AwaitKeys,
    /// Recheck and read the next key of the current source.
    NextKey,
    /// Await a Proxy source's current own descriptor.
    AwaitDescriptor,
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

fn continue_object_key_listing_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: ObjectKeyListingContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_object_key_listing(runtime, state, value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::ObjectKeyListing(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::ObjectKeyListing(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Object key listing produced a structured result",
        }
        .into()),
    }
}

fn finish_object_key_listing(
    runtime: &mut Runtime,
    state: ObjectKeyListingContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    Ok(NativeDispatch::Immediate(StoredValue::Object(
        runtime.allocate_array(state.realm, state.elements)?,
    )))
}

fn advance_object_enumerable_keys(
    runtime: &mut Runtime,
    mut state: ObjectKeyListingContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    while state.next < state.keys.len() {
        let key = state.keys[state.next].clone();
        state.next = state.next.saturating_add(1);
        if key
            .as_atom()
            .is_some_and(|atom| atom.kind() != crate::AtomKind::String)
        {
            continue;
        }
        state.stage = ObjectKeyListingStage::AwaitDescriptor;
        let dispatch = begin_internal_get_own_property(
            runtime,
            state.target,
            key,
            state.realm,
            return_to,
            state.origin.clone(),
            execution_budget,
        )?;
        return continue_object_key_listing_after(
            runtime,
            dispatch,
            state,
            return_to,
            execution_budget,
        );
    }
    finish_object_key_listing(runtime, state)
}

pub(super) fn advance_object_key_listing(
    runtime: &mut Runtime,
    mut state: ObjectKeyListingContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ObjectKeyListingStage::AwaitKeys => {
            state.keys = generated_key_list(runtime, completion)?;
            if state.listing == KeyListing::EnumerableOnly {
                return advance_object_enumerable_keys(runtime, state, return_to, execution_budget);
            }
            for key in &state.keys {
                match state.listing {
                    KeyListing::AllStringKeys
                        if key.as_index().is_some()
                            || key
                                .as_atom()
                                .is_some_and(|atom| atom.kind() == crate::AtomKind::String) =>
                    {
                        state
                            .elements
                            .push(StoredValue::String(property_key_string(key)?));
                    }
                    KeyListing::AllSymbolKeys => {
                        if let Some(atom) = key.as_atom()
                            && matches!(
                                atom.kind(),
                                crate::AtomKind::Symbol | crate::AtomKind::GlobalSymbol
                            )
                        {
                            state.elements.push(StoredValue::Symbol(atom.clone()));
                        }
                    }
                    KeyListing::EnumerableOnly | KeyListing::AllStringKeys => {}
                }
            }
            finish_object_key_listing(runtime, state)
        }
        ObjectKeyListingStage::AwaitDescriptor => {
            if let StoredValue::Object(descriptor) = completion {
                let enumerable_key = runtime.predefined_property_key(PredefinedAtom::Enumerable);
                let Some(OwnProperty::Data {
                    value: StoredValue::Boolean(enumerable),
                    ..
                }) =
                    heap_own_property(runtime, HeapReference::Object(descriptor), &enumerable_key)?
                else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "internal descriptor lacks enumerable",
                    }
                    .into());
                };
                if enumerable {
                    let key = state.keys[state.next.saturating_sub(1)].clone();
                    state
                        .elements
                        .push(StoredValue::String(property_key_string(&key)?));
                }
            } else if !matches!(completion, StoredValue::Undefined) {
                return Err(EngineFault::RuntimeInvariant {
                    message: "internal descriptor listing returned a non-descriptor",
                }
                .into());
            }
            advance_object_enumerable_keys(runtime, state, return_to, execution_budget)
        }
    }
}

/// `Object.keys(target)`, `Object.getOwnPropertyNames(target)`, and
/// `Object.getOwnPropertySymbols(target)`.
pub(super) fn own_property_keys(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    listing: KeyListing,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    let Some(reference) = reflection_target(
        runtime,
        realm,
        &target,
        PrimitivePolicy::NoKeys,
        Some(&origin),
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
    if runtime.proxy_state(reference)?.is_some() {
        let state = ObjectKeyListingContinuation {
            target: reference,
            keys: Vec::new(),
            next: 0,
            elements: Vec::new(),
            listing,
            realm,
            origin: origin.clone(),
            stage: ObjectKeyListingStage::AwaitKeys,
        };
        let dispatch = begin_internal_own_keys(
            runtime,
            reference,
            realm,
            return_to,
            origin,
            execution_budget,
        )?;
        return continue_object_key_listing_after(
            runtime,
            dispatch,
            state,
            return_to,
            execution_budget,
        );
    }
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

fn continue_proxy_enumerable_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: ProxyEnumerableContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_proxy_enumerable(runtime, state, value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::ProxyEnumerable(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::ProxyEnumerable(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Proxy enumerable-own-properties produced a structured result",
        }
        .into()),
    }
}

fn advance_proxy_enumerable_key(
    runtime: &mut Runtime,
    mut state: ProxyEnumerableContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    while state.next < state.keys.len() {
        let key = state.keys[state.next].clone();
        state.next = state.next.saturating_add(1);
        if key
            .as_atom()
            .is_some_and(|atom| atom.kind() != crate::AtomKind::String)
        {
            continue;
        }
        state.current_key = Some(key.clone());
        state.stage = ProxyEnumerableStage::Descriptor;
        let dispatch = begin_internal_get_own_property(
            runtime,
            state.target,
            key,
            state.realm,
            return_to,
            state.origin.clone(),
            execution_budget,
        )?;
        return continue_proxy_enumerable_after(
            runtime,
            dispatch,
            state,
            return_to,
            execution_budget,
        );
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(
        runtime.allocate_array(state.realm, state.elements)?,
    )))
}

pub(super) fn advance_proxy_enumerable(
    runtime: &mut Runtime,
    mut state: ProxyEnumerableContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ProxyEnumerableStage::Keys => {
            state.keys = generated_key_list(runtime, completion)?;
            advance_proxy_enumerable_key(runtime, state, return_to, execution_budget)
        }
        ProxyEnumerableStage::Descriptor => {
            let enumerable = if let StoredValue::Object(descriptor) = completion {
                let key = runtime.predefined_property_key(PredefinedAtom::Enumerable);
                let Some(OwnProperty::Data {
                    value: StoredValue::Boolean(enumerable),
                    ..
                }) = heap_own_property(runtime, HeapReference::Object(descriptor), &key)?
                else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "internal Proxy descriptor lacks enumerable",
                    }
                    .into());
                };
                enumerable
            } else if matches!(completion, StoredValue::Undefined) {
                false
            } else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "internal Proxy descriptor is not an object",
                }
                .into());
            };
            if !enumerable {
                state.current_key = None;
                return advance_proxy_enumerable_key(runtime, state, return_to, execution_budget);
            }
            let key = state
                .current_key
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Proxy enumerable scan lost its current key",
                })?;
            state.stage = ProxyEnumerableStage::Value;
            let dispatch = begin_internal_get(
                runtime,
                state.target,
                heap_reference_value(Some(state.target)),
                key.clone(),
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            )?;
            continue_proxy_enumerable_after(runtime, dispatch, state, return_to, execution_budget)
        }
        ProxyEnumerableStage::Value => {
            let key = state
                .current_key
                .take()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Proxy enumerable value completion lost its key",
                })?;
            state.elements.push(enumerable_result_element(
                runtime,
                state.realm,
                state.kind,
                &key,
                completion,
            )?);
            advance_proxy_enumerable_key(runtime, state, return_to, execution_budget)
        }
    }
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

    if runtime.proxy_state(reference)?.is_some() {
        let state = ProxyEnumerableContinuation {
            target: reference,
            keys: Vec::new(),
            next: 0,
            current_key: None,
            elements: Vec::new(),
            kind,
            realm,
            origin: origin.clone(),
            stage: ProxyEnumerableStage::Keys,
        };
        let dispatch = begin_internal_own_keys(
            runtime,
            reference,
            realm,
            return_to,
            origin,
            execution_budget,
        )?;
        return continue_proxy_enumerable_after(
            runtime,
            dispatch,
            state,
            return_to,
            execution_budget,
        );
    }

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

fn continue_object_assign_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: ObjectAssignContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_object_assign(runtime, state, Some(value), return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::ObjectAssign(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::ObjectAssign(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Object.assign nested internal method produced a structured result",
        }
        .into()),
    }
}

/// Resumes the iterative source/key/Get/Set traversal.
#[allow(
    clippy::too_many_lines,
    reason = "the explicit source, key, descriptor, Get, and Set phases keep Object.assign re-entry ordered and auditable"
)]
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
            ObjectAssignStage::AwaitKeys => {
                let value = take_object_assign_completion(&mut completion, "ownKeys")?;
                state.keys = generated_key_list(runtime, value)?;
                state.next_key = 0;
                state.stage = ObjectAssignStage::NextKey;
            }
            ObjectAssignStage::AwaitDescriptor => {
                let descriptor = take_object_assign_completion(&mut completion, "descriptor")?;
                let enumerable = if let StoredValue::Object(descriptor) = descriptor {
                    let key = runtime.predefined_property_key(PredefinedAtom::Enumerable);
                    let Some(OwnProperty::Data {
                        value: StoredValue::Boolean(enumerable),
                        ..
                    }) = heap_own_property(runtime, HeapReference::Object(descriptor), &key)?
                    else {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "Object.assign Proxy descriptor lacks enumerable",
                        }
                        .into());
                    };
                    enumerable
                } else if matches!(descriptor, StoredValue::Undefined) {
                    false
                } else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "Object.assign Proxy descriptor is invalid",
                    }
                    .into());
                };
                if !enumerable {
                    state.current_key = None;
                    state.stage = ObjectAssignStage::NextKey;
                    continue;
                }
                let key = state
                    .current_key
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Object.assign Proxy descriptor lost its key",
                    })?;
                let source = state
                    .source
                    .as_ref()
                    .and_then(StoredValue::heap_reference)
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Object.assign Proxy source was lost",
                    })?;
                state.stage = ObjectAssignStage::AwaitGet;
                let dispatch = begin_internal_get(
                    runtime,
                    source,
                    heap_reference_value(Some(source)),
                    key.clone(),
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                return continue_object_assign_after(
                    runtime,
                    dispatch,
                    state,
                    return_to,
                    execution_budget,
                );
            }
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
                state.source = Some(next);
                state.next_key = 0;
                let source = state.source.as_ref().and_then(StoredValue::heap_reference);
                let source_is_proxy = match source {
                    Some(source) => runtime.proxy_state(source)?.is_some(),
                    None => false,
                };
                if source_is_proxy {
                    let source = source.ok_or(EngineFault::RuntimeInvariant {
                        message: "Object.assign Proxy source disappeared",
                    })?;
                    state.stage = ObjectAssignStage::AwaitKeys;
                    let dispatch = begin_internal_own_keys(
                        runtime,
                        source,
                        state.realm,
                        return_to,
                        state.origin.clone(),
                        execution_budget,
                    )?;
                    return continue_object_assign_after(
                        runtime,
                        dispatch,
                        state,
                        return_to,
                        execution_budget,
                    );
                }
                state.keys = object_assign_source_keys(
                    runtime,
                    state.source.as_ref().ok_or(EngineFault::RuntimeInvariant {
                        message: "Object.assign source disappeared",
                    })?,
                    execution_budget,
                )?;
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
                if let Some(reference) = source.heap_reference()
                    && runtime.proxy_state(reference)?.is_some()
                {
                    state.current_key = Some(key.clone());
                    state.stage = ObjectAssignStage::AwaitDescriptor;
                    let dispatch = begin_internal_get_own_property(
                        runtime,
                        reference,
                        key,
                        state.realm,
                        return_to,
                        state.origin.clone(),
                        execution_budget,
                    )?;
                    return continue_object_assign_after(
                        runtime,
                        dispatch,
                        state,
                        return_to,
                        execution_budget,
                    );
                }
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
    if let Some(reference) = state.target.heap_reference()
        && runtime.proxy_state(reference)?.is_some()
    {
        state.stage = ObjectAssignStage::AwaitSet;
        let dispatch = begin_internal_set(
            runtime,
            reference,
            key,
            name.clone().unwrap_or_else(JsString::empty),
            value,
            state.target.duplicate(),
            true,
            false,
            state.realm,
            return_to,
            state.origin.clone(),
            execution_budget,
        )?;
        return object_assign_after_nested(dispatch, state);
    }
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

/// Starts `Object.getOwnPropertyDescriptors(O)`.
pub(super) fn get_own_property_descriptors(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    if matches!(target, StoredValue::Undefined | StoredValue::Null) {
        return Err(NativeFailure::Abrupt(type_error(
            realm,
            Some(&origin),
            "getOwnPropertyDescriptors",
            "cannot convert to object",
        )?));
    }

    let Some(reference) = target.heap_reference() else {
        let keys = if let StoredValue::String(value) = &target {
            let keys = primitive_string_own_keys(runtime, value)?;
            execution_budget.charge_instructions(usize_to_u64(keys.len()).saturating_add(1))?;
            keys
        } else {
            execution_budget.charge_instructions(1)?;
            Vec::new()
        };
        return materialize_ordinary_own_property_descriptors(
            runtime, realm, &target, keys, &origin,
        );
    };

    let state = GetOwnPropertyDescriptorsContinuation {
        target: reference,
        keys: Vec::new(),
        next: 0,
        current_key: None,
        result: None,
        realm,
        origin: origin.clone(),
        stage: GetOwnPropertyDescriptorsStage::Keys,
    };
    let dispatch = begin_internal_own_keys(
        runtime,
        reference,
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    continue_get_own_property_descriptors_after(
        runtime,
        dispatch,
        state,
        return_to,
        execution_budget,
    )
}

fn continue_get_own_property_descriptors_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: GetOwnPropertyDescriptorsContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_get_own_property_descriptors(runtime, state, value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::GetOwnPropertyDescriptors(Box::new(
                    state,
                ))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::GetOwnPropertyDescriptors(Box::new(
                    state,
                ))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "getOwnPropertyDescriptors internal method produced a structured result",
        }
        .into()),
    }
}

pub(super) fn advance_get_own_property_descriptors(
    runtime: &mut Runtime,
    mut state: GetOwnPropertyDescriptorsContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        GetOwnPropertyDescriptorsStage::Keys => {
            state.keys = generated_key_list(runtime, completion)?;
            state.result = Some(
                runtime.allocate_ordinary_object(runtime.realm_object_prototype(state.realm)?)?,
            );
        }
        GetOwnPropertyDescriptorsStage::Descriptor => {
            let key = state
                .current_key
                .take()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "getOwnPropertyDescriptors descriptor lost its key",
                })?;
            if let Some(own) = internal_complete_own_property(runtime, &completion)? {
                let descriptor = build_descriptor_object(runtime, state.realm, own)?;
                runtime.append_data_property(
                    HeapReference::Object(state.result.ok_or(EngineFault::RuntimeInvariant {
                        message: "getOwnPropertyDescriptors lost its result object",
                    })?),
                    key,
                    PropertyLayout::data(true, true, true),
                    StoredValue::Object(descriptor),
                )?;
            }
        }
    }

    let Some(key) = state.keys.get(state.next).cloned() else {
        return Ok(NativeDispatch::Immediate(StoredValue::Object(
            state.result.ok_or(EngineFault::RuntimeInvariant {
                message: "getOwnPropertyDescriptors completed without a result object",
            })?,
        )));
    };
    state.next = state.next.saturating_add(1);
    state.current_key = Some(key.clone());
    state.stage = GetOwnPropertyDescriptorsStage::Descriptor;
    let dispatch = begin_internal_get_own_property(
        runtime,
        state.target,
        key,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_own_property_descriptors_after(
        runtime,
        dispatch,
        state,
        return_to,
        execution_budget,
    )
}

fn materialize_ordinary_own_property_descriptors(
    runtime: &mut Runtime,
    realm: RealmId,
    target: &StoredValue,
    keys: Vec<PropertyKey>,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let result = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    for key in keys {
        let Some(own) = resolve_own_property(runtime, realm, target, &key, origin)? else {
            continue;
        };
        let descriptor = build_descriptor_object(runtime, realm, own)?;
        runtime.append_data_property(
            HeapReference::Object(result),
            key,
            PropertyLayout::data(true, true, true),
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
