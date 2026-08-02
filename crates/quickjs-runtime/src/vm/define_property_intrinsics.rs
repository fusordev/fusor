/*
 * JavaScript property-descriptor reading derived from QuickJS.
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

//! `Object.defineProperty` and `Object.getOwnPropertyDescriptor`.
//!
//! Reading a descriptor is resumable because each of the six fields is an
//! ordinary property read that can enter a getter. The fields are read in the
//! specification's order — `enumerable`, `configurable`, `value`, `writable`,
//! `get`, `set` — which `ToPropertyDescriptor` fixes and `js_obj_to_desc`
//! (`quickjs.c:39847`) follows, so a descriptor object with side-effecting
//! accessors observes the same sequence.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// Which descriptor field a continuation is awaiting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DescriptorField {
    Enumerable,
    Configurable,
    Value,
    Writable,
    Get,
    Set,
}

impl DescriptorField {
    /// The read order `ToPropertyDescriptor` fixes.
    const ORDER: [Self; 6] = [
        Self::Enumerable,
        Self::Configurable,
        Self::Value,
        Self::Writable,
        Self::Get,
        Self::Set,
    ];

    /// Returns the predefined atom naming this field.
    const fn predefined_atom(self) -> PredefinedAtom {
        match self {
            Self::Enumerable => PredefinedAtom::Enumerable,
            Self::Configurable => PredefinedAtom::Configurable,
            Self::Value => PredefinedAtom::Value,
            Self::Writable => PredefinedAtom::Writable,
            Self::Get => PredefinedAtom::Get,
            Self::Set => PredefinedAtom::SetProperty,
        }
    }
}

/// One in-progress `Object.defineProperty`.
pub(super) struct DefinePropertyContinuation {
    /// The object receiving the definition.
    target: StoredValue,
    /// The descriptor object being read.
    descriptor: StoredValue,
    /// The already-converted property key.
    key: PropertyKey,
    /// The key's name, for the failure message.
    name: JsString,
    /// The fields collected so far.
    fields: CollectedFields,
    /// The index into [`DescriptorField::ORDER`] of the field being awaited.
    next: usize,
    realm: RealmId,
    origin: JsStackFrame,
}

/// The descriptor fields read so far.
///
/// A field is `None` when absent and `Some` when present, which is the
/// distinction the descriptor validation needs: a present `undefined` differs
/// from an absent field.
#[derive(Default)]
struct CollectedFields {
    value: Option<StoredValue>,
    writable: Option<bool>,
    get: Option<StoredValue>,
    set: Option<StoredValue>,
    enumerable: Option<bool>,
    configurable: Option<bool>,
}

impl DefinePropertyContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        // The target, the descriptor, and up to three collected values.
        2_u64
            .saturating_add(u64::from(self.fields.value.is_some()))
            .saturating_add(u64::from(self.fields.get.is_some()))
            .saturating_add(u64::from(self.fields.set.is_some()))
    }

    /// Reports every retained value so cycle collection can trace them.
    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        trace_stored_value_root(&self.descriptor, mark);
        for value in [
            self.fields.value.as_ref(),
            self.fields.get.as_ref(),
            self.fields.set.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            trace_stored_value_root(value, mark);
        }
    }
}

/// Starts `Object.defineProperty(target, key, descriptor)`.
///
/// The target and the descriptor must both be objects; the key has already been
/// converted by the caller's resumable `ToPropertyKey`.
#[allow(
    clippy::too_many_arguments,
    reason = "define-property carries the same runtime, realm, operand, resume, origin, and budget authority as every other resumable native operation"
)]
pub(super) fn begin_define_property(
    runtime: &mut Runtime,
    realm: RealmId,
    target: StoredValue,
    key: PropertyKey,
    name: JsString,
    descriptor: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if !matches!(target, StoredValue::Function(_) | StoredValue::Object(_)) {
        return Err(NativeFailure::Abrupt(descriptor_type_error(
            realm,
            &origin,
            "not an object",
        )?));
    }
    if !matches!(
        descriptor,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        return Err(NativeFailure::Abrupt(descriptor_type_error(
            realm,
            &origin,
            "not an object",
        )?));
    }
    let state = DefinePropertyContinuation {
        target,
        descriptor,
        key,
        name,
        fields: CollectedFields::default(),
        next: 0,
        realm,
        origin,
    };
    advance_define_property(runtime, state, None, return_to, execution_budget)
}

/// Resumes the descriptor read, then applies the definition.
pub(super) fn advance_define_property(
    runtime: &mut Runtime,
    mut state: DefinePropertyContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    // Record the value the previous stage awaited, if any.
    if let Some(value) = completion {
        let field = DescriptorField::ORDER[state.next];
        record_field(&mut state.fields, field, value);
        state.next = state.next.saturating_add(1);
    }

    while state.next < DescriptorField::ORDER.len() {
        let field = DescriptorField::ORDER[state.next];
        let key = runtime.predefined_property_key(field.predefined_atom());
        // An absent field must stay absent rather than becoming a present
        // `undefined`, so presence is tested before the value is read.
        if !has_descriptor_field(runtime, &state.descriptor, &key)? {
            state.next = state.next.saturating_add(1);
            continue;
        }
        charge_heap_property_lookup(runtime, &state.descriptor, execution_budget)?;
        match read_static_property(runtime, state.realm, &state.descriptor, &key)? {
            PropertyReadOutcome::Value(value) => {
                record_field(&mut state.fields, field, value);
                state.next = state.next.saturating_add(1);
            }
            PropertyReadOutcome::Getter { function, receiver } => {
                let mut continuations = Vec::new();
                continuations.try_reserve_exact(1).map_err(|_| {
                    ExecutionError::AllocationFailed {
                        resource: RuntimeResource::Frames,
                        additional: 1,
                    }
                })?;
                continuations.push(NativeContinuation::DefineProperty(Box::new(state)));
                return Ok(NativeDispatch::Call(NativeCall {
                    function,
                    receiver,
                    arguments: CallArguments::empty(),
                    return_to,
                    origin: origin_of(&continuations),
                    continuations,
                    pre_call: None,
                    new_target: None,
                    native_caller: None,
                }));
            }
            PropertyReadOutcome::Failed(failure) => {
                return Err(NativeFailure::Abrupt(property_exception_at(
                    state.realm,
                    state.origin.clone(),
                    Some(&state.name),
                    failure,
                )?));
            }
        }
    }

    apply_collected_descriptor(runtime, state, execution_budget)
}

/// Returns the origin recorded in a pending define-property continuation.
fn origin_of(continuations: &[NativeContinuation]) -> JsStackFrame {
    match continuations.first() {
        Some(NativeContinuation::DefineProperty(state)) => state.origin.clone(),
        _ => native_function_host_origin(),
    }
}

/// Records one read field.
fn record_field(fields: &mut CollectedFields, field: DescriptorField, value: StoredValue) {
    match field {
        DescriptorField::Enumerable => fields.enumerable = Some(value.is_truthy()),
        DescriptorField::Configurable => fields.configurable = Some(value.is_truthy()),
        DescriptorField::Value => fields.value = Some(value),
        DescriptorField::Writable => fields.writable = Some(value.is_truthy()),
        DescriptorField::Get => fields.get = Some(value),
        DescriptorField::Set => fields.set = Some(value),
    }
}

/// Returns whether the descriptor object has the field, including inherited.
fn has_descriptor_field(
    runtime: &Runtime,
    descriptor: &StoredValue,
    key: &PropertyKey,
) -> Result<bool, NativeFailure> {
    let reference = match descriptor {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "descriptor field presence test received a non-object",
            }
            .into());
        }
    };
    Ok(lookup_heap_property(runtime, Some(reference), key)?.is_some())
}

/// Validates the collected fields and applies the definition.
fn apply_collected_descriptor(
    runtime: &mut Runtime,
    state: DefinePropertyContinuation,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let DefinePropertyContinuation {
        target,
        key,
        name,
        fields,
        realm,
        origin,
        ..
    } = state;

    let has_accessor = fields.get.is_some() || fields.set.is_some();
    let has_data = fields.value.is_some() || fields.writable.is_some();
    if has_accessor && has_data {
        return Err(NativeFailure::Abrupt(descriptor_type_error(
            realm,
            &origin,
            "cannot have setter/getter and value or writable",
        )?));
    }

    let definition = if has_accessor {
        let getter = accessor_function(fields.get.as_ref(), realm, &origin, "getter")?;
        let setter = accessor_function(fields.set.as_ref(), realm, &origin, "setter")?;
        PropertyDefinition::accessor(
            requested(fields.get.is_some(), getter),
            requested(fields.set.is_some(), setter),
        )
    } else if has_data {
        PropertyDefinition::data(
            match fields.value {
                Some(value) => Requested::Present(value),
                None => Requested::Absent,
            },
            requested_flag(fields.writable),
        )
    } else {
        PropertyDefinition::generic()
    };
    let definition = definition
        .with_enumerable(requested_flag(fields.enumerable))
        .with_configurable(requested_flag(fields.configurable));

    let outcome = define_own_property(runtime, &target, key, &definition, execution_budget)?;
    match outcome {
        PropertyDefinitionOutcome::Complete => Ok(NativeDispatch::Immediate(target)),
        PropertyDefinitionOutcome::Failed(failure) => Err(NativeFailure::Abrupt(
            property_exception_at(realm, origin, Some(&name), failure)?,
        )),
    }
}

/// Wraps a present-or-absent flag into a descriptor request.
const fn requested_flag(flag: Option<bool>) -> Requested<bool> {
    match flag {
        Some(flag) => Requested::Present(flag),
        None => Requested::Absent,
    }
}

/// Wraps a present-or-absent accessor into a descriptor request.
const fn requested(present: bool, function: Option<FunctionId>) -> Requested<Option<FunctionId>> {
    if present {
        Requested::Present(function)
    } else {
        Requested::Absent
    }
}

/// Validates one accessor field, which must be callable or `undefined`.
fn accessor_function(
    value: Option<&StoredValue>,
    realm: RealmId,
    origin: &JsStackFrame,
    role: &str,
) -> Result<Option<FunctionId>, NativeFailure> {
    match value {
        None | Some(StoredValue::Undefined) => Ok(None),
        Some(StoredValue::Function(function)) => Ok(Some(*function)),
        Some(_) => {
            let message = if role == "getter" {
                "invalid getter"
            } else {
                "invalid setter"
            };
            Err(NativeFailure::Abrupt(descriptor_type_error(
                realm, origin, message,
            )?))
        }
    }
}

/// Builds an engine `TypeError` for a descriptor failure.
fn descriptor_type_error(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    })
}

/// Applies ECMAScript `Object.getOwnPropertyDescriptor`.
///
/// This is `FromPropertyDescriptor`: an own property becomes a fresh ordinary
/// object whose fields are all writable, enumerable, and configurable, and an
/// absent property becomes `undefined`. A primitive target answers through the
/// property it would expose when boxed, so a `String`'s indices and `length` are
/// reported.
pub(super) fn own_property_descriptor(
    runtime: &mut Runtime,
    realm: RealmId,
    target: &StoredValue,
    key: &PropertyKey,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let own = match target {
        StoredValue::Function(function) => {
            own_property_of(runtime, HeapReference::Function(*function), key)?
        }
        StoredValue::Object(object) => {
            own_property_of(runtime, HeapReference::Object(*object), key)?
        }
        // A primitive string exposes its own index and `length` properties.
        StoredValue::String(value) => string_own_property(value, key)?,
        StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::Symbol(_) => None,
        StoredValue::Undefined | StoredValue::Null => {
            return Err(NativeFailure::Abrupt(PendingException {
                realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::TypeError,
                    message: JsString::from_utf8("cannot convert to object")?,
                },
                origin: origin.clone(),
            }));
        }
    };
    let Some(own) = own else {
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    };
    let descriptor = build_descriptor_object(runtime, realm, own)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(descriptor)))
}

/// Reads one own property, consulting the `String` wrapper exotic first.
fn own_property_of(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<Option<OwnProperty>, NativeFailure> {
    if let Some(property) = string_exotic_index_property(runtime, reference, key)? {
        return Ok(Some(property));
    }
    Ok(runtime.object_record(reference)?.own_property(key))
}

/// Builds the own property a primitive string exposes for `key`.
fn string_own_property(
    value: &JsString,
    key: &PropertyKey,
) -> Result<Option<OwnProperty>, NativeFailure> {
    if let Some(index) = key.as_index()
        && index.get() < value.len()
    {
        // A string index is enumerable but neither writable nor configurable.
        return Ok(Some(OwnProperty::Data {
            layout: PropertyLayout::data(false, true, false),
            value: StoredValue::String(value.slice(index.get()..index.get().saturating_add(1))?),
        }));
    }
    if key.as_atom().and_then(crate::Atom::predefined_atom) == Some(PredefinedAtom::Length) {
        return Ok(Some(OwnProperty::Data {
            layout: PropertyLayout::data(false, false, false),
            value: StoredValue::Number(JsNumber::from_u32(value.len())),
        }));
    }
    Ok(None)
}

/// Materializes a descriptor object from an own property.
fn build_descriptor_object(
    runtime: &mut Runtime,
    realm: RealmId,
    own: OwnProperty,
) -> Result<ObjectId, NativeFailure> {
    let prototype = runtime.realm_object_prototype(realm)?;
    let object = runtime.allocate_ordinary_object(prototype)?;
    let reference = HeapReference::Object(object);
    // Every field of a materialized descriptor is fully mutable, which is what
    // makes the result an ordinary object rather than a view.
    let field = PropertyLayout::data(true, true, true);
    let layout = own.layout();
    match own {
        OwnProperty::Data { value, .. } => {
            append_descriptor_field(runtime, reference, PredefinedAtom::Value, field, value)?;
            append_descriptor_field(
                runtime,
                reference,
                PredefinedAtom::Writable,
                field,
                StoredValue::Boolean(layout.writable() == Some(true)),
            )?;
        }
        OwnProperty::Accessor { getter, setter, .. } => {
            append_descriptor_field(
                runtime,
                reference,
                PredefinedAtom::Get,
                field,
                accessor_slot_value(getter),
            )?;
            append_descriptor_field(
                runtime,
                reference,
                PredefinedAtom::SetProperty,
                field,
                accessor_slot_value(setter),
            )?;
        }
    }
    append_descriptor_field(
        runtime,
        reference,
        PredefinedAtom::Enumerable,
        field,
        StoredValue::Boolean(layout.is_enumerable()),
    )?;
    append_descriptor_field(
        runtime,
        reference,
        PredefinedAtom::Configurable,
        field,
        StoredValue::Boolean(layout.is_configurable()),
    )?;
    Ok(object)
}

/// Renders an accessor slot, which is `undefined` when absent.
fn accessor_slot_value(function: Option<FunctionId>) -> StoredValue {
    match function {
        Some(function) => StoredValue::Function(function),
        None => StoredValue::Undefined,
    }
}

/// Appends one descriptor field.
fn append_descriptor_field(
    runtime: &mut Runtime,
    reference: HeapReference,
    name: PredefinedAtom,
    layout: PropertyLayout,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    let key = runtime.predefined_property_key(name);
    runtime.append_data_property(reference, key, layout, value)?;
    Ok(())
}
