/*
 * JavaScript Set semantics derived from QuickJS.
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

//! Typed, resumable Set construction, callback, and iterator semantics.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use crate::object::{SetIteratorKind, SetState};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SetCollectionKind {
    Set,
    WeakSet,
}

impl SetCollectionKind {
    const fn constructor_error(self) -> &'static str {
        match self {
            Self::Set => "Set constructor requires 'new'",
            Self::WeakSet => "WeakSet constructor requires 'new'",
        }
    }
}

pub(super) struct SetConstructorRequest {
    kind: SetCollectionKind,
    realm: RealmId,
    new_target: Option<FunctionId>,
    iterable: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
}

impl SetConstructorRequest {
    pub(super) const fn new(
        kind: SetCollectionKind,
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
pub(super) enum SetConstructorStage {
    Adder,
    IteratorMethod,
    Iterator,
    NextMethod,
    NextResult,
    Done,
    IteratorValue,
    AdderCall,
}

pub(super) struct SetConstructorContinuation {
    target: ObjectId,
    iterable: StoredValue,
    adder: Option<FunctionId>,
    iterator: Option<StoredValue>,
    next: Option<FunctionId>,
    result: Option<StoredValue>,
    realm: RealmId,
    stage: SetConstructorStage,
    origin: JsStackFrame,
}

impl SetConstructorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.adder.is_some()))
            .saturating_add(u64::from(self.iterator.is_some()))
            .saturating_add(u64::from(self.next.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
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
        for value in [self.iterator.as_ref(), self.result.as_ref()]
            .into_iter()
            .flatten()
        {
            trace_stored_value_root(value, mark);
        }
    }

    const fn closes_on_abrupt(&self) -> bool {
        matches!(self.stage, SetConstructorStage::AdderCall)
    }
}

pub(super) struct SetForEachContinuation {
    set: ObjectId,
    callback: FunctionId,
    this_argument: StoredValue,
    next: usize,
    origin: JsStackFrame,
}

impl SetForEachContinuation {
    pub(super) const fn retained_values() -> u64 {
        3
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.set)));
        mark(CollectionRoot::Heap(HeapReference::Function(self.callback)));
        trace_stored_value_root(&self.this_argument, mark);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SetOperationStage {
    Size,
    Has,
    Keys,
    HasCall,
    KeysCall,
    IteratorNextMethod,
    IteratorNextCall,
    IteratorDone,
    IteratorValue,
    IteratorReturnProperty,
    IteratorReturnCall,
}

pub(super) struct SetOperationContinuation {
    method: SetMethod,
    set: ObjectId,
    other: StoredValue,
    other_size: f64,
    has: Option<FunctionId>,
    keys: Option<FunctionId>,
    result: Option<ObjectId>,
    current: Option<StoredValue>,
    iterator: Option<StoredValue>,
    next: Option<FunctionId>,
    iterator_result: Option<StoredValue>,
    cursor: usize,
    internal_limit: Option<usize>,
    realm: RealmId,
    stage: SetOperationStage,
    origin: JsStackFrame,
}

impl SetOperationContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.has.is_some()))
            .saturating_add(u64::from(self.keys.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
            .saturating_add(u64::from(self.current.is_some()))
            .saturating_add(u64::from(self.iterator.is_some()))
            .saturating_add(u64::from(self.next.is_some()))
            .saturating_add(u64::from(self.iterator_result.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.set)));
        trace_stored_value_root(&self.other, mark);
        for function in [self.has, self.keys, self.next].into_iter().flatten() {
            mark(CollectionRoot::Heap(HeapReference::Function(function)));
        }
        if let Some(result) = self.result {
            mark(CollectionRoot::Heap(HeapReference::Object(result)));
        }
        for value in [
            self.current.as_ref(),
            self.iterator.as_ref(),
            self.iterator_result.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            trace_stored_value_root(value, mark);
        }
    }
}

pub(super) fn begin_set_constructor(
    runtime: &mut Runtime,
    request: SetConstructorRequest,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let SetConstructorRequest {
        kind,
        realm,
        new_target,
        iterable,
        return_to,
        origin,
    } = request;
    let Some(new_target) = new_target else {
        return set_type_error(realm, origin, kind.constructor_error());
    };
    let receiver = StoredValue::Function(new_target);
    charge_heap_property_lookup(runtime, &receiver, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let continuation = IntrinsicGetContinuation::SetConstructor {
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
    reason = "Set construction retains every OrdinaryCreateFromConstructor input across the prototype Get"
)]
pub(super) fn finish_set_constructor_after_prototype_get(
    runtime: &mut Runtime,
    kind: SetCollectionKind,
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
                SetCollectionKind::Set => runtime.realm_set_prototype(target_realm)?,
                SetCollectionKind::WeakSet => runtime.realm_weak_set_prototype(target_realm)?,
            })
        }
    };
    let target = match kind {
        SetCollectionKind::Set => runtime.allocate_set_object(prototype)?,
        SetCollectionKind::WeakSet => runtime.allocate_weak_set_object(prototype)?,
    };
    if matches!(iterable, StoredValue::Undefined | StoredValue::Null) {
        return Ok(NativeDispatch::Immediate(StoredValue::Object(target)));
    }
    let state = SetConstructorContinuation {
        target,
        iterable,
        adder: None,
        iterator: None,
        next: None,
        result: None,
        realm,
        stage: SetConstructorStage::Adder,
        origin,
    };
    read_set_constructor_property(
        runtime,
        state,
        &runtime.predefined_property_key(PredefinedAtom::Add),
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "one explicit match keeps every normative Set constructor iterator boundary auditable"
)]
pub(super) fn advance_set_constructor(
    runtime: &mut Runtime,
    mut state: SetConstructorContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        SetConstructorStage::Adder => {
            let StoredValue::Function(adder) = completion else {
                return set_type_error(state.realm, state.origin, "not a function");
            };
            state.adder = Some(adder);
            state.stage = SetConstructorStage::IteratorMethod;
            read_set_constructor_property(
                runtime,
                state,
                &runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
                return_to,
                execution_budget,
            )
        }
        SetConstructorStage::IteratorMethod => {
            let StoredValue::Function(method) = completion else {
                return set_type_error(state.realm, state.origin, "value is not iterable");
            };
            let receiver = state.iterable.duplicate();
            state.stage = SetConstructorStage::Iterator;
            call_set_constructor_function(method, receiver, Vec::new(), state, return_to)
        }
        SetConstructorStage::Iterator => {
            if completion.heap_reference().is_none() {
                return set_type_error(state.realm, state.origin, "not an object");
            }
            state.iterator = Some(completion);
            state.stage = SetConstructorStage::NextMethod;
            read_set_constructor_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        SetConstructorStage::NextMethod => {
            let StoredValue::Function(next) = completion else {
                return set_type_error(state.realm, state.origin, "not a function");
            };
            state.next = Some(next);
            call_set_constructor_next(state, return_to, execution_budget)
        }
        SetConstructorStage::NextResult => {
            if completion.heap_reference().is_none() {
                return set_type_error(state.realm, state.origin, "iterator must return an object");
            }
            state.result = Some(completion);
            state.stage = SetConstructorStage::Done;
            read_set_constructor_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        SetConstructorStage::Done => {
            if completion.is_truthy() {
                return Ok(NativeDispatch::Immediate(StoredValue::Object(state.target)));
            }
            state.stage = SetConstructorStage::IteratorValue;
            read_set_constructor_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        SetConstructorStage::IteratorValue => {
            let arguments = set_values([completion])?;
            let adder = state.adder.ok_or(EngineFault::RuntimeInvariant {
                message: "Set constructor reached entry addition without an adder",
            })?;
            state.stage = SetConstructorStage::AdderCall;
            call_set_constructor_function(
                adder,
                StoredValue::Object(state.target),
                arguments,
                state,
                return_to,
            )
        }
        SetConstructorStage::AdderCall => {
            state.result = None;
            call_set_constructor_next(state, return_to, execution_budget)
        }
    }
}

fn read_set_constructor_property(
    runtime: &mut Runtime,
    state: SetConstructorContinuation,
    key: &PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (base, property_name) = match state.stage {
        SetConstructorStage::Adder => (StoredValue::Object(state.target), "add"),
        SetConstructorStage::IteratorMethod => (state.iterable.duplicate(), "Symbol.iterator"),
        SetConstructorStage::NextMethod => (
            state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Set constructor next lookup has no iterator",
                })?
                .duplicate(),
            "next",
        ),
        SetConstructorStage::Done => (
            state
                .result
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Set constructor done lookup has no result",
                })?
                .duplicate(),
            "done",
        ),
        SetConstructorStage::IteratorValue => (
            state
                .result
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Set constructor value lookup has no result",
                })?
                .duplicate(),
            "value",
        ),
        SetConstructorStage::Iterator
        | SetConstructorStage::NextResult
        | SetConstructorStage::AdderCall => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Set constructor call stage attempted a property read",
            }
            .into());
        }
    };
    charge_iterator_property_lookup(runtime, &base, execution_budget)?;
    let name = JsString::from_utf8(property_name)?;
    let dispatch = begin_value_get(
        runtime,
        &base,
        key.clone(),
        Some(&name),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        set_constructor_continuation,
        |state, value| advance_set_constructor(runtime, state, value, return_to, execution_budget),
        "Set constructor Get produced a structured result",
    )
}

fn set_constructor_continuation(state: SetConstructorContinuation) -> NativeContinuation {
    NativeContinuation::SetConstructor(Box::new(state))
}

fn call_set_constructor_next(
    mut state: SetConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let next = state.next.ok_or(EngineFault::RuntimeInvariant {
        message: "Set constructor has no retained next method",
    })?;
    let receiver = state
        .iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Set constructor has no retained iterator",
        })?
        .duplicate();
    execution_budget.charge_instructions(1)?;
    state.stage = SetConstructorStage::NextResult;
    call_set_constructor_function(next, receiver, Vec::new(), state, return_to)
}

fn call_set_constructor_function(
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
    state: SetConstructorContinuation,
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
    continuations.push(NativeContinuation::SetConstructor(Box::new(state)));
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

fn begin_set_constructor_close(
    runtime: &mut Runtime,
    state: SetConstructorContinuation,
    original: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let iterator = state.iterator.ok_or(EngineFault::RuntimeInvariant {
        message: "Set IteratorClose started before iterator acquisition",
    })?;
    begin_exceptional_iterator_close(runtime, iterator, original, return_to, execution_budget)
}

pub(super) fn resume_set_constructor_abrupt(
    runtime: &mut Runtime,
    state: SetConstructorContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.closes_on_abrupt() {
        begin_set_constructor_close(runtime, state, pending, return_to, execution_budget)
    } else {
        Err(NativeFailure::Abrupt(pending))
    }
}

pub(super) struct SetMethodContext {
    pub(super) realm: RealmId,
    pub(super) return_to: Option<CallReturn>,
    pub(super) origin: JsStackFrame,
}

pub(super) fn dispatch_set_method(
    runtime: &mut Runtime,
    method: SetMethod,
    receiver: StoredValue,
    mut arguments: CallArguments,
    context: SetMethodContext,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let SetMethodContext {
        realm,
        return_to,
        origin,
    } = context;
    let set = require_set(runtime, &receiver, realm, &origin)?;
    execution_budget.charge_instructions(1)?;
    match method {
        SetMethod::Add => {
            let value = arguments.take_first_or_undefined();
            runtime.set_add(set, value)?;
            Ok(NativeDispatch::Immediate(receiver))
        }
        SetMethod::Has => {
            let value = arguments.take_first_or_undefined();
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(
                set_state(runtime, set)?.contains(&value),
            )))
        }
        SetMethod::Delete => {
            let value = arguments.take_first_or_undefined();
            let deleted = set_state_mut(runtime, set)?.delete(&value);
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(deleted)))
        }
        SetMethod::Clear => {
            let work = usize_to_u64(set_state(runtime, set)?.retained_len());
            execution_budget.charge_instructions(work)?;
            set_state_mut(runtime, set)?.clear();
            Ok(NativeDispatch::Immediate(StoredValue::Undefined))
        }
        SetMethod::Size => Ok(NativeDispatch::Immediate(StoredValue::Number(
            set_size_number(set_state(runtime, set)?.len()),
        ))),
        SetMethod::ForEach => {
            let callback = arguments.take_first_or_undefined();
            let StoredValue::Function(callback) = callback else {
                return set_type_error(realm, origin, "not a function");
            };
            let state = SetForEachContinuation {
                set,
                callback,
                this_argument: arguments.take_first_or_undefined(),
                next: 0,
                origin,
            };
            advance_set_for_each(runtime, state, return_to, execution_budget)
        }
        SetMethod::Values | SetMethod::Entries => {
            let kind = if method == SetMethod::Entries {
                SetIteratorKind::KeyAndValue
            } else {
                SetIteratorKind::Value
            };
            Ok(NativeDispatch::Immediate(StoredValue::Object(
                runtime.allocate_set_iterator(realm, set, kind)?,
            )))
        }
        SetMethod::IsDisjointFrom
        | SetMethod::IsSubsetOf
        | SetMethod::IsSupersetOf
        | SetMethod::Intersection
        | SetMethod::Difference
        | SetMethod::SymmetricDifference
        | SetMethod::Union => begin_set_operation(
            runtime,
            method,
            set,
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin,
            execution_budget,
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "GetSetRecord retains the branded receiver, arbitrary set-like object, realm, caller, and fuel authority"
)]
fn begin_set_operation(
    runtime: &mut Runtime,
    method: SetMethod,
    set: ObjectId,
    other: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if other.heap_reference().is_none() {
        return set_type_error(realm, origin, "not an object");
    }
    let state = SetOperationContinuation {
        method,
        set,
        other,
        other_size: 0.0,
        has: None,
        keys: None,
        result: None,
        current: None,
        iterator: None,
        next: None,
        iterator_result: None,
        cursor: 0,
        internal_limit: None,
        realm,
        stage: SetOperationStage::Size,
        origin,
    };
    read_set_operation_property(
        runtime,
        state,
        &runtime.predefined_property_key(PredefinedAtom::Size),
        return_to,
        execution_budget,
    )
}

pub(super) fn finish_set_record_size(
    runtime: &mut Runtime,
    mut state: SetOperationContinuation,
    size: JsNumber,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let size = size.as_f64();
    if size.is_nan() {
        return set_type_error(state.realm, state.origin, "invalid Set-like size");
    }
    let size = if size == 0.0 || size.is_infinite() {
        size
    } else {
        size.trunc()
    };
    if size < 0.0 {
        return set_range_error(state.realm, state.origin, "negative Set-like size");
    }
    state.other_size = size;
    state.stage = SetOperationStage::Has;
    read_set_operation_property(
        runtime,
        state,
        &runtime.predefined_property_key(PredefinedAtom::Has),
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "one state machine preserves GetSetRecord, set-like calls, IteratorStepValue, and normal IteratorClose order"
)]
pub(super) fn advance_set_operation(
    runtime: &mut Runtime,
    mut state: SetOperationContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        SetOperationStage::Size => {
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                completion,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::SetRecordSize(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        SetOperationStage::Has => {
            let StoredValue::Function(has) = completion else {
                return set_type_error(state.realm, state.origin, "not a function");
            };
            state.has = Some(has);
            state.stage = SetOperationStage::Keys;
            read_set_operation_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Keys),
                return_to,
                execution_budget,
            )
        }
        SetOperationStage::Keys => {
            let StoredValue::Function(keys) = completion else {
                return set_type_error(state.realm, state.origin, "not a function");
            };
            state.keys = Some(keys);
            start_set_algorithm(runtime, state, return_to, execution_budget)
        }
        SetOperationStage::HasCall => {
            let current = state.current.take().ok_or(EngineFault::RuntimeInvariant {
                message: "Set operation resumed a has call without its value",
            })?;
            let in_other = completion.is_truthy();
            match state.method {
                SetMethod::Difference => {
                    if in_other {
                        let result = state.result.ok_or(EngineFault::RuntimeInvariant {
                            message: "Set difference has no result",
                        })?;
                        set_state_mut(runtime, result)?.delete(&current);
                    }
                }
                SetMethod::Intersection => {
                    if in_other {
                        let result = state.result.ok_or(EngineFault::RuntimeInvariant {
                            message: "Set intersection has no result",
                        })?;
                        if !set_state(runtime, result)?.contains(&current) {
                            runtime.set_add(result, current)?;
                        }
                    }
                }
                SetMethod::IsDisjointFrom => {
                    if in_other {
                        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
                    }
                }
                SetMethod::IsSubsetOf => {
                    if !in_other {
                        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
                    }
                }
                _ => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "Set operation used has in an iterator-only algorithm",
                    }
                    .into());
                }
            }
            advance_set_internal_loop(runtime, state, return_to, execution_budget)
        }
        SetOperationStage::KeysCall => {
            if completion.heap_reference().is_none() {
                return set_type_error(state.realm, state.origin, "not an object");
            }
            state.iterator = Some(completion);
            state.stage = SetOperationStage::IteratorNextMethod;
            read_set_operation_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        SetOperationStage::IteratorNextMethod => {
            let StoredValue::Function(next) = completion else {
                return set_type_error(state.realm, state.origin, "not a function");
            };
            state.next = Some(next);
            call_set_operation_next(state, return_to, execution_budget)
        }
        SetOperationStage::IteratorNextCall => {
            if completion.heap_reference().is_none() {
                return set_type_error(state.realm, state.origin, "iterator must return an object");
            }
            state.iterator_result = Some(completion);
            state.stage = SetOperationStage::IteratorDone;
            read_set_operation_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        SetOperationStage::IteratorDone => {
            if completion.is_truthy() {
                return finish_set_operation(&state);
            }
            state.stage = SetOperationStage::IteratorValue;
            read_set_operation_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        SetOperationStage::IteratorValue => process_set_operation_iterator_value(
            runtime,
            state,
            completion,
            return_to,
            execution_budget,
        ),
        SetOperationStage::IteratorReturnProperty => match completion {
            StoredValue::Undefined | StoredValue::Null => {
                Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
            }
            StoredValue::Function(return_method) => {
                let receiver = state
                    .iterator
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Set normal IteratorClose has no iterator",
                    })?
                    .duplicate();
                state.stage = SetOperationStage::IteratorReturnCall;
                call_set_operation_function(return_method, receiver, Vec::new(), state, return_to)
            }
            _ => set_type_error(state.realm, state.origin, "not a function"),
        },
        SetOperationStage::IteratorReturnCall => {
            if completion.heap_reference().is_none() {
                return set_type_error(
                    state.realm,
                    state.origin,
                    "iterator return must return an object",
                );
            }
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
        }
    }
}

fn start_set_algorithm(
    runtime: &mut Runtime,
    mut state: SetOperationContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let this_size = set_size_number(set_state(runtime, state.set)?.len()).as_f64();
    match state.method {
        SetMethod::IsSubsetOf if this_size > state.other_size => {
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
        }
        SetMethod::IsSupersetOf if this_size < state.other_size => {
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
        }
        SetMethod::Difference => {
            let result = copy_set_to_intrinsic(runtime, state.set, state.realm, execution_budget)?;
            state.result = Some(result);
            if this_size <= state.other_size {
                state.internal_limit = Some(set_state(runtime, result)?.retained_len());
                advance_set_internal_loop(runtime, state, return_to, execution_budget)
            } else {
                begin_set_keys_iterator(state, return_to)
            }
        }
        SetMethod::Intersection => {
            state.result = Some(allocate_intrinsic_set(runtime, state.realm)?);
            if this_size <= state.other_size {
                advance_set_internal_loop(runtime, state, return_to, execution_budget)
            } else {
                begin_set_keys_iterator(state, return_to)
            }
        }
        SetMethod::IsDisjointFrom if this_size <= state.other_size => {
            advance_set_internal_loop(runtime, state, return_to, execution_budget)
        }
        SetMethod::IsDisjointFrom | SetMethod::IsSupersetOf => {
            begin_set_keys_iterator(state, return_to)
        }
        SetMethod::IsSubsetOf => {
            advance_set_internal_loop(runtime, state, return_to, execution_budget)
        }
        SetMethod::SymmetricDifference | SetMethod::Union => {
            state.result = Some(copy_set_to_intrinsic(
                runtime,
                state.set,
                state.realm,
                execution_budget,
            )?);
            begin_set_keys_iterator(state, return_to)
        }
        _ => Err(EngineFault::RuntimeInvariant {
            message: "non-operation Set method entered the set-like algorithm",
        }
        .into()),
    }
}

fn advance_set_internal_loop(
    runtime: &mut Runtime,
    mut state: SetOperationContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        execution_budget.charge_instructions(1)?;
        if state
            .internal_limit
            .is_some_and(|limit| state.cursor >= limit)
        {
            return finish_set_operation(&state);
        }
        let iterated = if state.method == SetMethod::Difference {
            state.result.ok_or(EngineFault::RuntimeInvariant {
                message: "Set difference has no result loop",
            })?
        } else {
            state.set
        };
        let Some(entry) = set_state(runtime, iterated)?.entry(state.cursor) else {
            return finish_set_operation(&state);
        };
        state.cursor = state.cursor.saturating_add(1);
        if !entry.is_live() {
            continue;
        }
        let value = entry.key().duplicate();
        let has = state.has.ok_or(EngineFault::RuntimeInvariant {
            message: "Set operation has no retained has method",
        })?;
        state.current = Some(value.duplicate());
        state.stage = SetOperationStage::HasCall;
        return call_set_operation_function(
            has,
            state.other.duplicate(),
            set_values([value])?,
            state,
            return_to,
        );
    }
}

fn begin_set_keys_iterator(
    mut state: SetOperationContinuation,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let keys = state.keys.ok_or(EngineFault::RuntimeInvariant {
        message: "Set operation has no retained keys method",
    })?;
    let receiver = state.other.duplicate();
    state.stage = SetOperationStage::KeysCall;
    call_set_operation_function(keys, receiver, Vec::new(), state, return_to)
}

fn call_set_operation_next(
    mut state: SetOperationContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget.charge_instructions(1)?;
    let next = state.next.ok_or(EngineFault::RuntimeInvariant {
        message: "Set operation has no retained next method",
    })?;
    let receiver = state
        .iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Set operation has no retained iterator",
        })?
        .duplicate();
    state.iterator_result = None;
    state.stage = SetOperationStage::IteratorNextCall;
    call_set_operation_function(next, receiver, Vec::new(), state, return_to)
}

fn process_set_operation_iterator_value(
    runtime: &mut Runtime,
    state: SetOperationContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.method {
        SetMethod::Difference => {
            let result = state.result.ok_or(EngineFault::RuntimeInvariant {
                message: "Set difference has no result",
            })?;
            set_state_mut(runtime, result)?.delete(&value);
        }
        SetMethod::Intersection => {
            let result = state.result.ok_or(EngineFault::RuntimeInvariant {
                message: "Set intersection has no result",
            })?;
            if set_state(runtime, state.set)?.contains(&value)
                && !set_state(runtime, result)?.contains(&value)
            {
                runtime.set_add(result, value)?;
            }
        }
        SetMethod::IsDisjointFrom => {
            if set_state(runtime, state.set)?.contains(&value) {
                return begin_set_normal_iterator_close(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                );
            }
        }
        SetMethod::IsSupersetOf => {
            if !set_state(runtime, state.set)?.contains(&value) {
                return begin_set_normal_iterator_close(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                );
            }
        }
        SetMethod::SymmetricDifference => {
            let result = state.result.ok_or(EngineFault::RuntimeInvariant {
                message: "Set symmetricDifference has no result",
            })?;
            let already = set_state(runtime, result)?.contains(&value);
            if set_state(runtime, state.set)?.contains(&value) {
                if already {
                    set_state_mut(runtime, result)?.delete(&value);
                }
            } else if !already {
                runtime.set_add(result, value)?;
            }
        }
        SetMethod::Union => {
            let result = state.result.ok_or(EngineFault::RuntimeInvariant {
                message: "Set union has no result",
            })?;
            if !set_state(runtime, result)?.contains(&value) {
                runtime.set_add(result, value)?;
            }
        }
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "internal-only Set operation consumed keys iterator",
            }
            .into());
        }
    }
    call_set_operation_next(state, return_to, execution_budget)
}

fn begin_set_normal_iterator_close(
    runtime: &mut Runtime,
    mut state: SetOperationContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = SetOperationStage::IteratorReturnProperty;
    read_set_operation_property(
        runtime,
        state,
        &runtime.predefined_property_key(PredefinedAtom::Return),
        return_to,
        execution_budget,
    )
}

fn finish_set_operation(state: &SetOperationContinuation) -> Result<NativeDispatch, NativeFailure> {
    match state.method {
        SetMethod::Difference
        | SetMethod::Intersection
        | SetMethod::SymmetricDifference
        | SetMethod::Union => Ok(NativeDispatch::Immediate(StoredValue::Object(
            state.result.ok_or(EngineFault::RuntimeInvariant {
                message: "Set composition completed without a result",
            })?,
        ))),
        SetMethod::IsDisjointFrom | SetMethod::IsSubsetOf | SetMethod::IsSupersetOf => {
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)))
        }
        _ => Err(EngineFault::RuntimeInvariant {
            message: "non-operation Set method completed the set-like algorithm",
        }
        .into()),
    }
}

fn read_set_operation_property(
    runtime: &mut Runtime,
    state: SetOperationContinuation,
    key: &PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (base, property_name) = match state.stage {
        SetOperationStage::Size => (state.other.duplicate(), "size"),
        SetOperationStage::Has => (state.other.duplicate(), "has"),
        SetOperationStage::Keys => (state.other.duplicate(), "keys"),
        SetOperationStage::IteratorNextMethod | SetOperationStage::IteratorReturnProperty => (
            state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Set operation iterator property read has no iterator",
                })?
                .duplicate(),
            if state.stage == SetOperationStage::IteratorNextMethod {
                "next"
            } else {
                "return"
            },
        ),
        SetOperationStage::IteratorDone | SetOperationStage::IteratorValue => (
            state
                .iterator_result
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Set operation result property read has no iterator result",
                })?
                .duplicate(),
            if state.stage == SetOperationStage::IteratorDone {
                "done"
            } else {
                "value"
            },
        ),
        SetOperationStage::HasCall
        | SetOperationStage::KeysCall
        | SetOperationStage::IteratorNextCall
        | SetOperationStage::IteratorReturnCall => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Set operation call stage attempted a property read",
            }
            .into());
        }
    };
    charge_iterator_property_lookup(runtime, &base, execution_budget)?;
    let name = JsString::from_utf8(property_name)?;
    let dispatch = begin_value_get(
        runtime,
        &base,
        key.clone(),
        Some(&name),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        set_operation_continuation,
        |state, value| advance_set_operation(runtime, state, value, return_to, execution_budget),
        "Set operation Get produced a structured result",
    )
}

fn set_operation_continuation(state: SetOperationContinuation) -> NativeContinuation {
    NativeContinuation::SetOperation(Box::new(state))
}

fn call_set_operation_function(
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
    state: SetOperationContinuation,
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
    continuations.push(NativeContinuation::SetOperation(Box::new(state)));
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

fn allocate_intrinsic_set(
    runtime: &mut Runtime,
    realm: RealmId,
) -> Result<ObjectId, NativeFailure> {
    let prototype = runtime.realm_set_prototype(realm)?;
    Ok(runtime.allocate_set_object(HeapReference::Object(prototype))?)
}

fn copy_set_to_intrinsic(
    runtime: &mut Runtime,
    source: ObjectId,
    realm: RealmId,
    execution_budget: &mut ExecutionBudget,
) -> Result<ObjectId, NativeFailure> {
    let live = set_state(runtime, source)?.len();
    let retained = set_state(runtime, source)?.retained_len();
    execution_budget.charge_instructions(usize_to_u64(retained))?;
    let mut copied =
        SetState::try_with_capacity(live).map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::CollectionEntries,
            additional: live,
        })?;
    let mut index = 0_usize;
    loop {
        let entry = set_state(runtime, source)?
            .entry(index)
            .map(|entry| entry.is_live().then(|| entry.key().duplicate()));
        let Some(value) = entry else {
            break;
        };
        if let Some(value) = value {
            copied
                .try_add(value)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::CollectionEntries,
                    additional: 1,
                })?;
        }
        index = index.saturating_add(1);
    }
    let prototype = runtime.realm_set_prototype(realm)?;
    Ok(runtime.allocate_set_object_with_state(HeapReference::Object(prototype), copied)?)
}

pub(super) fn advance_set_for_each(
    runtime: &mut Runtime,
    mut state: SetForEachContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        execution_budget.charge_instructions(1)?;
        let Some(entry) = set_state(runtime, state.set)?.entry(state.next) else {
            return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
        };
        state.next = state.next.saturating_add(1);
        if !entry.is_live() {
            continue;
        }
        let value = entry.key().duplicate();
        let arguments = set_values([value.duplicate(), value, StoredValue::Object(state.set)])?;
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
        continuations.push(NativeContinuation::SetForEach(Box::new(state)));
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

pub(super) fn begin_set_iterator_next(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(iterator) = receiver else {
        return set_type_error(realm, origin, "Set Iterator object expected");
    };
    let iterator = *iterator;
    let (set, kind, mut next) = {
        let object = runtime
            .objects
            .get(iterator)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "Set Iterator object",
                index: iterator.index(),
                generation: iterator.generation(),
            })?;
        let Some(state) = object.set_iterator_state() else {
            return set_type_error(realm, origin, "Set Iterator object expected");
        };
        let Some(set) = state.iterated() else {
            return iterator_result(runtime, realm, StoredValue::Undefined, true);
        };
        (set, state.kind(), state.next())
    };
    let found = loop {
        execution_budget.charge_instructions(1)?;
        let Some(entry) = set_state(runtime, set)?.entry(next) else {
            break None;
        };
        next = next.saturating_add(1);
        if entry.is_live() {
            break Some(entry.key().duplicate());
        }
    };
    let entry_value = match (&found, kind) {
        (Some(value), SetIteratorKind::KeyAndValue) => Some(value.duplicate()),
        _ => None,
    };
    let prepared = runtime.prepare_iterator_result_allocation(realm, entry_value)?;
    let (result, done) = match found {
        Some(value) => (
            runtime.commit_prepared_iterator_result(prepared, value, false)?,
            false,
        ),
        None => (
            runtime.commit_prepared_iterator_result(prepared, StoredValue::Undefined, true)?,
            true,
        ),
    };
    let state = runtime
        .objects
        .get_mut(iterator)
        .and_then(crate::object::HeapObject::set_iterator_state_mut)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "validated Set Iterator disappeared",
        })?;
    while state.next() < next {
        state.advance();
    }
    if done {
        state.finish();
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
}

fn require_set(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<ObjectId, NativeFailure> {
    let StoredValue::Object(set) = receiver else {
        return Err(NativeFailure::Abrupt(pending_set_type_error(
            realm,
            origin.clone(),
            "not a Set object",
        )?));
    };
    let object = runtime
        .objects
        .get(*set)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "Set object",
            index: set.index(),
            generation: set.generation(),
        })?;
    if object.set_state().is_none() {
        return Err(NativeFailure::Abrupt(pending_set_type_error(
            realm,
            origin.clone(),
            "not a Set object",
        )?));
    }
    Ok(*set)
}

fn set_state(runtime: &Runtime, set: ObjectId) -> Result<&SetState, NativeFailure> {
    runtime
        .objects
        .get(set)
        .and_then(crate::object::HeapObject::set_state)
        .ok_or_else(|| {
            EngineFault::StaleHeapEdge {
                edge: "Set object",
                index: set.index(),
                generation: set.generation(),
            }
            .into()
        })
}

fn set_state_mut(runtime: &mut Runtime, set: ObjectId) -> Result<&mut SetState, NativeFailure> {
    runtime
        .objects
        .get_mut(set)
        .and_then(crate::object::HeapObject::set_state_mut)
        .ok_or_else(|| {
            EngineFault::StaleHeapEdge {
                edge: "Set object",
                index: set.index(),
                generation: set.generation(),
            }
            .into()
        })
}

fn set_values<const N: usize>(values: [StoredValue; N]) -> Result<Vec<StoredValue>, NativeFailure> {
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

fn set_size_number(length: usize) -> JsNumber {
    let length = usize_to_u64(length);
    let high = u32::try_from(length >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(length & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    JsNumber::from_f64(f64::from(high) * 4_294_967_296.0 + f64::from(low))
}

fn set_type_error(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(pending_set_type_error(
        realm, origin, message,
    )?))
}

fn set_range_error(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::RangeError,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}

fn pending_set_type_error(
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
