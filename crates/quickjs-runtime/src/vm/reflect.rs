/*
 * JavaScript Reflect namespace semantics derived from QuickJS.
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

//! The `Reflect` methods other than `apply` and `construct`.
//!
//! Every method here shares two properties that separate it from the matching
//! `Object` static:
//!
//! * **The target must already be an object.** ECMAScript 2015 relaxed the
//!   `Object` statics to accept primitives, but the `Reflect` mirrors keep the
//!   original `TypeError: not an object`, which the pinned oracle implements as
//!   the `reflect` magic flag in `js_object_isExtensible`,
//!   `js_object_preventExtensions`, `js_object_getPrototypeOf`,
//!   `js_object_defineProperty`, and `js_object_getOwnPropertyDescriptor`
//!   (`quickjs.c:40026-40400`), and as an explicit tag test in the dedicated
//!   `js_reflect_*` entry points (`quickjs.c:50215-50329`). The test precedes
//!   `ToPropertyKey`, so a key whose `toString` throws never runs for a
//!   primitive target.
//! * **A refusal is a `false` answer rather than a `TypeError`.**
//!   `Reflect.set`, `Reflect.defineProperty`, `Reflect.deleteProperty`,
//!   `Reflect.setPrototypeOf`, and `Reflect.preventExtensions` report the
//!   internal method's boolean completion, so `Reflect.set(Object.freeze({a:1}),
//!   'a', 2)` is `false` where the assignment operator would either throw or
//!   silently succeed. `defineProperty` shares one implementation with
//!   `Object.defineProperty` and differs only in dropping the `JS_PROP_THROW`
//!   flag (`quickjs.c:40069-40080`), so a validation failure inside the
//!   descriptor — an invalid getter, or both an accessor and a data field — is
//!   still a `TypeError`.
//!
//! `Reflect.get` and `Reflect.set` also take an optional receiver, which is the
//! `this` an accessor observes and, for `set`, the object a created property
//! lands on. An omitted receiver defaults to the target
//! (`quickjs.c:50246-50249`, `quickjs.c:50289-50292`).

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// Dispatches one `Reflect` method beyond `apply` and `construct`.
///
/// Every method validates its target first, before any key conversion can run a
/// user `toString`. `setPrototypeOf` then validates its prototype argument,
/// which reports the same `not an object` message through
/// `JS_SetPrototypeInternal`'s shared `not_obj` label (`quickjs.c:7897-7922`)
/// but is a throw rather than the method's usual `false` answer.
#[allow(
    clippy::too_many_arguments,
    reason = "reflection shares the same receiver, arguments, limits, and resumption context every native dispatch takes"
)]
pub(super) fn dispatch_reflect_method(
    runtime: &mut Runtime,
    realm: RealmId,
    method: ReflectMethod,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = arguments.take_first_or_undefined();
    let reference = reflect_object_target(&target, realm, &origin)?;
    match method {
        ReflectMethod::SetPrototypeOf => {
            // A non-object prototype is rejected rather than answered `false`:
            // the boolean covers `[[SetPrototypeOf]]`'s refusal, not a bad
            // argument.
            let prototype = match arguments.take_first_or_undefined() {
                StoredValue::Null => None,
                StoredValue::Function(function) => Some(HeapReference::Function(function)),
                StoredValue::Object(object) => Some(HeapReference::Object(object)),
                StoredValue::Undefined
                | StoredValue::Boolean(_)
                | StoredValue::Number(_)
                | StoredValue::BigInt(_)
                | StoredValue::String(_)
                | StoredValue::Symbol(_) => {
                    return Err(not_an_object(realm, &origin)?);
                }
            };
            set_prototype(runtime, reference, prototype)
        }
        ReflectMethod::GetPrototypeOf => {
            let prototype = runtime.object_record(reference)?.prototype();
            Ok(NativeDispatch::Immediate(heap_reference_value(prototype)))
        }
        ReflectMethod::IsExtensible => Ok(NativeDispatch::Immediate(StoredValue::Boolean(
            runtime.is_extensible(reference)?,
        ))),
        // An ordinary object's `[[PreventExtensions]]` always succeeds, so the
        // answer is unconditionally `true`; only a Proxy can refuse, and this
        // profile has none.
        ReflectMethod::PreventExtensions => {
            runtime.prevent_extensions(reference)?;
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)))
        }
        ReflectMethod::OwnKeys => own_keys(runtime, realm, reference, execution_budget),
        ReflectMethod::Get
        | ReflectMethod::Set
        | ReflectMethod::Has
        | ReflectMethod::DeleteProperty
        | ReflectMethod::DefineProperty
        | ReflectMethod::GetOwnPropertyDescriptor => {
            let key = arguments.take_first_or_undefined();
            let keyed = match method {
                ReflectMethod::Get => ReflectKeyedTarget::Get {
                    target,
                    // An absent receiver defaults to the target rather than to
                    // `undefined`, so a two-argument call reads through the
                    // target's own accessors.
                    receiver: arguments.take_first(),
                },
                ReflectMethod::Set => {
                    let value = arguments.take_first_or_undefined();
                    ReflectKeyedTarget::Set {
                        target,
                        value,
                        receiver: arguments.take_first(),
                    }
                }
                ReflectMethod::Has => ReflectKeyedTarget::Has { target },
                ReflectMethod::DeleteProperty => ReflectKeyedTarget::Delete { target },
                ReflectMethod::DefineProperty => ReflectKeyedTarget::Define {
                    target,
                    descriptor: arguments.take_first_or_undefined(),
                },
                ReflectMethod::GetOwnPropertyDescriptor => {
                    ReflectKeyedTarget::OwnPropertyDescriptor { target }
                }
                ReflectMethod::OwnKeys
                | ReflectMethod::GetPrototypeOf
                | ReflectMethod::SetPrototypeOf
                | ReflectMethod::IsExtensible
                | ReflectMethod::PreventExtensions => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "keyless Reflect method reached the keyed dispatch",
                    }
                    .into());
                }
            };
            begin_property_key_conversion(
                runtime,
                key,
                PropertyKeyTarget::Reflect {
                    target: Box::new(keyed),
                    realm,
                },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

/// Resolves a `Reflect` target, which must already be an object.
fn reflect_object_target(
    value: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<HeapReference, NativeFailure> {
    match value {
        StoredValue::Function(function) => Ok(HeapReference::Function(*function)),
        StoredValue::Object(object) => Ok(HeapReference::Object(*object)),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => Err(not_an_object(realm, origin)?),
    }
}

/// Builds the shared `TypeError: not an object` every `Reflect` method reports
/// for a non-object target (`JS_ThrowTypeErrorNotAnObject`).
fn not_an_object(realm: RealmId, origin: &JsStackFrame) -> Result<NativeFailure, NativeFailure> {
    Ok(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not an object")?,
        },
        origin: origin.clone(),
    }))
}

/// Applies `Reflect.setPrototypeOf`, reporting the boolean completion.
fn set_prototype(
    runtime: &mut Runtime,
    reference: HeapReference,
    prototype: Option<HeapReference>,
) -> Result<NativeDispatch, NativeFailure> {
    // A non-extensible object still accepts a redundant write, because
    // `[[SetPrototypeOf]]` compares the current prototype first
    // (`quickjs.c:7941-7943`), so `Reflect.setPrototypeOf` of an unchanged
    // prototype is `true` even when frozen.
    let answered = match runtime.set_prototype_of(reference, prototype)? {
        SetPrototypeOutcome::Complete => true,
        SetPrototypeOutcome::NonExtensible | SetPrototypeOutcome::CyclicPrototype => false,
    };
    Ok(NativeDispatch::Immediate(StoredValue::Boolean(answered)))
}

/// Applies `Reflect.ownKeys`, which reports string *and* symbol keys.
///
/// This is the only key listing in the profile that emits the symbol phase, so
/// `Object.keys` and `Object.getOwnPropertyNames` stay string-only
/// (`JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK`, `quickjs.c:50325-50327`). A
/// symbol key is reported as the Symbol itself rather than as its description.
fn own_keys(
    runtime: &mut Runtime,
    realm: RealmId,
    reference: HeapReference,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (snapshot, work) = runtime.try_own_key_snapshot(reference, 0, KeyPhases::ALL)?;
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
            message: "own-key snapshot shrank during a Reflect.ownKeys listing",
        })?;
        elements.push(own_key_value(candidate.key())?);
    }
    let array = runtime.allocate_array(realm, elements)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(array)))
}

/// Renders one own key as the value `Reflect.ownKeys` reports.
fn own_key_value(key: &PropertyKey) -> Result<StoredValue, NativeFailure> {
    if let Some(index) = key.as_index() {
        return Ok(StoredValue::String(
            JsNumber::from_u32(index.get()).to_radix_string(10)?,
        ));
    }
    let atom = key.as_atom().ok_or(EngineFault::RuntimeInvariant {
        message: "own key is neither an index nor an atom",
    })?;
    match atom.kind() {
        // A symbol key is the Symbol itself, not its description; two symbols
        // sharing a description are still distinct keys.
        AtomKind::Symbol | AtomKind::GlobalSymbol => Ok(StoredValue::Symbol(atom.clone())),
        AtomKind::String => Ok(StoredValue::String(atom.description().cloned().ok_or(
            EngineFault::RuntimeInvariant {
                message: "own string key atom has no description",
            },
        )?)),
        // Private names are not property keys, so `[[OwnPropertyKeys]]` never
        // emits one.
        AtomKind::Private => Err(EngineFault::RuntimeInvariant {
            message: "own-key snapshot emitted a private name",
        }
        .into()),
    }
}

/// Which keyed `Reflect` method a converted key belongs to.
///
/// The target is already known to be an object; only the key still needed a
/// resumable conversion.
pub(super) enum ReflectKeyedTarget {
    Get {
        target: StoredValue,
        receiver: Option<StoredValue>,
    },
    Set {
        target: StoredValue,
        value: StoredValue,
        receiver: Option<StoredValue>,
    },
    Has {
        target: StoredValue,
    },
    Delete {
        target: StoredValue,
    },
    Define {
        target: StoredValue,
        descriptor: StoredValue,
    },
    OwnPropertyDescriptor {
        target: StoredValue,
    },
}

impl ReflectKeyedTarget {
    /// Returns how many stack values the pending conversion retains.
    pub(super) const fn retained_values(&self) -> u64 {
        match self {
            Self::Has { .. } | Self::Delete { .. } | Self::OwnPropertyDescriptor { .. } => 1,
            Self::Get { .. } | Self::Define { .. } => 2,
            Self::Set { .. } => 3,
        }
    }

    /// Reports every retained value as a collection root.
    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::Has { target }
            | Self::Delete { target }
            | Self::OwnPropertyDescriptor { target } => trace_stored_value_root(target, mark),
            Self::Get { target, receiver } => {
                trace_stored_value_root(target, mark);
                if let Some(receiver) = receiver {
                    trace_stored_value_root(receiver, mark);
                }
            }
            Self::Define { target, descriptor } => {
                trace_stored_value_root(target, mark);
                trace_stored_value_root(descriptor, mark);
            }
            Self::Set {
                target,
                value,
                receiver,
            } => {
                trace_stored_value_root(target, mark);
                trace_stored_value_root(value, mark);
                if let Some(receiver) = receiver {
                    trace_stored_value_root(receiver, mark);
                }
            }
        }
    }
}

/// Completes one keyed `Reflect` method once its key has been converted.
pub(super) fn finish_reflect_keyed_target(
    runtime: &mut Runtime,
    realm: RealmId,
    target: ReflectKeyedTarget,
    property: StaticPropertyOperand,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match target {
        ReflectKeyedTarget::Get { target, receiver } => {
            let receiver = receiver.unwrap_or_else(|| target.duplicate());
            reflect_get(runtime, &target, receiver, &property.key, return_to, origin)
        }
        ReflectKeyedTarget::Set {
            target,
            value,
            receiver,
        } => {
            let receiver = receiver.unwrap_or_else(|| target.duplicate());
            reflect_set(
                runtime,
                realm,
                &target,
                receiver,
                value,
                property,
                return_to,
                origin,
                execution_budget,
            )
        }
        ReflectKeyedTarget::Has { target } => Ok(NativeDispatch::Immediate(StoredValue::Boolean(
            has_property(runtime, realm, &target, &property.key)?,
        ))),
        // A refused delete is `false`, not the `delete` operator's strict-mode
        // `TypeError` (`quickjs.c:50228-50233`).
        ReflectKeyedTarget::Delete { target } => {
            match delete_static_property(runtime, &target, &property.key)? {
                PropertyDeleteOutcome::Deleted => {
                    Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)))
                }
                PropertyDeleteOutcome::Refused => {
                    Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
                }
                PropertyDeleteOutcome::Failed(failure) => Err(NativeFailure::Abrupt(
                    property_exception_at(realm, origin.clone(), Some(&property.name), failure)?,
                )),
            }
        }
        ReflectKeyedTarget::Define { target, descriptor } => begin_define_property(
            runtime,
            realm,
            target,
            property.key,
            property.name,
            descriptor,
            DefinitionReport::Boolean,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        ReflectKeyedTarget::OwnPropertyDescriptor { target } => {
            own_property_descriptor(runtime, realm, &target, &property.key, origin)
        }
    }
}

/// Applies `Reflect.get`, running any accessor with the requested receiver.
///
/// A `Reflect.get` target is always an object, so the read itself cannot fail;
/// only an accessor can suspend, and it runs with the requested receiver as its
/// `this` (`JS_GetPropertyInternal` with a distinct receiver,
/// `quickjs.c:50253`).
fn reflect_get(
    runtime: &mut Runtime,
    target: &StoredValue,
    receiver: StoredValue,
    key: &PropertyKey,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let reference = heap_reference_of(target)?;
    match read_heap_property_for_receiver(runtime, reference, receiver, key)? {
        PropertyReadOutcome::Value(value) => Ok(NativeDispatch::Immediate(value)),
        PropertyReadOutcome::Getter { function, receiver } => {
            Ok(NativeDispatch::Call(NativeCall {
                function,
                receiver,
                arguments: CallArguments::empty(),
                return_to,
                origin: origin.clone(),
                continuations: Vec::new(),
                pre_call: None,
                new_target: None,
                native_caller: None,
            }))
        }
        PropertyReadOutcome::Failed(_) => Err(EngineFault::RuntimeInvariant {
            message: "Reflect.get target passed its object check but failed its read",
        }
        .into()),
    }
}

/// Applies `Reflect.set`, reporting the boolean completion.
///
/// This is `OrdinarySet` with a distinct receiver, which upstream reaches by
/// passing `this_obj != obj` into `JS_SetPropertyInternal`
/// (`quickjs.c:9663-9930`). The two objects play different roles:
///
/// * The **target** supplies the property lookup. Its own property, or the first
///   one found walking its prototype chain, can already decide the outcome: an
///   accessor's setter is called with the receiver as `this`, a non-writable
///   data property refuses, and an exotic `String` index refuses.
/// * The **receiver** stores the result when the lookup finds nothing to call.
///   Its own property is re-validated there, so an accessor or a non-writable
///   data property on the receiver refuses even when the target's was writable,
///   and a non-extensible receiver refuses to gain a new one.
///
/// When the two are the same object the operation collapses onto the ordinary
/// assignment path, which already implements the array `length` and dense-index
/// exotics.
#[allow(
    clippy::too_many_arguments,
    reason = "a receiver-aware write carries the target, receiver, value, key, resume, origin, and budget authority together"
)]
fn reflect_set(
    runtime: &mut Runtime,
    realm: RealmId,
    target: &StoredValue,
    receiver: StoredValue,
    value: StoredValue,
    property: StaticPropertyOperand,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match plan_reflect_set(runtime, target, &receiver, &property.key)? {
        ReflectSetPlan::Refused => Ok(NativeDispatch::Immediate(StoredValue::Boolean(false))),
        ReflectSetPlan::Setter { function } => {
            reflect_setter_call(function, receiver, value, return_to, origin)
        }
        // An array's `length` converts with `ToNumber` before its range check,
        // which reports `RangeError: invalid array length` rather than `false`,
        // so the conversion re-enters through the resumable operator machinery
        // on whichever object actually stores the length.
        ReflectSetPlan::ArrayLength { base } => {
            let conversion =
                array_length_write_target(base, property.name, LengthWriteReport::Boolean, &value);
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::Number,
                conversion,
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        ReflectSetPlan::Define { base } => {
            // A created property is fully mutable, which is what
            // `CreateDataProperty` produces.
            let definition =
                PropertyDefinition::data(Requested::Present(value), Requested::Present(true))
                    .with_enumerable(Requested::Present(true))
                    .with_configurable(Requested::Present(true));
            let outcome =
                define_own_property(runtime, &base, property.key, &definition, execution_budget)?;
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(matches!(
                outcome,
                PropertyDefinitionOutcome::Complete
            ))))
        }
        // An *existing* receiver property is updated by value alone, so it keeps
        // its own attributes: `[[Set]]` passes only `[[Value]]` into
        // `[[DefineOwnProperty]]` (`JS_PROP_HAS_VALUE`, `quickjs.c:9912-9915`).
        ReflectSetPlan::Update { base } => {
            let definition = PropertyDefinition::data(Requested::Present(value), Requested::Absent);
            let outcome =
                define_own_property(runtime, &base, property.key, &definition, execution_budget)?;
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(matches!(
                outcome,
                PropertyDefinitionOutcome::Complete
            ))))
        }
        ReflectSetPlan::Assign { base } => {
            // The strict flag makes a refusal visible as `Failed` rather than
            // the sloppy write's silent `Complete`, which is exactly what the
            // boolean answer needs to distinguish.
            match write_static_property(
                runtime,
                realm,
                &base,
                property.key,
                value,
                true,
                execution_budget,
            )? {
                PropertyWriteOutcome::Complete => {
                    Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)))
                }
                PropertyWriteOutcome::Setter {
                    function,
                    receiver,
                    value,
                } => reflect_setter_call(function, receiver, value, return_to, origin),
                PropertyWriteOutcome::Failed(_) => {
                    Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
                }
            }
        }
    }
}

/// Calls one setter on behalf of `Reflect.set`.
///
/// The setter's own completion is discarded: `Reflect.set` answers `true` as
/// soon as the call returns normally, which the `ReflectTrue` continuation
/// supplies.
fn reflect_setter_call(
    function: FunctionId,
    receiver: StoredValue,
    value: StoredValue,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 1,
        })?;
    arguments.push(value);
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::ReflectTrue);
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin: origin.clone(),
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

/// What a `Reflect.set` resolves to before its value is consumed.
enum ReflectSetPlan {
    /// The lookup refused: a non-writable data property, an accessor without a
    /// setter, an accessor own property on a differing receiver, or a receiver
    /// that cannot hold the result.
    Refused,
    /// A setter must run with the receiver as `this`.
    Setter { function: FunctionId },
    /// The write lands on an array's `length`, whose numeric conversion and
    /// range check are resumable.
    ArrayLength { base: StoredValue },
    /// The receiver gains a new own data property through
    /// `[[DefineOwnProperty]]`, which is the receiver-differs branch.
    Define { base: StoredValue },
    /// The receiver already has a writable own data property, which is updated
    /// by value alone so its attributes survive.
    Update { base: StoredValue },
    /// The receiver is the target, so the ordinary assignment path applies.
    Assign { base: StoredValue },
}

/// Resolves a `Reflect.set` before its value is consumed.
///
/// The target's lookup runs first and can already decide the answer; only when
/// it finds nothing callable does the receiver's own state matter.
fn plan_reflect_set(
    runtime: &Runtime,
    target: &StoredValue,
    receiver: &StoredValue,
    key: &PropertyKey,
) -> Result<ReflectSetPlan, NativeFailure> {
    let target_reference = heap_reference_of(target)?;
    let same_object = match receiver {
        StoredValue::Function(function) => HeapReference::Function(*function) == target_reference,
        StoredValue::Object(object) => HeapReference::Object(*object) == target_reference,
        // A primitive receiver can never store a property, so a write that would
        // have to create one refuses; a setter found on the target still runs
        // with the primitive as its `this`.
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => false,
    };
    if same_object {
        return Ok(if is_array_length_target(runtime, target, key)? {
            ReflectSetPlan::ArrayLength {
                base: target.duplicate(),
            }
        } else {
            ReflectSetPlan::Assign {
                base: target.duplicate(),
            }
        });
    }

    // The target's chain decides first. A `String` wrapper's index is exotic and
    // non-writable, so it refuses before the receiver is consulted.
    if let Some(found) = lookup_heap_property(runtime, Some(target_reference), key)? {
        match found {
            OwnProperty::Accessor { setter, .. } => {
                return Ok(match setter {
                    Some(function) => ReflectSetPlan::Setter { function },
                    None => ReflectSetPlan::Refused,
                });
            }
            OwnProperty::Data { layout, .. } if layout.writable() != Some(true) => {
                return Ok(ReflectSetPlan::Refused);
            }
            OwnProperty::Data { .. } => {}
        }
    }

    let Ok(receiver_reference) = heap_reference_of(receiver) else {
        // Nothing callable was found and the receiver cannot hold a property.
        return Ok(ReflectSetPlan::Refused);
    };
    // An array `length` on the receiver keeps its resumable conversion, whose
    // `RangeError` outranks the boolean answer.
    if is_array_length_target(runtime, receiver, key)? {
        return Ok(ReflectSetPlan::ArrayLength {
            base: receiver.duplicate(),
        });
    }
    // The receiver's own property is re-validated: an accessor there is
    // `setter is forbidden` and a non-writable data property is read-only, both
    // of which answer `false` (`quickjs.c:9893-9911`).
    if let Some(own) = own_property_for_set(runtime, receiver_reference, key)? {
        return Ok(match own {
            OwnProperty::Data { layout, .. } if layout.writable() == Some(true) => {
                ReflectSetPlan::Update {
                    base: receiver.duplicate(),
                }
            }
            OwnProperty::Data { .. } | OwnProperty::Accessor { .. } => ReflectSetPlan::Refused,
        });
    }
    if !runtime.object_record(receiver_reference)?.is_extensible() {
        return Ok(ReflectSetPlan::Refused);
    }
    Ok(ReflectSetPlan::Define {
        base: receiver.duplicate(),
    })
}

/// Reads one own property of a differing `Reflect.set` receiver, consulting the
/// `String` wrapper exotic first so an in-range index refuses.
fn own_property_for_set(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<Option<OwnProperty>, NativeFailure> {
    if let Some(property) = string_exotic_index_property(runtime, reference, key)? {
        return Ok(Some(property));
    }
    Ok(runtime.object_record(reference)?.own_property(key))
}

/// Resolves a heap reference from a value already known to be an object.
fn heap_reference_of(value: &StoredValue) -> Result<HeapReference, NativeFailure> {
    match value {
        StoredValue::Function(function) => Ok(HeapReference::Function(*function)),
        StoredValue::Object(object) => Ok(HeapReference::Object(*object)),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => Err(EngineFault::RuntimeInvariant {
            message: "validated Reflect target is not an object",
        }
        .into()),
    }
}
