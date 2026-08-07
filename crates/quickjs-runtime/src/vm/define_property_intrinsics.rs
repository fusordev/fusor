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

//! Property descriptor conversion for `Object.defineProperty`,
//! `Object.defineProperties`, descriptor-bearing `Object.create`, and
//! `Object.getOwnPropertyDescriptor`.
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
    /// The already-converted property key.
    key: PropertyKey,
    /// The key's name, for the failure message.
    name: JsString,
    /// The reusable `ToPropertyDescriptor` reader.
    reader: DescriptorReadState,
    realm: RealmId,
    origin: JsStackFrame,
    result: DefinePropertyResult,
}

/// Which phase of `ObjectDefineProperties` is awaiting re-entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DefinePropertiesStage {
    AwaitKeys,
    NextKey,
    AwaitOwnDescriptor,
    AwaitDescriptorValue,
    ReadDescriptor,
    Apply,
    AwaitDefinition,
}

/// One converted descriptor paired with its original property key.
struct CollectedDefinition {
    key: PropertyKey,
    fields: CollectedFields,
}

/// One resumable `ObjectDefineProperties` operation shared by
/// `Object.defineProperties` and the second argument of `Object.create`.
pub(super) struct DefinePropertiesContinuation {
    target: StoredValue,
    properties: StoredValue,
    keys: Vec<PropertyKey>,
    next_key: usize,
    pending_key: Option<PropertyKey>,
    reader: Option<DescriptorReadState>,
    definitions: Vec<Option<CollectedDefinition>>,
    next_definition: usize,
    realm: RealmId,
    origin: JsStackFrame,
    stage: DefinePropertiesStage,
}

/// Whether a completed definition returns its target or the internal-method
/// Boolean. `Object.defineProperty` uses the former; `Reflect.defineProperty`
/// uses the latter and reports an ordinary rejection as `false`.
#[derive(Clone, Copy)]
pub(super) enum DefinePropertyResult {
    Target,
    Boolean,
}

/// The descriptor fields read so far.
///
/// A field is `None` when absent and `Some` when present, which is the
/// distinction the descriptor validation needs: a present `undefined` differs
/// from an absent field.
#[derive(Default)]
pub(super) struct CollectedFields {
    value: Option<StoredValue>,
    writable: Option<bool>,
    get: Option<StoredValue>,
    set: Option<StoredValue>,
    enumerable: Option<bool>,
    configurable: Option<bool>,
}

impl CollectedFields {
    fn retained_values(&self) -> u64 {
        u64::from(self.value.is_some())
            .saturating_add(u64::from(self.get.is_some()))
            .saturating_add(u64::from(self.set.is_some()))
    }

    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        for value in [self.value.as_ref(), self.get.as_ref(), self.set.as_ref()]
            .into_iter()
            .flatten()
        {
            trace_stored_value_root(value, mark);
        }
    }
}

/// The reusable, resumable part of `ToPropertyDescriptor`.
pub(super) struct DescriptorReadState {
    descriptor: StoredValue,
    fields: CollectedFields,
    next: usize,
    phase: DescriptorReadPhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DescriptorReadPhase {
    Next,
    AwaitHas,
    AwaitGet,
}

impl DescriptorReadState {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(self.fields.retained_values())
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.descriptor, mark);
        self.fields.trace_roots(mark);
    }
}

pub(super) enum DescriptorReadOutcome {
    Complete(CollectedFields),
    Nested(Box<NativeDispatch>),
}

impl DefinePropertyContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(self.reader.retained_values())
    }

    /// Reports every retained value so cycle collection can trace them.
    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        self.reader.trace_roots(mark);
    }
}

