/*
 * JavaScript for-in enumeration semantics derived from QuickJS.
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
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! Resumable `EnumerateObjectProperties` with Proxy internal methods.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForInStage {
    StartKeys,
    Descriptor,
    Prototype,
    PrototypeKeys,
}

pub(super) struct ForInContinuation {
    iterator: ObjectId,
    current: HeapReference,
    pending_key: Option<PropertyKey>,
    realm: RealmId,
    origin: JsStackFrame,
    stage: ForInStage,
}

impl ForInContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64.saturating_add(u64::from(self.pending_key.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.iterator)));
        mark(CollectionRoot::Heap(self.current));
    }
}

enum ForInDispatch {
    Resume(ForInContinuation, StoredValue),
    Suspend(Box<NativeDispatch>),
}

fn continue_for_in_after(
    dispatch: NativeDispatch,
    state: ForInContinuation,
) -> Result<ForInDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => Ok(ForInDispatch::Resume(state, value)),
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::ForIn(Box::new(state))],
            )?;
            Ok(ForInDispatch::Suspend(Box::new(NativeDispatch::Call(call))))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::ForIn(Box::new(state))],
            )?;
            Ok(ForInDispatch::Suspend(Box::new(NativeDispatch::Frame(
                frame,
            ))))
        }
        dispatch @ (NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. }) => Ok(ForInDispatch::Suspend(Box::new(dispatch))),
    }
}

fn string_keys(keys: Vec<PropertyKey>) -> Result<Vec<PropertyKey>, NativeFailure> {
    let mut strings = Vec::new();
    strings
        .try_reserve_exact(keys.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ForInEntries,
            additional: keys.len(),
        })?;
    strings.extend(keys.into_iter().filter(|key| {
        key.as_index().is_some()
            || key
                .as_atom()
                .is_some_and(|atom| atom.kind() == crate::AtomKind::String)
    }));
    Ok(strings)
}

fn prepare_for_in_next(
    runtime: &mut Runtime,
    iterator: ObjectId,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<ForInDispatch, NativeFailure> {
    loop {
        let Some(current) = runtime.for_in_cursor_current(iterator)? else {
            return Ok(ForInDispatch::Suspend(Box::new(NativeDispatch::Pair(
                StoredValue::Undefined,
                StoredValue::Boolean(true),
            ))));
        };
        if let Some(key) = runtime.for_in_cursor_candidate(iterator)? {
            execution_budget.charge_instructions(1)?;
            if runtime.for_in_cursor_has_visited(iterator, &key)? {
                runtime.advance_for_in_cursor_candidate(iterator)?;
                continue;
            }
            runtime.visit_for_in_cursor_candidate(iterator, key.clone())?;
            let state = ForInContinuation {
                iterator,
                current,
                pending_key: Some(key.clone()),
                realm,
                origin: origin.clone(),
                stage: ForInStage::Descriptor,
            };
            let dispatch = begin_internal_get_own_property(
                runtime,
                current,
                key,
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )?;
            return continue_for_in_after(dispatch, state);
        }
        execution_budget.charge_instructions(1)?;
        let state = ForInContinuation {
            iterator,
            current,
            pending_key: None,
            realm,
            origin: origin.clone(),
            stage: ForInStage::Prototype,
        };
        let dispatch = begin_internal_get_prototype_of(
            runtime,
            current,
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        )?;
        return continue_for_in_after(dispatch, state);
    }
}

pub(super) fn begin_for_in_start(
    runtime: &mut Runtime,
    value: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let proxy = match value.heap_reference() {
        Some(reference) => runtime.proxy_state(reference)?.is_some(),
        None => false,
    };
    if !proxy {
        let work = runtime.preview_for_in_iterator_work(&value)?;
        execution_budget.charge_instructions(work)?;
    }
    let iterator = runtime.allocate_for_in_cursor(realm, value)?;
    let Some(current) = runtime.for_in_cursor_current(iterator)? else {
        return Ok(NativeDispatch::Immediate(StoredValue::Object(iterator)));
    };
    execution_budget.charge_instructions(1)?;
    let state = ForInContinuation {
        iterator,
        current,
        pending_key: None,
        realm,
        origin: origin.clone(),
        stage: ForInStage::StartKeys,
    };
    let dispatch =
        begin_internal_own_keys(runtime, current, realm, return_to, origin, execution_budget)?;
    match continue_for_in_after(dispatch, state)? {
        ForInDispatch::Resume(state, completion) => {
            advance_for_in(runtime, state, completion, return_to, execution_budget)
        }
        ForInDispatch::Suspend(dispatch) => Ok(*dispatch),
    }
}

pub(super) fn begin_for_in_next(
    runtime: &mut Runtime,
    iterator: ObjectId,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match prepare_for_in_next(
        runtime,
        iterator,
        realm,
        return_to,
        origin,
        execution_budget,
    )? {
        ForInDispatch::Resume(state, completion) => {
            advance_for_in(runtime, state, completion, return_to, execution_budget)
        }
        ForInDispatch::Suspend(dispatch) => Ok(*dispatch),
    }
}

pub(super) fn advance_for_in(
    runtime: &mut Runtime,
    mut state: ForInContinuation,
    mut completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    loop {
        let next = match state.stage {
            ForInStage::StartKeys => {
                let keys = string_keys(generated_key_list(runtime, completion)?)?;
                runtime.replace_for_in_cursor_keys(state.iterator, state.current, keys)?;
                return Ok(NativeDispatch::Immediate(StoredValue::Object(
                    state.iterator,
                )));
            }
            ForInStage::Descriptor => {
                let property = internal_complete_own_property(runtime, &completion)?;
                let key = state
                    .pending_key
                    .take()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "for-in descriptor check lost its key",
                    })?;
                if property.is_some_and(|property| property.layout().is_enumerable()) {
                    return Ok(NativeDispatch::Pair(
                        for_in_key_value(&key)?,
                        StoredValue::Boolean(false),
                    ));
                }
                prepare_for_in_next(
                    runtime,
                    state.iterator,
                    state.realm,
                    return_to,
                    &state.origin,
                    execution_budget,
                )?
            }
            ForInStage::Prototype => {
                let Some(prototype) = completion.heap_reference() else {
                    if matches!(completion, StoredValue::Null) {
                        execution_budget.charge_instructions(usize_to_u64(
                            runtime.for_in_cursor_snapshot_len(state.iterator)?,
                        ))?;
                        runtime.finish_for_in_cursor(state.iterator)?;
                        return Ok(NativeDispatch::Pair(
                            StoredValue::Undefined,
                            StoredValue::Boolean(true),
                        ));
                    }
                    return Err(EngineFault::RuntimeInvariant {
                        message: "for-in [[GetPrototypeOf]] returned neither object nor null",
                    }
                    .into());
                };
                state.current = prototype;
                state.stage = ForInStage::PrototypeKeys;
                execution_budget.charge_instructions(1)?;
                let dispatch = begin_internal_own_keys(
                    runtime,
                    prototype,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                continue_for_in_after(dispatch, state)?
            }
            ForInStage::PrototypeKeys => {
                let keys = string_keys(generated_key_list(runtime, completion)?)?;
                execution_budget.charge_instructions(usize_to_u64(
                    runtime.for_in_cursor_snapshot_len(state.iterator)?,
                ))?;
                runtime.replace_for_in_cursor_keys(state.iterator, state.current, keys)?;
                prepare_for_in_next(
                    runtime,
                    state.iterator,
                    state.realm,
                    return_to,
                    &state.origin,
                    execution_budget,
                )?
            }
        };
        match next {
            ForInDispatch::Resume(next_state, next_completion) => {
                state = next_state;
                completion = next_completion;
            }
            ForInDispatch::Suspend(dispatch) => return Ok(*dispatch),
        }
    }
}
