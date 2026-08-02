/*
 * JavaScript Object.fromEntries semantics derived from QuickJS.
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

//! Resumable `AddEntriesFromIterable` for `Object.fromEntries`.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::enum_variant_names,
    reason = "the Await prefix makes every observable AddEntriesFromIterable boundary explicit"
)]
pub(super) enum FromEntriesStage {
    AwaitIteratorMethod,
    AwaitIterator,
    AwaitNextMethod,
    AwaitNextResult,
    AwaitDone,
    AwaitIteratorValue,
    AwaitEntryKey,
    AwaitEntryValue,
    AwaitPropertyKey,
}

/// One suspended `Object.fromEntries` iterator traversal.
pub(super) struct FromEntriesContinuation {
    target: ObjectId,
    iterable: StoredValue,
    iterator: Option<StoredValue>,
    next: Option<StoredValue>,
    result: Option<StoredValue>,
    entry: Option<StoredValue>,
    key: Option<StoredValue>,
    value: Option<StoredValue>,
    realm: RealmId,
    stage: FromEntriesStage,
    origin: JsStackFrame,
}

impl FromEntriesContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
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
        for value in [
            self.iterator.as_ref(),
            self.next.as_ref(),
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

    const fn next_acquired(&self) -> bool {
        self.next.is_some()
    }
}

/// Allocates the result before acquiring the iterator, as required by
/// `Object.fromEntries`.
pub(super) fn begin_from_entries(
    runtime: &mut Runtime,
    iterable: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    let state = FromEntriesContinuation {
        target,
        iterable,
        iterator: None,
        next: None,
        result: None,
        entry: None,
        key: None,
        value: None,
        realm,
        stage: FromEntriesStage::AwaitIteratorMethod,
        origin,
    };
    read_from_entries_property(
        runtime,
        state,
        &runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        return_to,
        execution_budget,
    )
}

/// Advances one observable iterator, entry-read, or property-key boundary.
#[allow(
    clippy::too_many_lines,
    reason = "one explicit match keeps iterator acquisition, entry reads, key conversion, and close boundaries auditable in specification order"
)]
pub(super) fn advance_from_entries(
    runtime: &mut Runtime,
    mut state: FromEntriesContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        FromEntriesStage::AwaitIteratorMethod => {
            let StoredValue::Function(method) = completion else {
                return abrupt_from_entries_type_error(&state, "value is not iterable");
            };
            let receiver = state.iterable.duplicate();
            state.stage = FromEntriesStage::AwaitIterator;
            call_from_entries_function(method, receiver, state, return_to)
        }
        FromEntriesStage::AwaitIterator => {
            if completion.heap_reference().is_none() {
                return abrupt_from_entries_type_error(&state, "not an object");
            }
            state.iterator = Some(completion);
            state.stage = FromEntriesStage::AwaitNextMethod;
            read_from_entries_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        FromEntriesStage::AwaitNextMethod => {
            state.next = Some(completion);
            call_from_entries_next(runtime, state, return_to, execution_budget)
        }
        FromEntriesStage::AwaitNextResult => {
            if completion.heap_reference().is_none() {
                return close_from_entries_with_type_error(
                    runtime,
                    state,
                    "iterator must return an object",
                    return_to,
                    execution_budget,
                );
            }
            state.result = Some(completion);
            state.stage = FromEntriesStage::AwaitDone;
            read_from_entries_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        FromEntriesStage::AwaitDone => {
            if completion.is_truthy() {
                return Ok(NativeDispatch::Immediate(StoredValue::Object(state.target)));
            }
            state.stage = FromEntriesStage::AwaitIteratorValue;
            read_from_entries_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        FromEntriesStage::AwaitIteratorValue => {
            if completion.heap_reference().is_none() {
                return close_from_entries_with_type_error(
                    runtime,
                    state,
                    "not an object",
                    return_to,
                    execution_budget,
                );
            }
            state.entry = Some(completion);
            state.stage = FromEntriesStage::AwaitEntryKey;
            read_from_entries_property(
                runtime,
                state,
                &PropertyKey::from_index(ArrayIndex::new(0).ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "entry key index zero was rejected",
                    },
                )?),
                return_to,
                execution_budget,
            )
        }
        FromEntriesStage::AwaitEntryKey => {
            state.key = Some(completion);
            state.stage = FromEntriesStage::AwaitEntryValue;
            read_from_entries_property(
                runtime,
                state,
                &PropertyKey::from_index(ArrayIndex::new(1).ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "entry value index one was rejected",
                    },
                )?),
                return_to,
                execution_budget,
            )
        }
        FromEntriesStage::AwaitEntryValue => {
            state.value = Some(completion);
            let key = state.key.take().ok_or(EngineFault::RuntimeInvariant {
                message: "fromEntries reached ToPropertyKey without an entry key",
            })?;
            state.stage = FromEntriesStage::AwaitPropertyKey;
            let conversion = begin_property_key_conversion(
                runtime,
                key,
                PropertyKeyTarget::ToKey,
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            );
            match conversion {
                Ok(dispatch) => attach_from_entries_after_key(
                    runtime,
                    dispatch,
                    state,
                    return_to,
                    execution_budget,
                ),
                Err(
                    NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending),
                ) => begin_from_entries_close(runtime, state, pending, return_to, execution_budget),
                Err(NativeFailure::Execution(error)) => Err(NativeFailure::Execution(error)),
            }
        }
        FromEntriesStage::AwaitPropertyKey => {
            let property = computed_property_operand(runtime, &completion)?;
            let value = state.value.take().ok_or(EngineFault::RuntimeInvariant {
                message: "fromEntries completed ToPropertyKey without an entry value",
            })?;
            match define_static_property(
                runtime,
                &StoredValue::Object(state.target),
                property.key,
                value,
                execution_budget,
            )? {
                PropertyWriteOutcome::Complete => {}
                PropertyWriteOutcome::Setter { .. } => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "fromEntries fresh target unexpectedly invoked a setter",
                    }
                    .into());
                }
                PropertyWriteOutcome::Failed(failure) => {
                    let pending = property_exception_at(
                        state.realm,
                        state.origin.clone(),
                        Some(&property.name),
                        failure,
                    )?;
                    return begin_from_entries_close(
                        runtime,
                        state,
                        pending,
                        return_to,
                        execution_budget,
                    );
                }
            }
            state.result = None;
            state.entry = None;
            call_from_entries_next(runtime, state, return_to, execution_budget)
        }
    }
}

fn read_from_entries_property(
    runtime: &mut Runtime,
    state: FromEntriesContinuation,
    key: &PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (base, property_name) = match state.stage {
        FromEntriesStage::AwaitIteratorMethod => (&state.iterable, "Symbol.iterator"),
        FromEntriesStage::AwaitNextMethod => (
            state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "fromEntries next lookup has no iterator",
                })?,
            "next",
        ),
        FromEntriesStage::AwaitDone => (
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "fromEntries done lookup has no iterator result",
            })?,
            "done",
        ),
        FromEntriesStage::AwaitIteratorValue => (
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "fromEntries value lookup has no iterator result",
            })?,
            "value",
        ),
        FromEntriesStage::AwaitEntryKey => (
            state.entry.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "fromEntries key lookup has no entry object",
            })?,
            "0",
        ),
        FromEntriesStage::AwaitEntryValue => (
            state.entry.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "fromEntries value lookup has no entry object",
            })?,
            "1",
        ),
        FromEntriesStage::AwaitIterator
        | FromEntriesStage::AwaitNextResult
        | FromEntriesStage::AwaitPropertyKey => {
            return Err(EngineFault::RuntimeInvariant {
                message: "fromEntries call stage attempted a property read",
            }
            .into());
        }
    };
    charge_iterator_property_lookup(runtime, base, execution_budget)?;
    match read_static_property(runtime, state.realm, base, key)? {
        PropertyReadOutcome::Value(value) => {
            advance_from_entries(runtime, state, value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            call_from_entries_function(function, receiver, state, return_to)
        }
        PropertyReadOutcome::Failed(failure) => {
            let name = JsString::from_utf8(property_name)?;
            let pending =
                property_exception_at(state.realm, state.origin.clone(), Some(&name), failure)?;
            if state.next_acquired() {
                begin_from_entries_close(runtime, state, pending, return_to, execution_budget)
            } else {
                Err(NativeFailure::Abrupt(pending))
            }
        }
    }
}

fn call_from_entries_next(
    runtime: &mut Runtime,
    mut state: FromEntriesContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let next = state.next.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "fromEntries iterator advance has no retained next method",
    })?;
    let StoredValue::Function(next) = next else {
        return close_from_entries_with_type_error(
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
            message: "fromEntries iterator advance has no retained iterator",
        })?
        .duplicate();
    state.stage = FromEntriesStage::AwaitNextResult;
    call_from_entries_function(*next, receiver, state, return_to)
}

fn call_from_entries_function(
    function: FunctionId,
    receiver: StoredValue,
    state: FromEntriesContinuation,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    iterator_getter_call(
        function,
        receiver,
        NativeContinuation::FromEntries(Box::new(state)),
        return_to,
        origin,
        None,
    )
}

fn attach_from_entries_after_key(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: FromEntriesContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(key) => {
            advance_from_entries(runtime, state, key, return_to, execution_budget)
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(&mut frame, from_entries_continuation(state)?)?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(&mut call, from_entries_continuation(state)?)?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone => Err(EngineFault::RuntimeInvariant {
            message: "fromEntries key conversion produced a structured result",
        }
        .into()),
    }
}

fn from_entries_continuation(
    state: FromEntriesContinuation,
) -> Result<Vec<NativeContinuation>, NativeFailure> {
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::FromEntries(Box::new(state)));
    Ok(continuations)
}

fn close_from_entries_with_type_error(
    runtime: &mut Runtime,
    state: FromEntriesContinuation,
    message: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let pending = from_entries_exception(
        state.realm,
        state.origin.clone(),
        ExceptionKind::TypeError,
        message,
    )?;
    begin_from_entries_close(runtime, state, pending, return_to, execution_budget)
}

fn abrupt_from_entries_type_error(
    state: &FromEntriesContinuation,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(from_entries_exception(
        state.realm,
        state.origin.clone(),
        ExceptionKind::TypeError,
        message,
    )?))
}

fn from_entries_exception(
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

fn begin_from_entries_close(
    runtime: &mut Runtime,
    state: FromEntriesContinuation,
    original: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let iterator = state.iterator.ok_or(EngineFault::RuntimeInvariant {
        message: "fromEntries IteratorClose started before iterator acquisition",
    })?;
    begin_exceptional_iterator_close(runtime, iterator, original, return_to, execution_budget)
}

pub(super) fn resume_from_entries_abrupt(
    runtime: &mut Runtime,
    state: FromEntriesContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.next_acquired() {
        begin_from_entries_close(runtime, state, pending, return_to, execution_budget)
    } else {
        Err(NativeFailure::Abrupt(pending))
    }
}