impl DefinePropertiesContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        let definitions = self
            .definitions
            .iter()
            .flatten()
            .fold(0_u64, |retained, definition| {
                retained.saturating_add(definition.fields.retained_values())
            });
        2_u64
            .saturating_add(usize_to_u64(self.keys.len()))
            .saturating_add(u64::from(self.pending_key.is_some()))
            .saturating_add(
                self.reader
                    .as_ref()
                    .map_or(0, DescriptorReadState::retained_values),
            )
            .saturating_add(definitions)
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        trace_stored_value_root(&self.properties, mark);
        if let Some(reader) = &self.reader {
            reader.trace_roots(mark);
        }
        for definition in self.definitions.iter().flatten() {
            definition.fields.trace_roots(mark);
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
    begin_define_property_with_result(
        runtime,
        realm,
        target,
        key,
        name,
        descriptor,
        return_to,
        origin,
        execution_budget,
        DefinePropertyResult::Target,
    )
}

/// Starts descriptor collection with the caller-selected completion shape.
#[allow(
    clippy::too_many_arguments,
    reason = "Reflect.defineProperty shares the resumable descriptor reader but selects a Boolean completion"
)]
pub(super) fn begin_define_property_with_result(
    runtime: &mut Runtime,
    realm: RealmId,
    target: StoredValue,
    key: PropertyKey,
    name: JsString,
    descriptor: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
    result: DefinePropertyResult,
) -> Result<NativeDispatch, NativeFailure> {
    if !matches!(target, StoredValue::Function(_) | StoredValue::Object(_)) {
        return Err(NativeFailure::Abrupt(descriptor_type_error(
            realm,
            &origin,
            "not an object",
        )?));
    }
    let state = DefinePropertyContinuation {
        target,
        key,
        name,
        reader: begin_descriptor_read(descriptor, realm, &origin)?,
        realm,
        origin,
        result,
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
    match advance_descriptor_read(
        runtime,
        &mut state.reader,
        completion,
        state.realm,
        &state.origin,
        return_to,
        execution_budget,
    )? {
        DescriptorReadOutcome::Complete(fields) => {
            apply_collected_descriptor(runtime, state, fields, return_to, execution_budget)
        }
        DescriptorReadOutcome::Nested(dispatch) => continue_descriptor_nested(
            *dispatch,
            NativeContinuation::DefineProperty(Box::new(state)),
        ),
    }
}

pub(super) fn continue_descriptor_nested(
    dispatch: NativeDispatch,
    continuation: NativeContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(&mut call, vec![continuation])?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(&mut frame, vec![continuation])?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Immediate(_)
        | NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "descriptor nested operation returned an invalid dispatch",
        }
        .into()),
    }
}

