/*
 * JavaScript Object listing semantics derived from QuickJS.
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

//! `Object.values`, `Object.entries`, and `Object.getOwnPropertyDescriptors`.
//!
//! All three walk the target's own keys and build a fresh result, and all three
//! share `Object.keys`' key snapshot so no two listings can disagree about which
//! keys exist. They differ in what each key contributes and in whether the walk
//! is resumable:
//!
//! * `values` and `entries` *read* each key, so every step can enter a getter
//!   and the walk suspends (`JS_GetOwnPropertyNamesInternal` with
//!   `JS_ITERATOR_KIND_VALUE` / `KIND_KEY_AND_VALUE`, `quickjs.c:40206-40260`).
//!   The enumerable attribute is re-tested against the *live* object at each
//!   step rather than trusted from the snapshot, so a getter that hides a later
//!   key removes it from the result.
//! * `getOwnPropertyDescriptors` never reads a value: an accessor contributes
//!   its getter and setter functions rather than the result of calling them, so
//!   the whole operation completes without suspending
//!   (`quickjs.c:40206-40245`). It also reports non-enumerable and symbol keys,
//!   which `values` and `entries` both skip.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// One in-progress `Object.values` or `Object.entries`.
pub(super) struct ObjectListingContinuation {
    /// The object being walked, which the reads target.
    target: StoredValue,
    /// Which listing is being built.
    listing: ObjectListing,
    /// The own keys captured before the first read.
    snapshot: ForInSnapshot,
    /// The index into the snapshot of the next key to consider.
    next: usize,
    /// The key whose read is suspended, when one is.
    pending: Option<PropertyKey>,
    /// The elements collected so far.
    elements: Vec<StoredValue>,
    realm: RealmId,
    origin: JsStackFrame,
}

impl ObjectListingContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        // The target plus every collected element.
        1_u64.saturating_add(usize_to_u64(self.elements.len()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        for element in &self.elements {
            trace_stored_value_root(element, mark);
        }
    }
}

/// Starts `Object.values(target)` or `Object.entries(target)`.
///
/// A primitive other than `null` and `undefined` answers through the properties
/// its wrapper would expose, so a `String` contributes its characters while a
/// Number contributes nothing; a nullish target reports the `ToObject` failure.
pub(super) fn begin_object_listing(
    runtime: &mut Runtime,
    realm: RealmId,
    listing: ObjectListing,
    argument: Option<StoredValue>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    let Some(reference) = listing_target(runtime, realm, &target, &origin, listing.name())? else {
        // A primitive `String`'s characters are own enumerable properties, so
        // they are listed the way a boxed wrapper's would be; every other
        // primitive has no own enumerable key. Neither can enter a getter, so
        // the elements are built directly.
        let elements = match &target {
            StoredValue::String(value) => {
                let value = value.clone();
                primitive_string_elements(runtime, realm, &value, listing)?
            }
            StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::Symbol(_) => Vec::new(),
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Function(_)
            | StoredValue::Object(_) => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "primitive Object listing received a non-primitive",
                }
                .into());
            }
        };
        return finish_object_listing(runtime, realm, elements);
    };
    let (snapshot, work) = runtime.try_for_in_snapshot(reference, 0)?;
    execution_budget.charge_instructions(work)?;
    let state = ObjectListingContinuation {
        target,
        listing,
        snapshot,
        next: 0,
        pending: None,
        elements: Vec::new(),
        realm,
        origin,
    };
    advance_object_listing(runtime, state, None, return_to, execution_budget)
}

/// Resumes a listing after a getter returned, then continues the walk.
pub(super) fn advance_object_listing(
    runtime: &mut Runtime,
    mut state: ObjectListingContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        let Some(key) = state.pending.take() else {
            return Err(EngineFault::RuntimeInvariant {
                message: "Object listing resumed without a pending key",
            }
            .into());
        };
        push_listing_element(runtime, &mut state, &key, value)?;
    } else if state.pending.is_some() {
        return Err(EngineFault::RuntimeInvariant {
            message: "Object listing started with a pending key",
        }
        .into());
    }

    while let Some(candidate) = state.snapshot.get(state.next).cloned() {
        state.next = state.next.saturating_add(1);
        execution_budget.charge_instructions(1)?;
        // The attribute is re-tested against the live object, so a getter that
        // deleted or hid a later key removes it from the result.
        if !own_key_is_enumerable_on(runtime, &state.target, candidate.key())? {
            continue;
        }
        charge_heap_property_lookup(runtime, &state.target, execution_budget)?;
        match read_static_property(runtime, state.realm, &state.target, candidate.key())? {
            PropertyReadOutcome::Value(value) => {
                let key = candidate.key().clone();
                push_listing_element(runtime, &mut state, &key, value)?;
            }
            PropertyReadOutcome::Getter { function, receiver } => {
                state.pending = Some(candidate.key().clone());
                return listing_getter_call(state, function, receiver, return_to);
            }
            PropertyReadOutcome::Failed(_) => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Object listing target failed an own-property read",
                }
                .into());
            }
        }
    }

    let ObjectListingContinuation {
        elements, realm, ..
    } = state;
    finish_object_listing(runtime, realm, elements)
}

/// Appends one key's contribution to the result under construction.
fn push_listing_element(
    runtime: &mut Runtime,
    state: &mut ObjectListingContinuation,
    key: &PropertyKey,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    let element = if state.listing.is_paired() {
        let name = StoredValue::String(own_key_name(key)?);
        StoredValue::Object(build_entry_pair(runtime, state.realm, name, value)?)
    } else {
        value
    };
    state
        .elements
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 1,
        })?;
    state.elements.push(element);
    Ok(())
}

/// Suspends a listing on one getter call.
fn listing_getter_call(
    state: ObjectListingContinuation,
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
    continuations.push(NativeContinuation::ObjectListing(Box::new(state)));
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

/// Materializes the collected elements as a fresh base Array.
fn finish_object_listing(
    runtime: &mut Runtime,
    realm: RealmId,
    elements: Vec<StoredValue>,
) -> Result<NativeDispatch, NativeFailure> {
    let array = runtime.allocate_array(realm, elements)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(array)))
}

/// `Object.getOwnPropertyDescriptors(target)`.
///
/// Every own key — enumerable or not, string or symbol — contributes a
/// materialized descriptor object under the same key, which makes the result a
/// valid `Object.defineProperties` argument. No value is read, so an accessor
/// contributes its functions rather than the result of calling them and the
/// operation never suspends.
pub(super) fn own_property_descriptors(
    runtime: &mut Runtime,
    realm: RealmId,
    argument: Option<StoredValue>,
    origin: Option<&JsStackFrame>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = argument.unwrap_or(StoredValue::Undefined);
    let host_origin = match origin {
        Some(origin) => origin.clone(),
        None => {
            return Err(EngineFault::RuntimeInvariant {
                message: "host Object descriptor listing has no source origin",
            }
            .into());
        }
    };
    let prototype = runtime.realm_object_prototype(realm)?;
    let result = runtime.allocate_ordinary_object(prototype)?;
    let Some(reference) = listing_target(
        runtime,
        realm,
        &target,
        &host_origin,
        "getOwnPropertyDescriptors",
    )?
    else {
        // A primitive `String`'s indices and `length` are exotic own
        // properties, so they are described the way a boxed wrapper's are.
        if let StoredValue::String(value) = &target {
            let value = value.clone();
            append_primitive_string_descriptors(runtime, realm, result, &value, &host_origin)?;
        }
        return Ok(NativeDispatch::Immediate(StoredValue::Object(result)));
    };
    let (snapshot, work) = runtime.try_own_key_snapshot(reference, 0, KeyPhases::ALL)?;
    execution_budget.charge_instructions(work)?;
    for index in 0..snapshot.len() {
        let key = snapshot
            .get(index)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "own-key snapshot shrank during a descriptor listing",
            })?
            .key()
            .clone();
        let Some(own) = resolve_own_property(runtime, realm, &target, &key, &host_origin)? else {
            // A key the snapshot listed but the object no longer has is simply
            // absent from the result, which is what `[[GetOwnProperty]]`
            // returning `undefined` produces.
            continue;
        };
        let descriptor = build_descriptor_object(runtime, realm, own)?;
        append_descriptor_entry(runtime, result, key, descriptor)?;
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
}

/// Describes a primitive `String`'s exotic own properties.
fn append_primitive_string_descriptors(
    runtime: &mut Runtime,
    realm: RealmId,
    result: ObjectId,
    value: &JsString,
    origin: &JsStackFrame,
) -> Result<(), NativeFailure> {
    let target = StoredValue::String(value.clone());
    for index in 0..value.len() {
        let key = PropertyKey::from_index(ArrayIndex::new(index).ok_or(
            EngineFault::RuntimeInvariant {
                message: "String length cannot contain the non-index u32 maximum",
            },
        )?);
        let Some(own) = resolve_own_property(runtime, realm, &target, &key, origin)? else {
            continue;
        };
        let descriptor = build_descriptor_object(runtime, realm, own)?;
        append_descriptor_entry(runtime, result, key, descriptor)?;
    }
    let length_key = runtime.predefined_property_key(PredefinedAtom::Length);
    if let Some(own) = resolve_own_property(runtime, realm, &target, &length_key, origin)? {
        let descriptor = build_descriptor_object(runtime, realm, own)?;
        append_descriptor_entry(runtime, result, length_key, descriptor)?;
    }
    Ok(())
}

/// Appends one fully mutable descriptor entry to the result object.
fn append_descriptor_entry(
    runtime: &mut Runtime,
    result: ObjectId,
    key: PropertyKey,
    descriptor: ObjectId,
) -> Result<(), NativeFailure> {
    runtime.append_data_property(
        HeapReference::Object(result),
        key,
        PropertyLayout::data(true, true, true),
        StoredValue::Object(descriptor),
    )?;
    Ok(())
}

/// Returns whether one own key is currently enumerable on the live object.
///
/// Every walk that copies or lists own properties re-tests this rather than
/// trusting its key snapshot, so a getter's own mutations are observable.
pub(super) fn own_key_is_enumerable_on(
    runtime: &Runtime,
    target: &StoredValue,
    key: &PropertyKey,
) -> Result<bool, NativeFailure> {
    let reference = match target {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "own-key enumerability test received a non-object",
            }
            .into());
        }
    };
    if let Some(property) = string_exotic_index_property(runtime, reference, key)? {
        return Ok(property.layout().is_enumerable());
    }
    Ok(runtime
        .object_record(reference)?
        .own_property(key)
        .is_some_and(|property| property.layout().is_enumerable()))
}

/// Resolves a listing's argument, which accepts any non-nullish value.
fn listing_target(
    runtime: &Runtime,
    realm: RealmId,
    value: &StoredValue,
    origin: &JsStackFrame,
    method: &str,
) -> Result<Option<HeapReference>, NativeFailure> {
    let _ = (runtime, method);
    match value {
        StoredValue::Function(function) => Ok(Some(HeapReference::Function(*function))),
        StoredValue::Object(object) => Ok(Some(HeapReference::Object(*object))),
        // `ToObject` fails for a nullish argument, which is what every listing
        // reports before touching a key.
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
        StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => Ok(None),
    }
}

/// Builds a primitive `String`'s listing elements.
///
/// The characters are the only own enumerable properties; `length` is not
/// enumerable, so it never appears. A paired element needs a realm-owned Array,
/// which is why the runtime is threaded through.
fn primitive_string_elements(
    runtime: &mut Runtime,
    realm: RealmId,
    value: &JsString,
    listing: ObjectListing,
) -> Result<Vec<StoredValue>, NativeFailure> {
    let length = value.len();
    let reserved = usize::try_from(length).unwrap_or(usize::MAX);
    let mut elements = Vec::new();
    elements
        .try_reserve_exact(reserved)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: reserved,
        })?;
    for index in 0..length {
        let character = StoredValue::String(value.slice(index..index.saturating_add(1))?);
        let element = if listing.is_paired() {
            let name = StoredValue::String(JsNumber::from_u32(index).to_radix_string(10)?);
            StoredValue::Object(build_entry_pair(runtime, realm, name, character)?)
        } else {
            character
        };
        elements.push(element);
    }
    Ok(elements)
}

/// Builds one `[key, value]` pair as a fresh base Array.
fn build_entry_pair(
    runtime: &mut Runtime,
    realm: RealmId,
    key: StoredValue,
    value: StoredValue,
) -> Result<ObjectId, NativeFailure> {
    let mut pair = Vec::new();
    pair.try_reserve_exact(2)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 2,
        })?;
    pair.push(key);
    pair.push(value);
    Ok(runtime.allocate_array(realm, pair)?)
}

/// Renders one own key as the string a listing or a failure message reports.
pub(super) fn own_key_name(key: &PropertyKey) -> Result<JsString, NativeFailure> {
    if let Some(index) = key.as_index() {
        return Ok(JsNumber::from_u32(index.get()).to_radix_string(10)?);
    }
    let atom = key.as_atom().ok_or(EngineFault::RuntimeInvariant {
        message: "listing own key is neither an index nor an atom",
    })?;
    // A symbol without a description renders as the empty string, which only a
    // failure message can observe: no listing reports a symbol key.
    Ok(atom.description().cloned().unwrap_or_else(JsString::empty))
}

/// Applies ECMAScript `ToObject`.
///
/// A primitive is boxed into its wrapper and a nullish value reports
/// `cannot convert to object`, which is the failure every `Object` static that
/// converts its argument shares.
pub(super) fn to_object(
    runtime: &mut Runtime,
    realm: RealmId,
    value: StoredValue,
    origin: &JsStackFrame,
) -> Result<StoredValue, NativeFailure> {
    match value {
        value @ (StoredValue::Function(_) | StoredValue::Object(_)) => Ok(value),
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
        StoredValue::Boolean(value) => Ok(StoredValue::Object(
            runtime.allocate_boxed_boolean(realm, value)?,
        )),
        StoredValue::BigInt(value) => Ok(StoredValue::Object(
            runtime.allocate_boxed_bigint(realm, value)?,
        )),
        StoredValue::Number(value) => Ok(StoredValue::Object(
            runtime.allocate_boxed_number(realm, value)?,
        )),
        StoredValue::String(value) => Ok(StoredValue::Object(
            runtime.allocate_boxed_string(realm, value)?,
        )),
        StoredValue::Symbol(value) => Ok(StoredValue::Object(
            runtime.allocate_boxed_symbol(realm, value)?,
        )),
    }
}

/// Resolves a heap reference from a value already known to be an object.
pub(super) fn heap_reference_of_object(
    value: &StoredValue,
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
        | StoredValue::Symbol(_) => Err(EngineFault::RuntimeInvariant {
            message: "converted value is not an object",
        }
        .into()),
    }
}
