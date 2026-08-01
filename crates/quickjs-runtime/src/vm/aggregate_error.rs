/*
 * JavaScript AggregateError iterator collection derived from QuickJS.
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

//! Resumable `AggregateError` iterable collection into a fresh realm-owned Array.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[derive(Clone, Copy)]
#[allow(
    clippy::enum_variant_names,
    reason = "the Await prefix makes every resumable iterator boundary explicit"
)]
pub(super) enum AggregateErrorStage {
    AwaitIteratorMethod,
    AwaitIterator,
    AwaitNextMethod,
    AwaitNextResult,
    AwaitDone,
    AwaitValue,
}

pub(super) struct AggregateErrorContinuation {
    error: ObjectId,
    iterable: StoredValue,
    iterator: Option<StoredValue>,
    next: Option<StoredValue>,
    array: Option<ObjectId>,
    result: Option<StoredValue>,
    stack: ErrorStackSnapshot,
    next_index: u32,
    realm: RealmId,
    stage: AggregateErrorStage,
    origin: JsStackFrame,
}

impl AggregateErrorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.iterator.is_some()))
            .saturating_add(u64::from(self.next.is_some()))
            .saturating_add(u64::from(self.array.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
            .saturating_add(self.stack.retained_values())
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.error)));
        trace_stored_value_root(&self.iterable, mark);
        if let Some(iterator) = &self.iterator {
            trace_stored_value_root(iterator, mark);
        }
        if let Some(next) = &self.next {
            trace_stored_value_root(next, mark);
        }
        if let Some(array) = self.array {
            mark(CollectionRoot::Heap(HeapReference::Object(array)));
        }
        if let Some(result) = &self.result {
            trace_stored_value_root(result, mark);
        }
        self.stack.trace_roots(mark);
    }

    const fn next_acquired(&self) -> bool {
        self.next.is_some()
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "AggregateError collection retains its result object, iterable, realm, provenance, caller continuation, and execution authority"
)]
pub(super) fn begin_aggregate_error_collection(
    runtime: &mut Runtime,
    error: ObjectId,
    iterable: StoredValue,
    stack: ErrorStackSnapshot,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = AggregateErrorContinuation {
        error,
        iterable,
        iterator: None,
        next: None,
        array: None,
        result: None,
        stack,
        next_index: 0,
        realm,
        stage: AggregateErrorStage::AwaitIteratorMethod,
        origin,
    };
    read_aggregate_property(
        runtime,
        state,
        runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "the ordered AggregateError iterator protocol is one explicit resumable state machine"
)]
pub(super) fn advance_aggregate_error_collection(
    runtime: &mut Runtime,
    mut state: AggregateErrorContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        AggregateErrorStage::AwaitIteratorMethod => {
            let StoredValue::Function(method) = completion else {
                return abrupt_type_error(&state, "value is not iterable");
            };
            let receiver = state.iterable.duplicate();
            state.stage = AggregateErrorStage::AwaitIterator;
            call_aggregate_function(method, receiver, state, return_to)
        }
        AggregateErrorStage::AwaitIterator => {
            if completion.heap_reference().is_none() {
                return abrupt_type_error(&state, "not an object");
            }
            state.iterator = Some(completion);
            state.stage = AggregateErrorStage::AwaitNextMethod;
            read_aggregate_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        AggregateErrorStage::AwaitNextMethod => {
            state.next = Some(completion);
            state.array = Some(runtime.allocate_array(state.realm, Vec::new())?);
            call_aggregate_next(runtime, state, return_to, execution_budget)
        }
        AggregateErrorStage::AwaitNextResult => {
            if completion.heap_reference().is_none() {
                return close_with_type_error(
                    runtime,
                    state,
                    "iterator must return an object",
                    return_to,
                    execution_budget,
                );
            }
            state.result = Some(completion);
            state.stage = AggregateErrorStage::AwaitDone;
            read_aggregate_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        AggregateErrorStage::AwaitDone => {
            if completion.is_truthy() {
                return finish_aggregate_error(runtime, &state);
            }
            state.stage = AggregateErrorStage::AwaitValue;
            read_aggregate_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        AggregateErrorStage::AwaitValue => {
            match append_aggregate_value(runtime, &mut state, completion, execution_budget) {
                Ok(()) => {}
                Err(
                    NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending),
                ) => {
                    return begin_aggregate_error_close(
                        runtime,
                        state,
                        pending,
                        return_to,
                        execution_budget,
                    );
                }
                Err(NativeFailure::Execution(error)) => {
                    return Err(NativeFailure::Execution(error));
                }
            }
            state.result = None;
            call_aggregate_next(runtime, state, return_to, execution_budget)
        }
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "property-key ownership remains local to one resumable Get boundary"
)]
fn read_aggregate_property(
    runtime: &mut Runtime,
    state: AggregateErrorContinuation,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (base, property_name) = match state.stage {
        AggregateErrorStage::AwaitIteratorMethod => (&state.iterable, "Symbol.iterator"),
        AggregateErrorStage::AwaitNextMethod => (
            state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "AggregateError next lookup has no iterator",
                })?,
            "next",
        ),
        AggregateErrorStage::AwaitDone => (
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "AggregateError done lookup has no iterator result",
            })?,
            "done",
        ),
        AggregateErrorStage::AwaitValue => (
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "AggregateError value lookup has no iterator result",
            })?,
            "value",
        ),
        AggregateErrorStage::AwaitIterator | AggregateErrorStage::AwaitNextResult => {
            return Err(EngineFault::RuntimeInvariant {
                message: "AggregateError call stage attempted a property read",
            }
            .into());
        }
    };
    charge_iterator_property_lookup(runtime, base, execution_budget)?;
    match read_static_property(runtime, state.realm, base, &key)? {
        PropertyReadOutcome::Value(value) => {
            advance_aggregate_error_collection(runtime, state, value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            call_aggregate_function(function, receiver, state, return_to)
        }
        PropertyReadOutcome::Failed(failure) => {
            let name = JsString::from_utf8(property_name)?;
            let pending =
                property_exception_at(state.realm, state.origin.clone(), Some(&name), failure)?;
            if state.next_acquired() {
                begin_aggregate_error_close(runtime, state, pending, return_to, execution_budget)
            } else {
                Err(NativeFailure::Abrupt(pending))
            }
        }
    }
}

fn call_aggregate_next(
    runtime: &mut Runtime,
    mut state: AggregateErrorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let next = state.next.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "AggregateError iterator advance has no retained next method",
    })?;
    let StoredValue::Function(next) = next else {
        return close_with_type_error(
            runtime,
            state,
            "not a function",
            return_to,
            execution_budget,
        );
    };
    execution_budget.charge_instructions(1)?;
    let receiver = state
        .iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "AggregateError iterator advance has no retained iterator",
        })?
        .duplicate();
    state.stage = AggregateErrorStage::AwaitNextResult;
    call_aggregate_function(*next, receiver, state, return_to)
}

fn call_aggregate_function(
    function: FunctionId,
    receiver: StoredValue,
    state: AggregateErrorContinuation,
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
    continuations.push(NativeContinuation::AggregateError(state));
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

fn append_aggregate_value(
    runtime: &mut Runtime,
    state: &mut AggregateErrorContinuation,
    value: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    let Some(index) = ArrayIndex::new(state.next_index) else {
        return Err(NativeFailure::Abrupt(pending_exception(
            state.realm,
            state.origin.clone(),
            ExceptionKind::RangeError,
            "invalid array length",
        )?));
    };
    let array = state.array.ok_or(EngineFault::RuntimeInvariant {
        message: "AggregateError append has no result array",
    })?;
    let work = runtime.preview_array_define_data_property_work(array)?;
    execution_budget.charge_instructions(work)?;
    match runtime.define_array_data_property(
        array,
        PropertyKey::from_index(index),
        PropertyLayout::data(true, true, true),
        value,
    )? {
        ArrayDefineOutcome::Complete => {}
        ArrayDefineOutcome::ReadOnlyLength | ArrayDefineOutcome::NonExtensible => {
            return Err(NativeFailure::Abrupt(pending_exception(
                state.realm,
                state.origin.clone(),
                ExceptionKind::TypeError,
                "cannot append iterator value",
            )?));
        }
    }
    state.next_index = state.next_index.saturating_add(1);
    Ok(())
}

fn finish_aggregate_error(
    runtime: &mut Runtime,
    state: &AggregateErrorContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    let array = state.array.ok_or(EngineFault::RuntimeInvariant {
        message: "AggregateError completed without a result array",
    })?;
    runtime.define_error_data_property(
        state.error,
        PredefinedAtom::Errors,
        StoredValue::Object(array),
    )?;
    let stack = render_error_stack(runtime, &state.stack)?;
    runtime.define_error_data_property(
        state.error,
        PredefinedAtom::Stack,
        StoredValue::String(stack),
    )?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(state.error)))
}

fn close_with_type_error(
    runtime: &mut Runtime,
    state: AggregateErrorContinuation,
    message: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let pending = pending_exception(
        state.realm,
        state.origin.clone(),
        ExceptionKind::TypeError,
        message,
    )?;
    begin_aggregate_error_close(runtime, state, pending, return_to, execution_budget)
}

fn abrupt_type_error(
    state: &AggregateErrorContinuation,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    abrupt_type_error_with_kind(
        state.realm,
        state.origin.clone(),
        ExceptionKind::TypeError,
        message,
    )
}

fn abrupt_type_error_with_kind(
    realm: RealmId,
    origin: JsStackFrame,
    kind: ExceptionKind,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(pending_exception(
        realm, origin, kind, message,
    )?))
}

fn pending_exception(
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

fn begin_aggregate_error_close(
    runtime: &mut Runtime,
    state: AggregateErrorContinuation,
    original: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let iterator = state.iterator.ok_or(EngineFault::RuntimeInvariant {
        message: "AggregateError IteratorClose started before iterator acquisition",
    })?;
    begin_exceptional_iterator_close(runtime, iterator, original, return_to, execution_budget)
}

pub(super) fn resume_aggregate_error_abrupt(
    runtime: &mut Runtime,
    state: AggregateErrorContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.next_acquired() {
        begin_aggregate_error_close(runtime, state, pending, return_to, execution_budget)
    } else {
        Err(NativeFailure::Abrupt(pending))
    }
}