pub(super) fn begin_descriptor_read(
    descriptor: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<DescriptorReadState, NativeFailure> {
    if !matches!(
        descriptor,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        return Err(NativeFailure::Abrupt(descriptor_type_error(
            realm,
            origin,
            "not an object",
        )?));
    }
    Ok(DescriptorReadState {
        descriptor,
        fields: CollectedFields::default(),
        next: 0,
        phase: DescriptorReadPhase::Next,
    })
}

pub(super) fn advance_descriptor_read(
    runtime: &mut Runtime,
    state: &mut DescriptorReadState,
    completion: Option<StoredValue>,
    realm: RealmId,
    origin: &JsStackFrame,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<DescriptorReadOutcome, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.phase {
            DescriptorReadPhase::AwaitHas => {
                let Some(StoredValue::Boolean(present)) = completion.take() else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "descriptor HasProperty did not return a Boolean",
                    }
                    .into());
                };
                if !present {
                    state.next = state.next.saturating_add(1);
                    state.phase = DescriptorReadPhase::Next;
                    continue;
                }
                state.phase = DescriptorReadPhase::AwaitGet;
            }
            DescriptorReadPhase::AwaitGet => {
                if let Some(value) = completion.take() {
                    let field = DescriptorField::ORDER.get(state.next).copied().ok_or(
                        EngineFault::RuntimeInvariant {
                            message: "descriptor Get resumed after its final field",
                        },
                    )?;
                    record_field(&mut state.fields, field, value, realm, origin)?;
                    state.next = state.next.saturating_add(1);
                    state.phase = DescriptorReadPhase::Next;
                    continue;
                }
                let field = DescriptorField::ORDER.get(state.next).copied().ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "descriptor Get started after its final field",
                    },
                )?;
                let reference =
                    state
                        .descriptor
                        .heap_reference()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "descriptor reader lost its object",
                        })?;
                execution_budget.charge_instructions(1)?;
                let dispatch = begin_internal_get(
                    runtime,
                    reference,
                    state.descriptor.duplicate(),
                    runtime.predefined_property_key(field.predefined_atom()),
                    realm,
                    return_to,
                    origin.clone(),
                    execution_budget,
                )?;
                match dispatch {
                    NativeDispatch::Immediate(value) => {
                        completion = Some(value);
                    }
                    dispatch => return Ok(DescriptorReadOutcome::Nested(Box::new(dispatch))),
                }
            }
            DescriptorReadPhase::Next => {
                if state.next >= DescriptorField::ORDER.len() {
                    return Ok(DescriptorReadOutcome::Complete(std::mem::take(
                        &mut state.fields,
                    )));
                }
                let field = DescriptorField::ORDER[state.next];
                let reference =
                    state
                        .descriptor
                        .heap_reference()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "descriptor reader lost its object",
                        })?;
                state.phase = DescriptorReadPhase::AwaitHas;
                execution_budget.charge_instructions(1)?;
                let dispatch = begin_internal_has(
                    runtime,
                    reference,
                    runtime.predefined_property_key(field.predefined_atom()),
                    realm,
                    return_to,
                    origin.clone(),
                    execution_budget,
                )?;
                match dispatch {
                    NativeDispatch::Immediate(value) => completion = Some(value),
                    dispatch => return Ok(DescriptorReadOutcome::Nested(Box::new(dispatch))),
                }
            }
        }
    }
}

