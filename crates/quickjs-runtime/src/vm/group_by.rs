/*
 * JavaScript Object.groupBy semantics derived from QuickJS.
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

//! Resumable ECMA-262 `GroupBy` for `Object.groupBy` and `Map.groupBy`.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::enum_variant_names,
    reason = "the Await prefix makes every observable GroupBy boundary explicit"
)]
pub(super) enum GroupByStage {
    AwaitIteratorMethod,
    AwaitIterator,
    AwaitNextMethod,
    AwaitNextResult,
    AwaitDone,
    AwaitIteratorValue,
    AwaitCallback,
    AwaitPropertyKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GroupByKind {
    Property,
    Collection,
}

enum GroupKey {
    Property(PropertyKey),
    Collection(StoredValue),
}

struct ValueGroup {
    key: GroupKey,
    elements: Vec<StoredValue>,
}

/// One suspended `Object.groupBy` traversal and its not-yet-materialized groups.
pub(super) struct GroupByContinuation {
    items: StoredValue,
    callback: FunctionId,
    iterator: Option<StoredValue>,
    next: Option<StoredValue>,
    result: Option<StoredValue>,
    value: Option<StoredValue>,
    groups: Vec<ValueGroup>,
    kind: GroupByKind,
    index: u64,
    realm: RealmId,
    stage: GroupByStage,
    origin: JsStackFrame,
}

impl GroupByContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        let grouped = self.groups.iter().fold(0_u64, |count, group| {
            count
                .saturating_add(usize_to_u64(group.elements.len()))
                .saturating_add(u64::from(matches!(group.key, GroupKey::Collection(_))))
        });
        2_u64
            .saturating_add(u64::from(self.iterator.is_some()))
            .saturating_add(u64::from(self.next.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
            .saturating_add(u64::from(self.value.is_some()))
            .saturating_add(grouped)
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.items, mark);
        mark(CollectionRoot::Heap(HeapReference::Function(self.callback)));
        for value in [
            self.iterator.as_ref(),
            self.next.as_ref(),
            self.result.as_ref(),
            self.value.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            trace_stored_value_root(value, mark);
        }
        for group in &self.groups {
            if let GroupKey::Collection(key) = &group.key {
                trace_stored_value_root(key, mark);
            }
            for value in &group.elements {
                trace_stored_value_root(value, mark);
            }
        }
    }

    const fn closes_on_abrupt(&self) -> bool {
        matches!(
            self.stage,
            GroupByStage::AwaitCallback | GroupByStage::AwaitPropertyKey
        )
    }
}

/// Validates the ECMA-262 arguments before acquiring the items iterator.
pub(super) fn begin_group_by(
    runtime: &mut Runtime,
    items: StoredValue,
    callback: &StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_group_by_kind(
        runtime,
        items,
        callback,
        GroupByKind::Property,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn begin_map_group_by(
    runtime: &mut Runtime,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let items = arguments.take_first_or_undefined();
    let callback = arguments.take_first_or_undefined();
    begin_group_by_kind(
        runtime,
        items,
        &callback,
        GroupByKind::Collection,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the shared GroupBy entry retains its key-coercion kind and ordinary native-call authority"
)]
fn begin_group_by_kind(
    runtime: &mut Runtime,
    items: StoredValue,
    callback: &StoredValue,
    kind: GroupByKind,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(items, StoredValue::Undefined | StoredValue::Null) {
        return abrupt_group_by_type_error(realm, origin, "cannot convert to object");
    }
    let StoredValue::Function(callback) = callback else {
        return abrupt_group_by_type_error(realm, origin, "not a function");
    };
    let state = GroupByContinuation {
        items,
        callback: *callback,
        iterator: None,
        next: None,
        result: None,
        value: None,
        groups: Vec::new(),
        kind,
        index: 0,
        realm,
        stage: GroupByStage::AwaitIteratorMethod,
        origin,
    };
    read_group_by_property(
        runtime,
        state,
        &runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        return_to,
        execution_budget,
    )
}

/// Advances one observable iterator, callback, or property-key boundary.
#[allow(
    clippy::too_many_lines,
    reason = "one explicit match keeps GroupBy iterator, callback, conversion, and close boundaries auditable in specification order"
)]
pub(super) fn advance_group_by(
    runtime: &mut Runtime,
    mut state: GroupByContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        GroupByStage::AwaitIteratorMethod => {
            let StoredValue::Function(method) = completion else {
                return abrupt_group_by_type_error(
                    state.realm,
                    state.origin,
                    "value is not iterable",
                );
            };
            let receiver = state.items.duplicate();
            state.stage = GroupByStage::AwaitIterator;
            call_group_by_function(method, receiver, Vec::new(), state, return_to)
        }
        GroupByStage::AwaitIterator => {
            if completion.heap_reference().is_none() {
                return abrupt_group_by_type_error(state.realm, state.origin, "not an object");
            }
            state.iterator = Some(completion);
            state.stage = GroupByStage::AwaitNextMethod;
            read_group_by_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        GroupByStage::AwaitNextMethod => {
            state.next = Some(completion);
            call_group_by_next(runtime, state, return_to, execution_budget)
        }
        GroupByStage::AwaitNextResult => {
            if completion.heap_reference().is_none() {
                return abrupt_group_by_type_error(
                    state.realm,
                    state.origin,
                    "iterator must return an object",
                );
            }
            state.result = Some(completion);
            state.stage = GroupByStage::AwaitDone;
            read_group_by_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        GroupByStage::AwaitDone => {
            if runtime.to_boolean(&completion)? {
                return finish_group_by(runtime, state, execution_budget);
            }
            state.stage = GroupByStage::AwaitIteratorValue;
            read_group_by_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        GroupByStage::AwaitIteratorValue => {
            state.value = Some(completion);
            call_group_by_callback(state, return_to)
        }
        GroupByStage::AwaitCallback => {
            if state.kind == GroupByKind::Collection {
                let key = canonicalize_collection_key(completion);
                let value = state.value.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Map.groupBy callback completed without an iterator value",
                })?;
                add_value_to_group(&mut state.groups, GroupKey::Collection(key), value)?;
                state.result = None;
                state.index = state.index.saturating_add(1);
                return call_group_by_next(runtime, state, return_to, execution_budget);
            }
            state.stage = GroupByStage::AwaitPropertyKey;
            let conversion = begin_property_key_conversion(
                runtime,
                completion,
                PropertyKeyTarget::ToKey,
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            );
            match conversion {
                Ok(dispatch) => {
                    attach_group_by_after_key(runtime, dispatch, state, return_to, execution_budget)
                }
                Err(
                    NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending),
                ) => begin_group_by_close(runtime, state, pending, return_to, execution_budget),
                Err(NativeFailure::Execution(error)) => Err(NativeFailure::Execution(error)),
            }
        }
        GroupByStage::AwaitPropertyKey => {
            let property = computed_property_operand(runtime, &completion)?;
            let value = state.value.take().ok_or(EngineFault::RuntimeInvariant {
                message: "groupBy completed ToPropertyKey without an iterator value",
            })?;
            add_value_to_group(&mut state.groups, GroupKey::Property(property.key), value)?;
            state.result = None;
            state.index = state.index.saturating_add(1);
            call_group_by_next(runtime, state, return_to, execution_budget)
        }
    }
}

fn read_group_by_property(
    runtime: &mut Runtime,
    state: GroupByContinuation,
    key: &PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (base, property_name) = match state.stage {
        GroupByStage::AwaitIteratorMethod => (&state.items, "Symbol.iterator"),
        GroupByStage::AwaitNextMethod => (
            state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "groupBy next lookup has no iterator",
                })?,
            "next",
        ),
        GroupByStage::AwaitDone => (
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "groupBy done lookup has no iterator result",
            })?,
            "done",
        ),
        GroupByStage::AwaitIteratorValue => (
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "groupBy value lookup has no iterator result",
            })?,
            "value",
        ),
        GroupByStage::AwaitIterator
        | GroupByStage::AwaitNextResult
        | GroupByStage::AwaitCallback
        | GroupByStage::AwaitPropertyKey => {
            return Err(EngineFault::RuntimeInvariant {
                message: "groupBy call stage attempted a property read",
            }
            .into());
        }
    };
    let base = base.duplicate();
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
        group_by_native_continuation,
        |state, value| advance_group_by(runtime, state, value, return_to, execution_budget),
        "groupBy Get produced a structured result",
    )
}

fn group_by_native_continuation(state: GroupByContinuation) -> NativeContinuation {
    NativeContinuation::GroupBy(Box::new(state))
}

fn call_group_by_next(
    runtime: &mut Runtime,
    mut state: GroupByContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.index >= MAX_SAFE_INTEGER {
        return close_group_by_with_type_error(
            runtime,
            state,
            "too many items",
            return_to,
            execution_budget,
        );
    }
    let next = state.next.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "groupBy iterator advance has no retained next method",
    })?;
    let StoredValue::Function(next) = next else {
        return abrupt_group_by_type_error(state.realm, state.origin, "not a function");
    };
    execution_budget.charge_instructions(1)?;
    let receiver = state
        .iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "groupBy iterator advance has no retained iterator",
        })?
        .duplicate();
    state.stage = GroupByStage::AwaitNextResult;
    call_group_by_function(*next, receiver, Vec::new(), state, return_to)
}

fn call_group_by_callback(
    mut state: GroupByContinuation,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let value = state
        .value
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "groupBy callback has no retained iterator value",
        })?
        .duplicate();
    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(2)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 2,
        })?;
    arguments.push(value);
    arguments.push(StoredValue::Number(JsNumber::from_f64(group_index_as_f64(
        state.index,
    ))));
    let callback = state.callback;
    state.stage = GroupByStage::AwaitCallback;
    call_group_by_function(
        callback,
        StoredValue::Undefined,
        arguments,
        state,
        return_to,
    )
}

#[expect(
    clippy::cast_precision_loss,
    reason = "GroupBy rejects index 2^53 - 1 before conversion, so every callback index is exactly representable in binary64"
)]
fn group_index_as_f64(index: u64) -> f64 {
    index as f64
}

fn call_group_by_function(
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
    state: GroupByContinuation,
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
    continuations.push(NativeContinuation::GroupBy(Box::new(state)));
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

fn attach_group_by_after_key(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: GroupByContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(key) => {
            advance_group_by(runtime, state, key, return_to, execution_budget)
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(&mut frame, group_by_continuation(state)?)?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(&mut call, group_by_continuation(state)?)?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "groupBy key conversion produced a structured result",
        }
        .into()),
    }
}

fn group_by_continuation(
    state: GroupByContinuation,
) -> Result<Vec<NativeContinuation>, NativeFailure> {
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::GroupBy(Box::new(state)));
    Ok(continuations)
}

fn add_value_to_group(
    groups: &mut Vec<ValueGroup>,
    key: GroupKey,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    if let Some(group) = groups
        .iter_mut()
        .find(|group| group_keys_equal(&group.key, &key))
    {
        group
            .elements
            .try_reserve(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::FrameValues,
                additional: 1,
            })?;
        group.elements.push(value);
        return Ok(());
    }
    let mut elements = Vec::new();
    elements
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 1,
        })?;
    elements.push(value);
    groups
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 1,
        })?;
    groups.push(ValueGroup { key, elements });
    Ok(())
}

fn group_keys_equal(left: &GroupKey, right: &GroupKey) -> bool {
    match (left, right) {
        (GroupKey::Property(left), GroupKey::Property(right)) => left == right,
        (GroupKey::Collection(left), GroupKey::Collection(right)) => left.same_value_zero(right),
        (GroupKey::Property(_), GroupKey::Collection(_))
        | (GroupKey::Collection(_), GroupKey::Property(_)) => false,
    }
}

fn canonicalize_collection_key(key: StoredValue) -> StoredValue {
    match key {
        StoredValue::Number(value) if value.as_f64() == 0.0 => {
            StoredValue::Number(JsNumber::from_f64(0.0))
        }
        key => key,
    }
}

fn finish_group_by(
    runtime: &mut Runtime,
    state: GroupByContinuation,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.kind {
        GroupByKind::Property => {
            let target = runtime.allocate_ordinary_object_with_optional_prototype(None)?;
            for group in state.groups {
                let GroupKey::Property(key) = group.key else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "Object.groupBy retained a collection key",
                    }
                    .into());
                };
                let elements = runtime.allocate_array(state.realm, group.elements)?;
                match define_static_property(
                    runtime,
                    &StoredValue::Object(target),
                    key,
                    StoredValue::Object(elements),
                    execution_budget,
                )? {
                    PropertyWriteOutcome::Complete => {}
                    PropertyWriteOutcome::Setter { .. } | PropertyWriteOutcome::Failed(_) => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "groupBy fresh result refused a data property",
                        }
                        .into());
                    }
                }
            }
            Ok(NativeDispatch::Immediate(StoredValue::Object(target)))
        }
        GroupByKind::Collection => {
            let prototype = runtime.realm_map_prototype(state.realm)?;
            let target = runtime.allocate_map_object(HeapReference::Object(prototype))?;
            for group in state.groups {
                let GroupKey::Collection(key) = group.key else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "Map.groupBy retained a property key",
                    }
                    .into());
                };
                let elements = runtime.allocate_array(state.realm, group.elements)?;
                runtime.map_set(target, key, StoredValue::Object(elements))?;
            }
            Ok(NativeDispatch::Immediate(StoredValue::Object(target)))
        }
    }
}

fn abrupt_group_by_type_error(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(group_by_exception(
        realm,
        origin,
        ExceptionKind::TypeError,
        message,
    )?))
}

fn close_group_by_with_type_error(
    runtime: &mut Runtime,
    state: GroupByContinuation,
    message: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let pending = group_by_exception(
        state.realm,
        state.origin.clone(),
        ExceptionKind::TypeError,
        message,
    )?;
    begin_group_by_close(runtime, state, pending, return_to, execution_budget)
}

fn group_by_exception(
    realm: RealmId,
    origin: JsStackFrame,
    kind: ExceptionKind,
    message: &str,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin,
    })
}

fn begin_group_by_close(
    runtime: &mut Runtime,
    state: GroupByContinuation,
    original: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let iterator = state.iterator.ok_or(EngineFault::RuntimeInvariant {
        message: "groupBy IteratorClose started before iterator acquisition",
    })?;
    begin_exceptional_iterator_close(runtime, iterator, original, return_to, execution_budget)
}

pub(super) fn resume_group_by_abrupt(
    runtime: &mut Runtime,
    state: GroupByContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.closes_on_abrupt() {
        begin_group_by_close(runtime, state, pending, return_to, execution_budget)
    } else {
        Err(NativeFailure::Abrupt(pending))
    }
}
