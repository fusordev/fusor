/*
 * JavaScript reflection semantics derived from QuickJS.
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

//! ECMAScript `%Reflect%` methods over the runtime's ordinary object model.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// Dispatches one `%Reflect%` method in the ECMA-262 algorithm order.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the Reflect dispatcher keeps method-specific validation order and resumable key/list conversion at one audited boundary"
)]
pub(super) fn begin_reflect_method(
    runtime: &mut Runtime,
    realm: RealmId,
    method: ReflectMethod,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    active_frames: usize,
    active_frame_values: u64,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        ReflectMethod::Apply => {
            let target = arguments.take_first_or_undefined();
            let target = require_callable(&target, realm, &origin, "not a function")?;
            let receiver = arguments.take_first_or_undefined();
            let list = arguments.take_first_or_undefined();
            begin_array_like_call(
                runtime,
                realm,
                target,
                receiver,
                list,
                return_to,
                origin,
                active_frames,
                active_frame_values,
                execution_budget,
                None,
                None,
                false,
            )
        }
        ReflectMethod::Construct => begin_reflect_construct(
            runtime,
            realm,
            arguments,
            return_to,
            origin,
            active_frames,
            active_frame_values,
            execution_budget,
        ),
        ReflectMethod::DefineProperty => {
            let target = arguments.take_first_or_undefined();
            require_object(&target, realm, &origin)?;
            let key = arguments.take_first_or_undefined();
            let descriptor = arguments.take_first_or_undefined();
            begin_property_key_conversion(
                runtime,
                key,
                PropertyKeyTarget::ReflectDefineProperty {
                    target,
                    descriptor,
                    realm,
                },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        ReflectMethod::DeleteProperty => {
            let target = arguments.take_first_or_undefined();
            require_object(&target, realm, &origin)?;
            let key = arguments.take_first_or_undefined();
            begin_property_key_conversion(
                runtime,
                key,
                PropertyKeyTarget::Delete {
                    base: target,
                    strict: false,
                    realm,
                },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        ReflectMethod::Get => {
            let target = arguments.take_first_or_undefined();
            require_object(&target, realm, &origin)?;
            let key = arguments.take_first_or_undefined();
            let receiver = arguments.take_first().unwrap_or_else(|| target.duplicate());
            begin_property_key_conversion(
                runtime,
                key,
                PropertyKeyTarget::ReflectGet { target, receiver },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        ReflectMethod::GetOwnPropertyDescriptor => {
            let target = arguments.take_first_or_undefined();
            require_object(&target, realm, &origin)?;
            let key = arguments.take_first_or_undefined();
            begin_property_key_conversion(
                runtime,
                key,
                PropertyKeyTarget::ReflectOwnPropertyDescriptor { target, realm },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        ReflectMethod::GetPrototypeOf => {
            let target = arguments.take_first_or_undefined();
            let reference = require_object(&target, realm, &origin)?;
            let prototype = runtime.object_record(reference)?.prototype();
            Ok(NativeDispatch::Immediate(heap_reference_value(prototype)))
        }
        ReflectMethod::Has => {
            let target = arguments.take_first_or_undefined();
            require_object(&target, realm, &origin)?;
            let key = arguments.take_first_or_undefined();
            begin_property_key_conversion(
                runtime,
                key,
                PropertyKeyTarget::ReflectHas { target, realm },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        ReflectMethod::IsExtensible => {
            let target = arguments.take_first_or_undefined();
            let reference = require_object(&target, realm, &origin)?;
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(
                runtime.is_extensible(reference)?,
            )))
        }
        ReflectMethod::OwnKeys => {
            let target = arguments.take_first_or_undefined();
            let reference = require_object(&target, realm, &origin)?;
            reflect_own_keys(runtime, realm, reference, execution_budget)
        }
        ReflectMethod::PreventExtensions => {
            let target = arguments.take_first_or_undefined();
            let reference = require_object(&target, realm, &origin)?;
            runtime.prevent_extensions(reference)?;
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)))
        }
        ReflectMethod::Set => {
            let target = arguments.take_first_or_undefined();
            require_object(&target, realm, &origin)?;
            let key = arguments.take_first_or_undefined();
            let value = arguments.take_first_or_undefined();
            let receiver = arguments.take_first().unwrap_or_else(|| target.duplicate());
            begin_property_key_conversion(
                runtime,
                key,
                PropertyKeyTarget::ReflectSet {
                    target,
                    receiver,
                    value,
                    realm,
                },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        ReflectMethod::SetPrototypeOf => {
            let target = arguments.take_first_or_undefined();
            let reference = require_object(&target, realm, &origin)?;
            let prototype = match arguments.take_first_or_undefined() {
                StoredValue::Null => None,
                StoredValue::Function(function) => Some(HeapReference::Function(function)),
                StoredValue::Object(object) => Some(HeapReference::Object(object)),
                _ => return Err(reflect_type_error(realm, &origin, "not an object")),
            };
            let changed = matches!(
                runtime.set_prototype_of(reference, prototype)?,
                SetPrototypeOutcome::Complete
            );
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(changed)))
        }
    }
}

fn require_callable(
    value: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<FunctionId, NativeFailure> {
    match value {
        StoredValue::Function(function) => Ok(*function),
        _ => Err(reflect_type_error(realm, origin, message)),
    }
}

fn require_object(
    value: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<HeapReference, NativeFailure> {
    value
        .heap_reference()
        .ok_or_else(|| reflect_type_error(realm, origin, "not an object"))
}

fn reflect_type_error(realm: RealmId, origin: &JsStackFrame, message: &str) -> NativeFailure {
    match JsString::from_utf8(message) {
        Ok(message) => NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message,
            },
            origin: origin.clone(),
        }),
        Err(error) => NativeFailure::Execution(error.into()),
    }
}

fn heap_reference_value(reference: Option<HeapReference>) -> StoredValue {
    match reference {
        None => StoredValue::Null,
        Some(HeapReference::Function(function)) => StoredValue::Function(function),
        Some(HeapReference::Object(object)) => StoredValue::Object(object),
    }
}

fn reflect_own_keys(
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
            message: "Reflect.ownKeys snapshot shrank during materialization",
        })?;
        let key = candidate.key();
        let value = if let Some(index) = key.as_index() {
            StoredValue::String(JsNumber::from_u32(index.get()).to_radix_string(10)?)
        } else {
            let atom = key.as_atom().ok_or(EngineFault::RuntimeInvariant {
                message: "Reflect.ownKeys candidate is neither an index nor an atom",
            })?;
            match atom.kind() {
                crate::AtomKind::String => StoredValue::String(atom.description().cloned().ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "Reflect.ownKeys string atom has no description",
                    },
                )?),
                crate::AtomKind::Symbol | crate::AtomKind::GlobalSymbol => {
                    StoredValue::Symbol(atom.clone())
                }
                crate::AtomKind::Private => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "private name escaped through Reflect.ownKeys",
                    }
                    .into());
                }
            }
        };
        elements.push(value);
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(
        runtime.allocate_array(realm, elements)?,
    )))
}

/// Applies `OrdinarySet` with an explicit receiver and returns the internal
/// method Boolean rather than turning a rejected write into an exception.
#[allow(
    clippy::too_many_arguments,
    reason = "ordinary set keeps the target, receiver, key, value, resume target, origin, and budget explicit"
)]
pub(super) fn reflect_set_property(
    runtime: &mut Runtime,
    realm: RealmId,
    target: StoredValue,
    key: PropertyKey,
    name: JsString,
    value: StoredValue,
    receiver: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if target.strict_equals(&receiver) {
        if is_array_length_target(runtime, &target, &key)? {
            let conversion = array_length_write_target(target, name, false, true, &value);
            return begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::Number,
                conversion,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        return reflect_set_outcome(
            write_static_property(runtime, realm, &target, key, value, true, execution_budget)?,
            return_to,
            origin,
        );
    }

    let reference = target
        .heap_reference()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Reflect.set lost its validated target",
        })?;
    let descriptor = lookup_heap_property(runtime, Some(reference), &key)?;
    if let Some(OwnProperty::Accessor { setter, .. }) = &descriptor {
        return match setter {
            Some(function) => reflect_setter_call(*function, receiver, value, return_to, origin),
            None => Ok(NativeDispatch::Immediate(StoredValue::Boolean(false))),
        };
    }
    if descriptor.as_ref().is_some_and(|descriptor| {
        matches!(descriptor, OwnProperty::Data { layout, .. } if layout.writable() != Some(true))
    }) {
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
    }
    let Some(receiver_reference) = receiver.heap_reference() else {
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
    };
    match own_property_of(runtime, receiver_reference, &key)? {
        Some(OwnProperty::Accessor { .. }) => {
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
        }
        Some(OwnProperty::Data { layout, .. }) if layout.writable() != Some(true) => {
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
        }
        Some(OwnProperty::Data { .. }) => {
            let definition = PropertyDefinition::data(Requested::Present(value), Requested::Absent);
            let outcome =
                define_own_property(runtime, &receiver, key, &definition, execution_budget)?;
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(matches!(
                outcome,
                PropertyDefinitionOutcome::Complete
            ))))
        }
        None => reflect_set_outcome(
            define_static_property(runtime, &receiver, key, value, execution_budget)?,
            return_to,
            origin,
        ),
    }
}

fn reflect_set_outcome(
    outcome: PropertyWriteOutcome,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    match outcome {
        PropertyWriteOutcome::Complete => Ok(NativeDispatch::Immediate(StoredValue::Boolean(true))),
        PropertyWriteOutcome::Failed(_) => {
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
        }
        PropertyWriteOutcome::Setter {
            function,
            receiver,
            value,
        } => reflect_setter_call(function, receiver, value, return_to, origin),
    }
}

fn reflect_setter_call(
    function: FunctionId,
    receiver: StoredValue,
    value: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 1,
        })?;
    values.push(value);
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::ReflectSet);
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::from_values(values),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}