/// Records one read field.
fn record_field(
    fields: &mut CollectedFields,
    field: DescriptorField,
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<(), NativeFailure> {
    match field {
        DescriptorField::Enumerable => fields.enumerable = Some(value.is_truthy()),
        DescriptorField::Configurable => fields.configurable = Some(value.is_truthy()),
        DescriptorField::Value => fields.value = Some(value),
        DescriptorField::Writable => fields.writable = Some(value.is_truthy()),
        DescriptorField::Get => {
            let _ = accessor_function(Some(&value), realm, origin, "getter")?;
            fields.get = Some(value);
        }
        DescriptorField::Set => {
            let _ = accessor_function(Some(&value), realm, origin, "setter")?;
            fields.set = Some(value);
        }
    }
    Ok(())
}

/// Validates the collected fields and applies the definition.
fn apply_collected_descriptor(
    runtime: &mut Runtime,
    state: DefinePropertyContinuation,
    mut fields: CollectedFields,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let DefinePropertyContinuation {
        target,
        key,
        name,
        realm,
        origin,
        result,
        ..
    } = state;

    validate_collected_fields(&fields, realm, &origin)?;

    // ArraySetLength converts a present `value` twice before descriptor
    // validation or mutation. Keep that work in the existing resumable Array
    // length conversion state machine so an object value can re-enter script at
    // both specification-mandated conversion points.
    if is_array_length_target(runtime, &target, &key)?
        && let Some(value) = fields.value.take()
    {
        let conversion = array_length_define_target(
            target,
            name,
            &value,
            ArrayLengthDefinition {
                writable: fields.writable,
                enumerable: fields.enumerable,
                configurable: fields.configurable,
                result,
            },
        );
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

    let definition = property_definition_from_fields(fields, realm, &origin)?;

    if let Some(reference) = target.heap_reference() {
        return begin_internal_define_own_property(
            runtime,
            reference,
            key,
            definition,
            realm,
            return_to,
            origin,
            execution_budget,
            result,
        );
    }

    let outcome = define_own_property(runtime, &target, key, &definition, execution_budget)?;
    match outcome {
        PropertyDefinitionOutcome::Complete => Ok(NativeDispatch::Immediate(match result {
            DefinePropertyResult::Target => target,
            DefinePropertyResult::Boolean => StoredValue::Boolean(true),
        })),
        PropertyDefinitionOutcome::Failed(_) if matches!(result, DefinePropertyResult::Boolean) => {
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
        }
        PropertyDefinitionOutcome::Failed(failure) => Err(NativeFailure::Abrupt(
            property_exception_at(realm, origin, Some(&name), failure)?,
        )),
    }
}

pub(super) fn validate_collected_fields(
    fields: &CollectedFields,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<(), NativeFailure> {
    let has_accessor = fields.get.is_some() || fields.set.is_some();
    let has_data = fields.value.is_some() || fields.writable.is_some();
    if has_accessor && has_data {
        return Err(NativeFailure::Abrupt(descriptor_type_error(
            realm,
            origin,
            "cannot have setter/getter and value or writable",
        )?));
    }
    if has_accessor {
        let _ = accessor_function(fields.get.as_ref(), realm, origin, "getter")?;
        let _ = accessor_function(fields.set.as_ref(), realm, origin, "setter")?;
    }
    Ok(())
}

pub(super) fn property_definition_from_fields(
    fields: CollectedFields,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<PropertyDefinition, NativeFailure> {
    let has_accessor = fields.get.is_some() || fields.set.is_some();
    let has_data = fields.value.is_some() || fields.writable.is_some();
    let definition = if has_accessor {
        let getter = accessor_function(fields.get.as_ref(), realm, origin, "getter")?;
        let setter = accessor_function(fields.set.as_ref(), realm, origin, "setter")?;
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
    Ok(definition
        .with_enumerable(requested_flag(fields.enumerable))
        .with_configurable(requested_flag(fields.configurable)))
}

/// Applies `CompletePropertyDescriptor` to one converted descriptor and
/// materializes the runtime's complete own-property representation.
pub(super) fn complete_own_property_from_fields(
    fields: CollectedFields,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<OwnProperty, NativeFailure> {
    validate_collected_fields(&fields, realm, origin)?;
    let enumerable = fields.enumerable.unwrap_or(false);
    let configurable = fields.configurable.unwrap_or(false);
    if fields.get.is_some() || fields.set.is_some() {
        let getter = accessor_function(fields.get.as_ref(), realm, origin, "getter")?;
        let setter = accessor_function(fields.set.as_ref(), realm, origin, "setter")?;
        return Ok(OwnProperty::Accessor {
            layout: PropertyLayout::accessor(enumerable, configurable),
            getter,
            setter,
        });
    }
    Ok(OwnProperty::Data {
        layout: PropertyLayout::data(fields.writable.unwrap_or(false), enumerable, configurable),
        value: fields.value.unwrap_or(StoredValue::Undefined),
    })
}

/// Starts the specification `ObjectDefineProperties(target, properties)`
/// operation shared by `Object.defineProperties` and `Object.create`.
#[allow(
    clippy::too_many_arguments,
    reason = "the resumable definition collector carries the standard runtime, realm, operands, return target, origin, and execution budget"
)]
pub(super) fn begin_define_properties(
    runtime: &mut Runtime,
    realm: RealmId,
    target: StoredValue,
    properties: StoredValue,
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
    if matches!(properties, StoredValue::Undefined | StoredValue::Null) {
        return Err(NativeFailure::Abrupt(descriptor_type_error(
            realm,
            &origin,
            "cannot convert to object",
        )?));
    }
    let keys = if properties.heap_reference().is_some() {
        Vec::new()
    } else {
        define_properties_keys(runtime, &properties, execution_budget)?
    };
    let mut definitions = Vec::new();
    definitions
        .try_reserve_exact(keys.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: keys.len(),
        })?;
    let mut state = DefinePropertiesContinuation {
        target,
        properties,
        keys,
        next_key: 0,
        pending_key: None,
        reader: None,
        definitions,
        next_definition: 0,
        realm,
        origin,
        stage: DefinePropertiesStage::NextKey,
    };
    let Some(properties) = state.properties.heap_reference() else {
        return advance_define_properties(runtime, state, None, return_to, execution_budget);
    };
    state.stage = DefinePropertiesStage::AwaitKeys;
    let dispatch = begin_internal_own_keys(
        runtime,
        properties,
        realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_define_properties_after(runtime, dispatch, state, return_to, execution_budget)
}

fn continue_define_properties_after(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: DefinePropertiesContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_define_properties(runtime, state, Some(value), return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::DefineProperties(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::DefineProperties(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "defineProperties internal method produced a structured result",
        }
        .into()),
    }
}

/// Resumes descriptor collection or the later ordered definition phase.
#[allow(
    clippy::too_many_lines,
    reason = "one explicit loop keeps the five specification phases and every suspension transition auditable in order"
)]
pub(super) fn advance_define_properties(
    runtime: &mut Runtime,
    mut state: DefinePropertiesContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            DefinePropertiesStage::AwaitKeys => {
                state.keys = generated_key_list(
                    runtime,
                    take_define_properties_completion(&mut completion, "ownKeys")?,
                )?;
                state.stage = DefinePropertiesStage::NextKey;
            }
            DefinePropertiesStage::AwaitOwnDescriptor => {
                let descriptor =
                    take_define_properties_completion(&mut completion, "own property descriptor")?;
                let enumerable = internal_complete_own_property(runtime, &descriptor)?
                    .is_some_and(|property| property.layout().is_enumerable());
                if !enumerable {
                    state.pending_key = None;
                    state.stage = DefinePropertiesStage::NextKey;
                    continue;
                }
                let key = state
                    .pending_key
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "defineProperties own descriptor lost its key",
                    })?
                    .clone();
                let properties =
                    state
                        .properties
                        .heap_reference()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "defineProperties object source disappeared",
                        })?;
                state.stage = DefinePropertiesStage::AwaitDescriptorValue;
                let dispatch = begin_internal_get(
                    runtime,
                    properties,
                    state.properties.duplicate(),
                    key,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                return continue_define_properties_after(
                    runtime,
                    dispatch,
                    state,
                    return_to,
                    execution_budget,
                );
            }
            DefinePropertiesStage::AwaitDescriptorValue => {
                let descriptor =
                    take_define_properties_completion(&mut completion, "descriptor value")?;
                state.reader = Some(begin_descriptor_read(
                    descriptor,
                    state.realm,
                    &state.origin,
                )?);
                state.stage = DefinePropertiesStage::ReadDescriptor;
            }
            DefinePropertiesStage::ReadDescriptor => {
                let outcome = advance_descriptor_read(
                    runtime,
                    state.reader.as_mut().ok_or(EngineFault::RuntimeInvariant {
                        message: "defineProperties lost its descriptor reader",
                    })?,
                    completion.take(),
                    state.realm,
                    &state.origin,
                    return_to,
                    execution_budget,
                )?;
                match outcome {
                    DescriptorReadOutcome::Complete(fields) => {
                        validate_collected_fields(&fields, state.realm, &state.origin)?;
                        let key = state.pending_key.take().ok_or(
                            EngineFault::RuntimeInvariant {
                                message: "defineProperties converted a descriptor without a key",
                            },
                        )?;
                        state
                            .definitions
                            .push(Some(CollectedDefinition { key, fields }));
                        state.reader = None;
                        state.stage = DefinePropertiesStage::NextKey;
                    }
                    DescriptorReadOutcome::Nested(dispatch) => {
                        return continue_descriptor_nested(
                            *dispatch,
                            NativeContinuation::DefineProperties(Box::new(state)),
                        );
                    }
                }
            }
            DefinePropertiesStage::AwaitDefinition => {
                let _ = take_define_properties_completion(&mut completion, "definition")?;
                state.stage = DefinePropertiesStage::Apply;
            }
            DefinePropertiesStage::NextKey => {
                let Some(key) = state.keys.get(state.next_key).cloned() else {
                    state.properties = StoredValue::Undefined;
                    state.keys.clear();
                    state.stage = DefinePropertiesStage::Apply;
                    continue;
                };
                state.next_key = state.next_key.saturating_add(1);
                execution_budget.charge_instructions(1)?;
                if let Some(properties) = state.properties.heap_reference() {
                    state.pending_key = Some(key.clone());
                    state.stage = DefinePropertiesStage::AwaitOwnDescriptor;
                    let dispatch = begin_internal_get_own_property(
                        runtime,
                        properties,
                        key,
                        state.realm,
                        return_to,
                        state.origin.clone(),
                        execution_budget,
                    )?;
                    return continue_define_properties_after(
                        runtime,
                        dispatch,
                        state,
                        return_to,
                        execution_budget,
                    );
                }
                charge_define_properties_lookup(runtime, &state.properties, execution_budget)?;
                let Some(own) = resolve_own_property(
                    runtime,
                    state.realm,
                    &state.properties,
                    &key,
                    &state.origin,
                )?
                else {
                    continue;
                };
                if !own.layout().is_enumerable() {
                    continue;
                }
                charge_define_properties_lookup(runtime, &state.properties, execution_budget)?;
                match read_static_property(runtime, state.realm, &state.properties, &key)? {
                    PropertyReadOutcome::Value(descriptor) => {
                        state.pending_key = Some(key);
                        state.reader = Some(begin_descriptor_read(
                            descriptor,
                            state.realm,
                            &state.origin,
                        )?);
                        state.stage = DefinePropertiesStage::ReadDescriptor;
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.pending_key = Some(key);
                        state.stage = DefinePropertiesStage::AwaitDescriptorValue;
                        return define_properties_call(function, receiver, state, return_to);
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
            DefinePropertiesStage::Apply => {
                let Some(slot) = state.definitions.get_mut(state.next_definition) else {
                    return Ok(NativeDispatch::Immediate(state.target));
                };
                let definition = slot.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "defineProperties revisited an applied descriptor",
                })?;
                state.next_definition = state.next_definition.saturating_add(1);

                let property =
                    property_definition_from_fields(definition.fields, state.realm, &state.origin)?;
                let target =
                    state
                        .target
                        .heap_reference()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "defineProperties target ceased to be an object",
                        })?;
                state.stage = DefinePropertiesStage::AwaitDefinition;
                let dispatch = begin_internal_define_own_property(
                    runtime,
                    target,
                    definition.key,
                    property,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                    DefinePropertyResult::Target,
                )?;
                return continue_define_properties_after(
                    runtime,
                    dispatch,
                    state,
                    return_to,
                    execution_budget,
                );
            }
        }
    }
}

