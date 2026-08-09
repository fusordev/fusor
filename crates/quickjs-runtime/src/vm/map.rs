/*
 * JavaScript Map semantics derived from QuickJS.
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

//! Typed, resumable Map construction and callback semantics.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use crate::object::{MapIteratorKind, MapState};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MapCollectionKind {
    Map,
    WeakMap,
}

impl MapCollectionKind {
    const fn constructor_error(self) -> &'static str {
        match self {
            Self::Map => "Map constructor requires 'new'",
            Self::WeakMap => "WeakMap constructor requires 'new'",
        }
    }
}

pub(super) struct MapConstructorRequest {
    kind: MapCollectionKind,
    realm: RealmId,
    new_target: Option<FunctionId>,
    iterable: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
}

impl MapConstructorRequest {
    pub(super) const fn new(
        kind: MapCollectionKind,
        realm: RealmId,
        new_target: Option<FunctionId>,
        iterable: StoredValue,
        return_to: Option<CallReturn>,
        origin: JsStackFrame,
    ) -> Self {
        Self {
            kind,
            realm,
            new_target,
            iterable,
            return_to,
            origin,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MapConstructorStage {
    Adder,
    IteratorMethod,
    Iterator,
    NextMethod,
    NextResult,
    Done,
    IteratorValue,
    EntryKey,
    EntryValue,
    AdderCall,
}

pub(super) struct MapConstructorContinuation {
    target: ObjectId,
    iterable: StoredValue,
    adder: Option<FunctionId>,
    iterator: Option<StoredValue>,
    next: Option<FunctionId>,
    result: Option<StoredValue>,
    entry: Option<StoredValue>,
    key: Option<StoredValue>,
    value: Option<StoredValue>,
    realm: RealmId,
    stage: MapConstructorStage,
    origin: JsStackFrame,
}

impl MapConstructorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.adder.is_some()))
            .saturating_add(u64::from(self.iterator.is_some()))
            .saturating_add(u64::from(self.next.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
            .saturating_add(u64::from(self.entry.is_some()))
            .saturating_add(u64::from(self.key.is_some()))
            .saturating_add(u64::from(self.value.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.target)));
        trace_stored_value_root(&self.iterable, mark);
        if let Some(adder) = self.adder {
            mark(CollectionRoot::Heap(HeapReference::Function(adder)));
        }
        if let Some(next) = self.next {
            mark(CollectionRoot::Heap(HeapReference::Function(next)));
        }
        for value in [
            self.iterator.as_ref(),
            self.result.as_ref(),
            self.entry.as_ref(),
            self.key.as_ref(),
            self.value.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            trace_stored_value_root(value, mark);
        }
    }

    const fn closes_on_abrupt(&self) -> bool {
        matches!(
            self.stage,
            MapConstructorStage::EntryKey
                | MapConstructorStage::EntryValue
                | MapConstructorStage::AdderCall
        )
    }
}

pub(super) struct MapForEachContinuation {
    map: ObjectId,
    callback: FunctionId,
    this_argument: StoredValue,
    next: usize,
    origin: JsStackFrame,
}

impl MapForEachContinuation {
    pub(super) const fn retained_values() -> u64 {
        3
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.map)));
        mark(CollectionRoot::Heap(HeapReference::Function(self.callback)));
        trace_stored_value_root(&self.this_argument, mark);
    }
}

pub(super) struct MapComputedContinuation {
    kind: MapCollectionKind,
    map: ObjectId,
    key: StoredValue,
    origin: JsStackFrame,
}

impl MapComputedContinuation {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.map)));
        trace_stored_value_root(&self.key, mark);
    }
}

pub(super) fn begin_map_constructor(
    runtime: &mut Runtime,
    request: MapConstructorRequest,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let MapConstructorRequest {
        kind,
        realm,
        new_target,
        iterable,
        return_to,
        origin,
    } = request;
    let Some(new_target) = new_target else {
        return map_type_error(realm, origin, kind.constructor_error());
    };
    let receiver = StoredValue::Function(new_target);
    charge_heap_property_lookup(runtime, &receiver, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let continuation = IntrinsicGetContinuation::MapConstructor {
        kind,
        realm,
        new_target,
        iterable,
        origin: origin.clone(),
    };
    let dispatch = begin_internal_get(
        runtime,
        HeapReference::Function(new_target),
        receiver,
        key,
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    continue_intrinsic_get_after(runtime, dispatch, continuation, return_to, execution_budget)
}

#[allow(
    clippy::too_many_arguments,
    reason = "Map construction retains every OrdinaryCreateFromConstructor and AddEntriesFromIterable input across the prototype Get"
)]
pub(super) fn finish_map_constructor_after_prototype_get(
    runtime: &mut Runtime,
    kind: MapCollectionKind,
    realm: RealmId,
    new_target: FunctionId,
    iterable: StoredValue,
    origin: JsStackFrame,
    return_to: Option<CallReturn>,
    requested: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        _ => {
            let target_realm = runtime.function_realm(new_target)?;
            HeapReference::Object(match kind {
                MapCollectionKind::Map => runtime.realm_map_prototype(target_realm)?,
                MapCollectionKind::WeakMap => runtime.realm_weak_map_prototype(target_realm)?,
            })
        }
    };
    let target = match kind {
        MapCollectionKind::Map => runtime.allocate_map_object(prototype)?,
        MapCollectionKind::WeakMap => runtime.allocate_weak_map_object(prototype)?,
    };
    if matches!(iterable, StoredValue::Undefined | StoredValue::Null) {
        return Ok(NativeDispatch::Immediate(StoredValue::Object(target)));
    }
    let state = MapConstructorContinuation {
        target,
        iterable,
        adder: None,
        iterator: None,
        next: None,
        result: None,
        entry: None,
        key: None,
        value: None,
        realm,
        stage: MapConstructorStage::Adder,
        origin,
    };
    let set_key = runtime.property_key_from_string(&JsString::from_utf8("set")?)?;
    read_map_constructor_property(runtime, state, &set_key, return_to, execution_budget)
}

#[allow(
    clippy::too_many_lines,
    reason = "one explicit match keeps every normative AddEntriesFromIterable boundary auditable"
)]
pub(super) fn advance_map_constructor(
    runtime: &mut Runtime,
    mut state: MapConstructorContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        MapConstructorStage::Adder => {
            let StoredValue::Function(adder) = completion else {
                return map_type_error(state.realm, state.origin, "not a function");
            };
            state.adder = Some(adder);
            state.stage = MapConstructorStage::IteratorMethod;
            read_map_constructor_property(
                runtime,
                state,
                &runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
                return_to,
                execution_budget,
            )
        }
        MapConstructorStage::IteratorMethod => {
            let StoredValue::Function(method) = completion else {
                return map_type_error(state.realm, state.origin, "value is not iterable");
            };
            let receiver = state.iterable.duplicate();
            state.stage = MapConstructorStage::Iterator;
            call_map_constructor_function(method, receiver, Vec::new(), state, return_to)
        }
        MapConstructorStage::Iterator => {
            if completion.heap_reference().is_none() {
                return map_type_error(state.realm, state.origin, "not an object");
            }
            state.iterator = Some(completion);
            state.stage = MapConstructorStage::NextMethod;
            read_map_constructor_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        MapConstructorStage::NextMethod => {
            let StoredValue::Function(next) = completion else {
                return map_type_error(state.realm, state.origin, "not a function");
            };
            state.next = Some(next);
            call_map_constructor_next(state, return_to, execution_budget)
        }
        MapConstructorStage::NextResult => {
            if completion.heap_reference().is_none() {
                return map_type_error(state.realm, state.origin, "iterator must return an object");
            }
            state.result = Some(completion);
            state.stage = MapConstructorStage::Done;
            read_map_constructor_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        MapConstructorStage::Done => {
            if runtime.to_boolean(&completion)? {
                return Ok(NativeDispatch::Immediate(StoredValue::Object(state.target)));
            }
            state.stage = MapConstructorStage::IteratorValue;
            read_map_constructor_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        MapConstructorStage::IteratorValue => {
            if completion.heap_reference().is_none() {
                return close_map_constructor_with_type_error(
                    runtime,
                    state,
                    "not an object",
                    return_to,
                    execution_budget,
                );
            }
            state.entry = Some(completion);
            state.stage = MapConstructorStage::EntryKey;
            read_map_constructor_property(
                runtime,
                state,
                &PropertyKey::from_index(ArrayIndex::new(0).ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "Map entry key index zero was rejected",
                    },
                )?),
                return_to,
                execution_budget,
            )
        }
        MapConstructorStage::EntryKey => {
            state.key = Some(completion);
            state.stage = MapConstructorStage::EntryValue;
            read_map_constructor_property(
                runtime,
                state,
                &PropertyKey::from_index(ArrayIndex::new(1).ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "Map entry value index one was rejected",
                    },
                )?),
                return_to,
                execution_budget,
            )
        }
        MapConstructorStage::EntryValue => {
            let key = state.key.take().ok_or(EngineFault::RuntimeInvariant {
                message: "Map constructor reached adder call without a key",
            })?;
            state.value = Some(completion);
            let value = state.value.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "Map constructor reached adder call without a value",
            })?;
            let arguments = try_values([key, value.duplicate()])?;
            let adder = state.adder.ok_or(EngineFault::RuntimeInvariant {
                message: "Map constructor reached entry addition without an adder",
            })?;
            state.stage = MapConstructorStage::AdderCall;
            call_map_constructor_function(
                adder,
                StoredValue::Object(state.target),
                arguments,
                state,
                return_to,
            )
        }
        MapConstructorStage::AdderCall => {
            state.result = None;
            state.entry = None;
            state.value = None;
            call_map_constructor_next(state, return_to, execution_budget)
        }
    }
}

fn read_map_constructor_property(
    runtime: &mut Runtime,
    state: MapConstructorContinuation,
    key: &PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (base, property_name) = match state.stage {
        MapConstructorStage::Adder => (StoredValue::Object(state.target), "set"),
        MapConstructorStage::IteratorMethod => (state.iterable.duplicate(), "Symbol.iterator"),
        MapConstructorStage::NextMethod => (
            state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Map constructor next lookup has no iterator",
                })?
                .duplicate(),
            "next",
        ),
        MapConstructorStage::Done => (
            state
                .result
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Map constructor done lookup has no result",
                })?
                .duplicate(),
            "done",
        ),
        MapConstructorStage::IteratorValue => (
            state
                .result
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Map constructor value lookup has no result",
                })?
                .duplicate(),
            "value",
        ),
        MapConstructorStage::EntryKey => (
            state
                .entry
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Map constructor key lookup has no entry",
                })?
                .duplicate(),
            "0",
        ),
        MapConstructorStage::EntryValue => (
            state
                .entry
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Map constructor value lookup has no entry",
                })?
                .duplicate(),
            "1",
        ),
        MapConstructorStage::Iterator
        | MapConstructorStage::NextResult
        | MapConstructorStage::AdderCall => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Map constructor call stage attempted a property read",
            }
            .into());
        }
    };
    charge_iterator_property_lookup(runtime, &base, execution_budget)?;
    let name = JsString::from_utf8(property_name)?;
    let dispatch = match begin_value_get(
        runtime,
        &base,
        key.clone(),
        Some(&name),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending)) if state.closes_on_abrupt() => {
            return begin_map_constructor_close(
                runtime,
                state,
                pending,
                return_to,
                execution_budget,
            );
        }
        Err(failure) => return Err(failure),
    };
    continue_get_after(
        dispatch,
        state,
        map_constructor_continuation,
        |state, value| advance_map_constructor(runtime, state, value, return_to, execution_budget),
        "Map constructor Get produced a structured result",
    )
}

fn map_constructor_continuation(state: MapConstructorContinuation) -> NativeContinuation {
    NativeContinuation::MapConstructor(Box::new(state))
}

fn call_map_constructor_next(
    mut state: MapConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let next = state.next.ok_or(EngineFault::RuntimeInvariant {
        message: "Map constructor has no retained next method",
    })?;
    let receiver = state
        .iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Map constructor has no retained iterator",
        })?
        .duplicate();
    execution_budget.charge_instructions(1)?;
    state.stage = MapConstructorStage::NextResult;
    call_map_constructor_function(next, receiver, Vec::new(), state, return_to)
}

fn call_map_constructor_function(
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
    state: MapConstructorContinuation,
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
    continuations.push(NativeContinuation::MapConstructor(Box::new(state)));
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

fn begin_map_constructor_close(
    runtime: &mut Runtime,
    state: MapConstructorContinuation,
    original: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let iterator = state.iterator.ok_or(EngineFault::RuntimeInvariant {
        message: "Map IteratorClose started before iterator acquisition",
    })?;
    begin_exceptional_iterator_close(runtime, iterator, original, return_to, execution_budget)
}

fn close_map_constructor_with_type_error(
    runtime: &mut Runtime,
    state: MapConstructorContinuation,
    message: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let pending = pending_map_type_error(state.realm, state.origin.clone(), message)?;
    begin_map_constructor_close(runtime, state, pending, return_to, execution_budget)
}

pub(super) fn resume_map_constructor_abrupt(
    runtime: &mut Runtime,
    state: MapConstructorContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.closes_on_abrupt() {
        begin_map_constructor_close(runtime, state, pending, return_to, execution_budget)
    } else {
        Err(NativeFailure::Abrupt(pending))
    }
}

pub(super) struct MapMethodContext {
    pub(super) realm: RealmId,
    pub(super) return_to: Option<CallReturn>,
    pub(super) origin: JsStackFrame,
}

pub(super) fn dispatch_map_method(
    runtime: &mut Runtime,
    method: MapMethod,
    receiver: StoredValue,
    mut arguments: CallArguments,
    context: MapMethodContext,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let MapMethodContext {
        realm,
        return_to,
        origin,
    } = context;
    let map = require_map(runtime, &receiver, realm, &origin)?;
    execution_budget.charge_instructions(1)?;
    match method {
        MapMethod::Set => {
            let key = arguments.take_first_or_undefined();
            let value = arguments.take_first_or_undefined();
            runtime.map_set(map, key, value)?;
            Ok(NativeDispatch::Immediate(receiver))
        }
        MapMethod::Get => {
            let key = arguments.take_first_or_undefined();
            let value = map_state(runtime, map)?
                .get(&key)
                .map_or(StoredValue::Undefined, StoredValue::duplicate);
            Ok(NativeDispatch::Immediate(value))
        }
        MapMethod::Has => {
            let key = arguments.take_first_or_undefined();
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(
                map_state(runtime, map)?.contains_key(&key),
            )))
        }
        MapMethod::Delete => {
            let key = arguments.take_first_or_undefined();
            let deleted = map_state_mut(runtime, map)?.delete(&key);
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(deleted)))
        }
        MapMethod::Clear => {
            map_state_mut(runtime, map)?.clear();
            Ok(NativeDispatch::Immediate(StoredValue::Undefined))
        }
        MapMethod::Size => Ok(NativeDispatch::Immediate(StoredValue::Number(
            map_size_number(map_state(runtime, map)?.len()),
        ))),
        MapMethod::GetOrInsert => {
            let key = arguments.take_first_or_undefined();
            if let Some(value) = map_state(runtime, map)?.get(&key) {
                return Ok(NativeDispatch::Immediate(value.duplicate()));
            }
            let value = arguments.take_first_or_undefined();
            runtime.map_set(map, key, value.duplicate())?;
            Ok(NativeDispatch::Immediate(value))
        }
        MapMethod::GetOrInsertComputed => {
            let key = arguments.take_first_or_undefined();
            let callback = arguments.take_first_or_undefined();
            let StoredValue::Function(callback) = callback else {
                return map_type_error(realm, origin, "not a function");
            };
            if let Some(value) = map_state(runtime, map)?.get(&key) {
                return Ok(NativeDispatch::Immediate(value.duplicate()));
            }
            begin_map_computed_call(
                MapCollectionKind::Map,
                map,
                key,
                callback,
                origin,
                return_to,
            )
        }
        MapMethod::ForEach => {
            let callback = arguments.take_first_or_undefined();
            let StoredValue::Function(callback) = callback else {
                return map_type_error(realm, origin, "not a function");
            };
            let state = MapForEachContinuation {
                map,
                callback,
                this_argument: arguments.take_first_or_undefined(),
                next: 0,
                origin,
            };
            advance_map_for_each(runtime, state, return_to, execution_budget)
        }
        MapMethod::Values | MapMethod::Keys | MapMethod::Entries => {
            let kind = match method {
                MapMethod::Values => MapIteratorKind::Value,
                MapMethod::Keys => MapIteratorKind::Key,
                MapMethod::Entries => MapIteratorKind::KeyAndValue,
                _ => unreachable!("Map iterator method arm is exhaustive"),
            };
            Ok(NativeDispatch::Immediate(StoredValue::Object(
                runtime.allocate_map_iterator(realm, map, kind)?,
            )))
        }
    }
}

pub(super) fn resume_map_computed(
    runtime: &mut Runtime,
    state: MapComputedContinuation,
    completion: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    match state.kind {
        MapCollectionKind::Map => {
            runtime.map_set(state.map, state.key, completion.duplicate())?;
        }
        MapCollectionKind::WeakMap => {
            runtime.weak_map_set(state.map, &state.key, completion.duplicate())?;
        }
    }
    Ok(NativeDispatch::Immediate(completion))
}

pub(super) fn begin_map_computed_call(
    kind: MapCollectionKind,
    map: ObjectId,
    key: StoredValue,
    callback: FunctionId,
    origin: JsStackFrame,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let arguments = try_values([key.duplicate()])?;
    let state = MapComputedContinuation {
        kind,
        map,
        key,
        origin,
    };
    let origin = state.origin.clone();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::MapComputed(Box::new(state)));
    Ok(NativeDispatch::Call(NativeCall {
        function: callback,
        receiver: StoredValue::Undefined,
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

pub(super) fn advance_map_for_each(
    runtime: &mut Runtime,
    mut state: MapForEachContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        execution_budget.charge_instructions(1)?;
        let Some(entry) = map_state(runtime, state.map)?.entry(state.next) else {
            return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
        };
        state.next = state.next.saturating_add(1);
        if !entry.is_live() {
            continue;
        }
        let arguments = try_values([
            entry.value().duplicate(),
            entry.key().duplicate(),
            StoredValue::Object(state.map),
        ])?;
        let origin = state.origin.clone();
        let callback = state.callback;
        let receiver = state.this_argument.duplicate();
        let mut continuations = Vec::new();
        continuations
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::Frames,
                additional: 1,
            })?;
        continuations.push(NativeContinuation::MapForEach(Box::new(state)));
        return Ok(NativeDispatch::Call(NativeCall {
            function: callback,
            receiver,
            arguments: CallArguments::from_values(arguments),
            return_to,
            origin,
            continuations,
            pre_call: None,
            new_target: None,
            native_caller: None,
        }));
    }
}

pub(super) fn begin_map_iterator_next(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(iterator) = receiver else {
        return map_type_error(realm, origin, "Map Iterator object expected");
    };
    let iterator = *iterator;
    let (map, kind, mut next) = {
        let object = runtime
            .objects
            .get(iterator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "Map Iterator object",
                index: iterator.index(),
                generation: iterator.generation(),
            })?;
        let Some(state) = object.map_iterator_state() else {
            return map_type_error(realm, origin, "Map Iterator object expected");
        };
        let Some(map) = state.iterated() else {
            return iterator_result(runtime, realm, StoredValue::Undefined, true);
        };
        (map, state.kind(), state.next())
    };
    let found = loop {
        execution_budget.charge_instructions(1)?;
        let Some(entry) = map_state(runtime, map)?.entry(next) else {
            break None;
        };
        next = next.saturating_add(1);
        if entry.is_live() {
            break Some((entry.key().duplicate(), entry.value().duplicate()));
        }
    };
    let entry_key = match (&found, kind) {
        (Some((key, _)), MapIteratorKind::KeyAndValue) => Some(key.duplicate()),
        _ => None,
    };
    let prepared = runtime.prepare_iterator_result_allocation(realm, entry_key)?;
    let (result, done) = match found {
        Some((key, value)) => {
            let value = match kind {
                MapIteratorKind::Key => key,
                MapIteratorKind::Value | MapIteratorKind::KeyAndValue => value,
            };
            (
                runtime.commit_prepared_iterator_result(prepared, value, false)?,
                false,
            )
        }
        None => (
            runtime.commit_prepared_iterator_result(prepared, StoredValue::Undefined, true)?,
            true,
        ),
    };
    let state = runtime
        .objects
        .get_mut(iterator)
        .and_then(crate::object::HeapObject::map_iterator_state_mut)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "validated Map Iterator disappeared",
        })?;
    while state.next() < next {
        state.advance();
    }
    if done {
        state.finish();
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
}

fn require_map(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<ObjectId, NativeFailure> {
    let StoredValue::Object(map) = receiver else {
        return Err(NativeFailure::Abrupt(pending_map_type_error(
            realm,
            origin.clone(),
            "not a Map object",
        )?));
    };
    let object = runtime
        .objects
        .get(*map)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "Map object",
            index: map.index(),
            generation: map.generation(),
        })?;
    if object.map_state().is_none() {
        return Err(NativeFailure::Abrupt(pending_map_type_error(
            realm,
            origin.clone(),
            "not a Map object",
        )?));
    }
    Ok(*map)
}

fn map_state(runtime: &Runtime, map: ObjectId) -> Result<&MapState, NativeFailure> {
    runtime
        .objects
        .get(map)
        .and_then(crate::object::HeapObject::map_state)
        .ok_or_else(|| {
            EngineFault::StaleHeapEdge {
                edge: "Map object",
                index: map.index(),
                generation: map.generation(),
            }
            .into()
        })
}

fn map_state_mut(runtime: &mut Runtime, map: ObjectId) -> Result<&mut MapState, NativeFailure> {
    runtime
        .objects
        .get_mut(map)
        .and_then(crate::object::HeapObject::map_state_mut)
        .ok_or_else(|| {
            EngineFault::StaleHeapEdge {
                edge: "Map object",
                index: map.index(),
                generation: map.generation(),
            }
            .into()
        })
}

fn try_values<const N: usize>(values: [StoredValue; N]) -> Result<Vec<StoredValue>, NativeFailure> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(N)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: N,
        })?;
    output.extend(values);
    Ok(output)
}

fn map_size_number(length: usize) -> JsNumber {
    let length = usize_to_u64(length);
    let high = u32::try_from(length >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(length & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    JsNumber::from_f64(f64::from(high) * 4_294_967_296.0 + f64::from(low))
}

fn map_type_error(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(pending_map_type_error(
        realm, origin, message,
    )?))
}

fn pending_map_type_error(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin,
    })
}
