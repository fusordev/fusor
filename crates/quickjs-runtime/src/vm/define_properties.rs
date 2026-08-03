/*
 * JavaScript Object.defineProperties semantics derived from QuickJS.
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

//! `Object.defineProperties` and `Object.create`'s descriptors argument.
//!
//! Both run `ObjectDefineProperties`, which is a *two-phase* operation and the
//! one place the specification and the pinned oracle disagree here. ECMAScript
//! reads and validates every descriptor first, then applies them all
//! (`ObjectDefineProperties` steps 3-5), so a descriptor that throws while
//! being read leaves the target completely untouched — including for keys the
//! walk already passed. Upstream's `JS_ObjectDefineProperties` interleaves the
//! phases, so an earlier definition survives a later read that throws:
//!
//! ```text
//! const target = {};
//! Object.defineProperties(target, {a: {value: 1}, get b() { throw new Error() }});
//! target.a   // oracle 1; specification and V8 undefined
//! ```
//!
//! The specification's order is implemented, because this port preserves
//! observable ECMAScript behavior rather than upstream's private sequencing.
//!
//! Everything else follows upstream. The descriptors object's own *enumerable*
//! keys are visited, string and symbol alike; each key's value is read first,
//! which can enter an accessor; and each descriptor is then read field by field
//! with the same order and the same resumable reads `Object.defineProperty`
//! uses, so the walk has two nested suspension points per key.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// Which read a walk is awaiting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DefinePropertiesStage {
    /// The descriptors object's own property, whose value is the descriptor.
    AwaitDescriptor,
    /// One field of the descriptor named by `field`.
    AwaitField,
}

/// One in-progress `ObjectDefineProperties`.
pub(super) struct DefinePropertiesContinuation {
    /// The object receiving every definition, once all are validated.
    target: StoredValue,
    /// The descriptors object being walked.
    descriptors: StoredValue,
    /// The descriptors object's own keys, captured before the first read.
    keys: ForInSnapshot,
    /// The index into `keys` of the key being read.
    next: usize,
    /// The descriptor object for the key being read.
    descriptor: Option<StoredValue>,
    /// The fields read from that descriptor so far.
    fields: CollectedFields,
    /// The index into [`DescriptorField::ORDER`] of the next field to read.
    field: usize,
    /// Every validated definition, paired with its key, awaiting the apply
    /// phase.
    validated: Vec<(PropertyKey, PropertyDefinition)>,
    stage: DefinePropertiesStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl DefinePropertiesContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        // The target, the descriptors object, the descriptor in flight, and one
        // slot per validated definition.
        2_u64
            .saturating_add(u64::from(self.descriptor.is_some()))
            .saturating_add(usize_to_u64(self.validated.len()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        trace_stored_value_root(&self.descriptors, mark);
        if let Some(descriptor) = &self.descriptor {
            trace_stored_value_root(descriptor, mark);
        }
        self.fields.trace_roots(mark);
        for (_, definition) in &self.validated {
            if let Some(value) = definition.requested_value() {
                trace_stored_value_root(value, mark);
            }
            let (getter, setter) = definition.requested_accessors();
            for function in [getter, setter].into_iter().flatten() {
                mark(CollectionRoot::Heap(HeapReference::Function(function)));
            }
        }
    }
}

/// Starts `ObjectDefineProperties(target, descriptors)`.
///
/// The target must already be an object; the descriptors argument converts with
/// `ToObject`, so a primitive is boxed and a nullish one throws.
#[allow(
    clippy::too_many_arguments,
    reason = "define-properties carries the same runtime, realm, operand, resume, origin, and budget authority as every other resumable native operation"
)]
pub(super) fn begin_define_properties(
    runtime: &mut Runtime,
    realm: RealmId,
    target: StoredValue,
    descriptors: StoredValue,
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
    let descriptors = to_object(runtime, realm, descriptors, &origin)?;
    let reference = heap_reference_of_object(&descriptors)?;
    let (keys, work) = runtime.try_own_key_snapshot(reference, 0, KeyPhases::ALL)?;
    execution_budget.charge_instructions(work)?;
    let state = DefinePropertiesContinuation {
        target,
        descriptors,
        keys,
        next: 0,
        descriptor: None,
        fields: CollectedFields::default(),
        field: 0,
        validated: Vec::new(),
        stage: DefinePropertiesStage::AwaitDescriptor,
        realm,
        origin,
    };
    advance_define_properties(runtime, state, None, return_to, execution_budget)
}

/// Resumes a walk after a read returned, then continues it.
pub(super) fn advance_define_properties(
    runtime: &mut Runtime,
    mut state: DefinePropertiesContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        match state.stage {
            DefinePropertiesStage::AwaitDescriptor => {
                state.descriptor = Some(value);
                state.fields = CollectedFields::default();
                state.field = 0;
                state.stage = DefinePropertiesStage::AwaitField;
            }
            DefinePropertiesStage::AwaitField => {
                let index = state
                    .field
                    .checked_sub(1)
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "descriptor field resumed without a field in flight",
                    })?;
                let field =
                    *DescriptorField::ORDER
                        .get(index)
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "descriptor field resumption is out of range",
                        })?;
                record_field(&mut state.fields, field, value);
            }
        }
    }

    // Phase one: read and validate every descriptor, applying none.
    loop {
        if state.descriptor.is_none() {
            let Some(candidate) = state.keys.get(state.next).cloned() else {
                break;
            };
            state.next = state.next.saturating_add(1);
            execution_budget.charge_instructions(1)?;
            // Only enumerable keys are visited, re-tested against the live
            // object so an accessor's own mutations are observable.
            if !own_key_is_enumerable_on(runtime, &state.descriptors, candidate.key())? {
                continue;
            }
            charge_heap_property_lookup(runtime, &state.descriptors, execution_budget)?;
            match read_static_property(runtime, state.realm, &state.descriptors, candidate.key())? {
                PropertyReadOutcome::Value(descriptor) => {
                    state.descriptor = Some(descriptor);
                    state.fields = CollectedFields::default();
                    state.field = 0;
                    state.stage = DefinePropertiesStage::AwaitField;
                }
                PropertyReadOutcome::Getter { function, receiver } => {
                    state.stage = DefinePropertiesStage::AwaitDescriptor;
                    return define_properties_suspend(state, function, receiver, return_to);
                }
                PropertyReadOutcome::Failed(_) => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "descriptors object failed an own-property read",
                    }
                    .into());
                }
            }
            continue;
        }

        if let Some((function, receiver)) =
            read_descriptor_fields(runtime, &mut state, execution_budget)?
        {
            return define_properties_suspend(state, function, receiver, return_to);
        }
        queue_validated_definition(&mut state)?;
    }

    apply_validated_definitions(runtime, state, execution_budget)
}

/// Reads the current descriptor's remaining fields.
///
/// Returns the accessor to call when a field read suspends, and `None` when the
/// descriptor is complete.
fn read_descriptor_fields(
    runtime: &mut Runtime,
    state: &mut DefinePropertiesContinuation,
    execution_budget: &mut ExecutionBudget,
) -> Result<Option<(FunctionId, StoredValue)>, NativeFailure> {
    let descriptor = state
        .descriptor
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "descriptor walk lost its descriptor",
        })?
        .duplicate();
    // A non-object descriptor is rejected before any field is read.
    if !matches!(
        descriptor,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        return Err(NativeFailure::Abrupt(descriptor_type_error(
            state.realm,
            &state.origin,
            "not an object",
        )?));
    }
    while state.field < DescriptorField::ORDER.len() {
        let field = DescriptorField::ORDER[state.field];
        state.field = state.field.saturating_add(1);
        let key = runtime.predefined_property_key(field.predefined_atom());
        // An absent field must stay absent rather than becoming a present
        // `undefined`, so presence is tested before the value is read.
        if !has_descriptor_field(runtime, &descriptor, &key)? {
            continue;
        }
        charge_heap_property_lookup(runtime, &descriptor, execution_budget)?;
        match read_static_property(runtime, state.realm, &descriptor, &key)? {
            PropertyReadOutcome::Value(value) => record_field(&mut state.fields, field, value),
            PropertyReadOutcome::Getter { function, receiver } => {
                state.stage = DefinePropertiesStage::AwaitField;
                return Ok(Some((function, receiver)));
            }
            PropertyReadOutcome::Failed(_) => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "descriptor object failed a field read",
                }
                .into());
            }
        }
    }
    Ok(None)
}

/// Validates the completed descriptor and queues it for the apply phase.
fn queue_validated_definition(
    state: &mut DefinePropertiesContinuation,
) -> Result<(), NativeFailure> {
    let index = state
        .next
        .checked_sub(1)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "validated descriptor has no key",
        })?;
    let key = state
        .keys
        .get(index)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "descriptors key snapshot shrank during validation",
        })?
        .key()
        .clone();
    let fields = std::mem::take(&mut state.fields);
    let definition = validate_collected_fields(fields, state.realm, &state.origin)?;
    state
        .validated
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 1,
        })?;
    state.validated.push((key, definition));
    state.descriptor = None;
    state.field = 0;
    state.stage = DefinePropertiesStage::AwaitDescriptor;
    Ok(())
}

/// Applies every validated definition, in the order their keys were visited.
///
/// This is phase two, so no user code can run: every descriptor has already been
/// read and validated, and a refused definition is the only failure left.
fn apply_validated_definitions(
    runtime: &mut Runtime,
    state: DefinePropertiesContinuation,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let DefinePropertiesContinuation {
        target,
        validated,
        realm,
        origin,
        ..
    } = state;
    for (key, definition) in validated {
        let name = own_key_name(&key)?;
        match define_own_property(runtime, &target, key, &definition, execution_budget)? {
            PropertyDefinitionOutcome::Complete => {}
            PropertyDefinitionOutcome::Failed(failure) => {
                return Err(NativeFailure::Abrupt(property_exception_at(
                    realm,
                    origin,
                    Some(&name),
                    failure,
                )?));
            }
        }
    }
    Ok(NativeDispatch::Immediate(target))
}

/// Suspends a walk on one read.
fn define_properties_suspend(
    state: DefinePropertiesContinuation,
    function: FunctionId,
    receiver: StoredValue,
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
    continuations.push(NativeContinuation::DefineProperties(Box::new(state)));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::empty(),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}