fn define_properties_keys(
    runtime: &Runtime,
    properties: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<Vec<PropertyKey>, NativeFailure> {
    if let StoredValue::String(value) = properties {
        let keys = primitive_string_own_keys(runtime, value)?;
        execution_budget.charge_instructions(usize_to_u64(keys.len()).saturating_add(1))?;
        return Ok(keys);
    }
    execution_budget.charge_instructions(1)?;
    Ok(Vec::new())
}

fn charge_define_properties_lookup(
    runtime: &Runtime,
    properties: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    if properties.heap_reference().is_some() {
        charge_heap_property_lookup(runtime, properties, execution_budget)
    } else {
        execution_budget.charge_instructions(1).map_err(Into::into)
    }
}

fn define_properties_call(
    function: FunctionId,
    receiver: StoredValue,
    state: DefinePropertiesContinuation,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::empty(),
        return_to,
        origin,
        continuations: define_properties_continuation(state)?,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

fn define_properties_continuation(
    state: DefinePropertiesContinuation,
) -> Result<Vec<NativeContinuation>, NativeFailure> {
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::DefineProperties(Box::new(state)));
    Ok(continuations)
}

fn take_define_properties_completion(
    completion: &mut Option<StoredValue>,
    operation: &'static str,
) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        let message = match operation {
            "ownKeys" => "defineProperties resumed without an own-key completion",
            "own property descriptor" => {
                "defineProperties resumed without an own-descriptor completion"
            }
            "descriptor value" => "defineProperties resumed without a descriptor-value completion",
            "definition" => "defineProperties resumed without a definition completion",
            _ => "defineProperties resumed without an expected completion",
        };
        EngineFault::RuntimeInvariant { message }.into()
    })
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

/// Resolves one own property for the `Object.prototype` reflection methods.
///
/// This shares `own_property_descriptor`'s resolution so `hasOwnProperty` and
/// `propertyIsEnumerable` agree with it on every exotic case, including a
/// primitive String's index and `length` properties.
pub(super) fn resolve_own_property(
    runtime: &Runtime,
    realm: RealmId,
    target: &StoredValue,
    key: &PropertyKey,
    origin: &JsStackFrame,
) -> Result<Option<OwnProperty>, NativeFailure> {
    match target {
        StoredValue::Function(function) => {
            own_property_of(runtime, HeapReference::Function(*function), key)
        }
        StoredValue::Object(object) => {
            own_property_of(runtime, HeapReference::Object(*object), key)
        }
        StoredValue::String(value) => string_own_property(value, key),
        StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::Symbol(_) => Ok(None),
        // `ToObject` runs first, so a nullish receiver throws.
        StoredValue::Undefined | StoredValue::Null => {
            Err(NativeFailure::Abrupt(PendingException {
                realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::TypeError,
                    message: JsString::from_utf8("cannot convert to object")?,
                },
                origin: origin.clone(),
            }))
        }
    }
}

/// Reads one own property, consulting the `String` wrapper exotic first.
pub(super) fn own_property_of(
    runtime: &Runtime,
    reference: HeapReference,
    key: &PropertyKey,
) -> Result<Option<OwnProperty>, NativeFailure> {
    Ok(heap_own_property(runtime, reference, key)?)
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
pub(super) fn build_descriptor_object(
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

/// Materializes a partial property descriptor for Proxy
/// `[[DefineOwnProperty]]`'s trap argument. Only present fields are created.
pub(super) fn build_definition_object(
    runtime: &mut Runtime,
    realm: RealmId,
    definition: &PropertyDefinition,
) -> Result<ObjectId, NativeFailure> {
    let prototype = runtime.realm_object_prototype(realm)?;
    let object = runtime.allocate_ordinary_object(prototype)?;
    let reference = HeapReference::Object(object);
    let field = PropertyLayout::data(true, true, true);
    if let Some(enumerable) = definition.requested_enumerable() {
        append_descriptor_field(
            runtime,
            reference,
            PredefinedAtom::Enumerable,
            field,
            StoredValue::Boolean(enumerable),
        )?;
    }
    if let Some(configurable) = definition.requested_configurable() {
        append_descriptor_field(
            runtime,
            reference,
            PredefinedAtom::Configurable,
            field,
            StoredValue::Boolean(configurable),
        )?;
    }
    if definition.is_accessor_descriptor() {
        if let Some(getter) = definition.requested_getter() {
            append_descriptor_field(
                runtime,
                reference,
                PredefinedAtom::Get,
                field,
                accessor_slot_value(getter),
            )?;
        }
        if let Some(setter) = definition.requested_setter() {
            append_descriptor_field(
                runtime,
                reference,
                PredefinedAtom::SetProperty,
                field,
                accessor_slot_value(setter),
            )?;
        }
    } else {
        if let Some(value) = definition.requested_value() {
            append_descriptor_field(
                runtime,
                reference,
                PredefinedAtom::Value,
                field,
                value.duplicate(),
            )?;
        }
        if let Some(writable) = definition.requested_writable() {
            append_descriptor_field(
                runtime,
                reference,
                PredefinedAtom::Writable,
                field,
                StoredValue::Boolean(writable),
            )?;
        }
    }
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
