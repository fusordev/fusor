/*
 * JavaScript Array.fromAsync semantics derived from ECMA-262.
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

//! Resumable `Array.fromAsync` evaluation.
//!
//! The state machine mirrors the two ECMA-262 paths. The iterable path awaits
//! each `next` result and mapper result and performs exceptional
//! `AsyncIteratorClose` only for mapper, size, and element-definition failures.
//! The array-like path awaits each retrieved value before mapping it and never
//! owns an iterator to close.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayFromAsyncStage {
    AsyncIteratorMethod,
    SyncIteratorMethod,
    IterableConstruct,
    IteratorMethodCall,
    NextMethod,
    NextCall,
    NextAwaitSetup,
    NextAwaitReaction,
    Done,
    Value,
    IterableMapperCall,
    IterableMapperAwaitSetup,
    IterableMapperAwaitReaction,
    IterableDefineElement,
    ArrayLikeLength,
    ArrayLikeLengthConversion,
    ArrayLikeConstruct,
    ArrayLikeElement,
    ArrayLikeElementAwaitSetup,
    ArrayLikeElementAwaitReaction,
    ArrayLikeMapperCall,
    ArrayLikeMapperAwaitSetup,
    ArrayLikeMapperAwaitReaction,
    ArrayLikeDefineElement,
    LengthSetter,
    CloseReturnMethod,
    CloseReturnCall,
    CloseAwaitSetup,
    CloseAwaitReaction,
}

/// One active or Promise-suspended `Array.fromAsync` evaluation.
pub(crate) struct ArrayFromAsyncRecord {
    constructor: StoredValue,
    source: StoredValue,
    mapper: Option<FunctionId>,
    this_arg: StoredValue,
    capability: crate::object::PromiseCapability,
    target: Option<StoredValue>,
    iterator: Option<StoredValue>,
    next: Option<StoredValue>,
    result: Option<StoredValue>,
    original_reason: Option<StoredValue>,
    awaiting: Option<ObjectId>,
    length: u64,
    index: u64,
    realm: RealmId,
    stage: ArrayFromAsyncStage,
    sync_fallback: bool,
    origin: JsStackFrame,
}

impl ArrayFromAsyncRecord {
    pub(crate) fn retained_values(&self) -> u64 {
        6_u64
            .saturating_add(u64::from(self.mapper.is_some()))
            .saturating_add(u64::from(self.target.is_some()))
            .saturating_add(u64::from(self.iterator.is_some()))
            .saturating_add(u64::from(self.next.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
            .saturating_add(u64::from(self.original_reason.is_some()))
            .saturating_add(u64::from(self.awaiting.is_some()))
    }

    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.constructor, mark);
        trace_stored_value_root(&self.source, mark);
        trace_stored_value_root(&self.this_arg, mark);
        trace_promise_capability(&self.capability, mark);
        if let Some(mapper) = self.mapper {
            mark(CollectionRoot::Heap(HeapReference::Function(mapper)));
        }
        for value in [
            self.target.as_ref(),
            self.iterator.as_ref(),
            self.next.as_ref(),
            self.result.as_ref(),
            self.original_reason.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            trace_stored_value_root(value, mark);
        }
        if let Some(awaiting) = self.awaiting {
            mark(CollectionRoot::Heap(HeapReference::Object(awaiting)));
        }
    }

    const fn closes_on_abrupt(&self) -> bool {
        matches!(
            self.stage,
            ArrayFromAsyncStage::IterableMapperCall
                | ArrayFromAsyncStage::IterableMapperAwaitSetup
                | ArrayFromAsyncStage::IterableMapperAwaitReaction
                | ArrayFromAsyncStage::IterableDefineElement
        )
    }

    const fn is_closing(&self) -> bool {
        matches!(
            self.stage,
            ArrayFromAsyncStage::CloseReturnMethod
                | ArrayFromAsyncStage::CloseReturnCall
                | ArrayFromAsyncStage::CloseAwaitSetup
                | ArrayFromAsyncStage::CloseAwaitReaction
        )
    }
}

fn allocate_capability(
    runtime: &mut Runtime,
    realm: RealmId,
) -> Result<crate::object::PromiseCapability, NativeFailure> {
    let prototype = runtime.realm_promise_prototype(realm)?;
    let promise = runtime.allocate_promise_with_prototype(HeapReference::Object(prototype))?;
    let (resolve, reject) = runtime.allocate_promise_resolving_functions(promise, realm)?;
    Ok(crate::object::PromiseCapability {
        promise: StoredValue::Object(promise),
        resolve,
        reject,
    })
}

pub(super) fn begin_array_from_async(
    runtime: &mut Runtime,
    realm: RealmId,
    constructor: StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let capability = allocate_capability(runtime, realm)?;
    let source = arguments.take_first_or_undefined();
    let mapper_value = arguments.take_first_or_undefined();
    let this_arg = arguments.take_first_or_undefined();
    let mapper = match mapper_value {
        StoredValue::Undefined => None,
        StoredValue::Function(function) => Some(function),
        _ => {
            let state = ArrayFromAsyncRecord {
                constructor,
                source,
                mapper: None,
                this_arg,
                capability,
                target: None,
                iterator: None,
                next: None,
                result: None,
                original_reason: None,
                awaiting: None,
                length: 0,
                index: 0,
                realm,
                stage: ArrayFromAsyncStage::AsyncIteratorMethod,
                sync_fallback: false,
                origin,
            };
            let pending = array_from_async_exception(
                state.realm,
                state.origin.clone(),
                ExceptionKind::TypeError,
                "not a function",
            )?;
            return reject_array_from_async_pending(runtime, state, pending, return_to);
        }
    };
    let state = ArrayFromAsyncRecord {
        constructor,
        source,
        mapper,
        this_arg,
        capability,
        target: None,
        iterator: None,
        next: None,
        result: None,
        original_reason: None,
        awaiting: None,
        length: 0,
        index: 0,
        realm,
        stage: ArrayFromAsyncStage::AsyncIteratorMethod,
        sync_fallback: false,
        origin,
    };
    read_array_from_async_property(
        runtime,
        state,
        runtime.predefined_symbol_property_key(PredefinedAtom::SymbolAsyncIterator),
        "Symbol.asyncIterator",
        return_to,
        execution_budget,
    )
}

/// Resumes an ordinary getter, constructor, method, mapper, or setter call.
#[expect(
    clippy::too_many_lines,
    reason = "one exhaustive match keeps the resumable ECMA-262 stage transitions visible and auditable"
)]
pub(super) fn advance_array_from_async(
    runtime: &mut Runtime,
    mut state: ArrayFromAsyncRecord,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            ArrayFromAsyncStage::AsyncIteratorMethod => {
                let method = take_array_from_async_completion(&mut completion)?;
                match method {
                    StoredValue::Undefined | StoredValue::Null => {
                        state.stage = ArrayFromAsyncStage::SyncIteratorMethod;
                        return read_array_from_async_property(
                            runtime,
                            state,
                            runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
                            "Symbol.iterator",
                            return_to,
                            execution_budget,
                        );
                    }
                    StoredValue::Function(method) => {
                        let receiver = state.source.duplicate();
                        state.stage = ArrayFromAsyncStage::IteratorMethodCall;
                        return call_array_from_async_function(
                            state,
                            method,
                            receiver,
                            Vec::new(),
                            None,
                            return_to,
                        );
                    }
                    _ => {
                        let pending = array_from_async_exception(
                            state.realm,
                            state.origin.clone(),
                            ExceptionKind::TypeError,
                            "not a function",
                        )?;
                        return reject_array_from_async_pending(runtime, state, pending, return_to);
                    }
                }
            }
            ArrayFromAsyncStage::SyncIteratorMethod => {
                let method = take_array_from_async_completion(&mut completion)?;
                match method {
                    StoredValue::Undefined | StoredValue::Null => {
                        let source = std::mem::replace(&mut state.source, StoredValue::Undefined);
                        state.source = match to_object_value(
                            runtime,
                            state.realm,
                            source,
                            state.origin.clone(),
                        )? {
                            Ok(object) => object,
                            Err(pending) => {
                                return reject_array_from_async_pending(
                                    runtime, state, pending, return_to,
                                );
                            }
                        };
                        state.stage = ArrayFromAsyncStage::ArrayLikeLength;
                        return read_array_from_async_property(
                            runtime,
                            state,
                            runtime.predefined_property_key(PredefinedAtom::Length),
                            "length",
                            return_to,
                            execution_budget,
                        );
                    }
                    StoredValue::Function(method) => {
                        state.sync_fallback = true;
                        let receiver = state.source.duplicate();
                        state.stage = ArrayFromAsyncStage::IteratorMethodCall;
                        return call_array_from_async_function(
                            state,
                            method,
                            receiver,
                            Vec::new(),
                            None,
                            return_to,
                        );
                    }
                    _ => {
                        let pending = array_from_async_exception(
                            state.realm,
                            state.origin.clone(),
                            ExceptionKind::TypeError,
                            "not a function",
                        )?;
                        return reject_array_from_async_pending(runtime, state, pending, return_to);
                    }
                }
            }
            ArrayFromAsyncStage::IterableConstruct => {
                if let Some(value) = completion.take() {
                    let target = match require_array_from_async_object(&state, value) {
                        Ok(target) => target,
                        Err(pending) => {
                            return reject_array_from_async_pending(
                                runtime, state, pending, return_to,
                            );
                        }
                    };
                    state.target = Some(target);
                }
                return call_array_from_async_next(runtime, state, return_to, execution_budget);
            }
            ArrayFromAsyncStage::IteratorMethodCall => {
                let value = take_array_from_async_completion(&mut completion)?;
                let iterator = match require_array_from_async_object(&state, value) {
                    Ok(iterator) => iterator,
                    Err(pending) => {
                        return reject_array_from_async_pending(runtime, state, pending, return_to);
                    }
                };
                state.iterator = Some(iterator);
                state.stage = ArrayFromAsyncStage::NextMethod;
                return read_array_from_async_property(
                    runtime,
                    state,
                    runtime.predefined_property_key(PredefinedAtom::Next),
                    "next",
                    return_to,
                    execution_budget,
                );
            }
            ArrayFromAsyncStage::NextMethod => {
                state.next = Some(take_array_from_async_completion(&mut completion)?);
                if state.sync_fallback {
                    let iterator = state.iterator.take().ok_or(EngineFault::RuntimeInvariant {
                        message: "Array.fromAsync sync fallback lost its iterator",
                    })?;
                    let next = state.next.take().ok_or(EngineFault::RuntimeInvariant {
                        message: "Array.fromAsync sync fallback lost its next method",
                    })?;
                    let wrapper =
                        runtime.allocate_async_from_sync_iterator(state.realm, iterator, next)?;
                    state.iterator = Some(StoredValue::Object(wrapper));
                    state.next = Some(StoredValue::Function(
                        runtime.realm_async_from_sync_iterator_next(state.realm)?,
                    ));
                }
                state.stage = ArrayFromAsyncStage::IterableConstruct;
                return allocate_or_construct_array_from_async(
                    runtime,
                    state,
                    Vec::new(),
                    return_to,
                    execution_budget,
                );
            }
            ArrayFromAsyncStage::NextCall => {
                let value = take_array_from_async_completion(&mut completion)?;
                return begin_array_from_async_await(
                    runtime,
                    state,
                    value,
                    ArrayFromAsyncStage::NextAwaitSetup,
                    return_to,
                    execution_budget,
                );
            }
            ArrayFromAsyncStage::NextAwaitSetup => {
                return finish_array_from_async_await(
                    runtime,
                    state,
                    &take_array_from_async_completion(&mut completion)?,
                    ArrayFromAsyncStage::NextAwaitReaction,
                );
            }
            ArrayFromAsyncStage::NextAwaitReaction => {
                let value = take_array_from_async_completion(&mut completion)?;
                let result = match require_array_from_async_object(&state, value) {
                    Ok(result) => result,
                    Err(pending) => {
                        return reject_array_from_async_pending(runtime, state, pending, return_to);
                    }
                };
                state.result = Some(result);
                state.stage = ArrayFromAsyncStage::Done;
                return read_array_from_async_property(
                    runtime,
                    state,
                    runtime.predefined_property_key(PredefinedAtom::Done),
                    "done",
                    return_to,
                    execution_budget,
                );
            }
            ArrayFromAsyncStage::Done => {
                if take_array_from_async_completion(&mut completion)?.is_truthy() {
                    state.iterator = None;
                    state.next = None;
                    state.result = None;
                    state.length = state.index;
                    return finish_array_from_async_length(
                        runtime,
                        state,
                        return_to,
                        execution_budget,
                    );
                }
                state.stage = ArrayFromAsyncStage::Value;
                return read_array_from_async_property(
                    runtime,
                    state,
                    runtime.predefined_property_key(PredefinedAtom::Value),
                    "value",
                    return_to,
                    execution_budget,
                );
            }
            ArrayFromAsyncStage::Value => {
                let value = take_array_from_async_completion(&mut completion)?;
                if let Some(mapper) = state.mapper {
                    let arguments = array_static_mapper_arguments(value, state.index)?;
                    let receiver = state.this_arg.duplicate();
                    state.stage = ArrayFromAsyncStage::IterableMapperCall;
                    return call_array_from_async_function(
                        state, mapper, receiver, arguments, None, return_to,
                    );
                }
                state.stage = ArrayFromAsyncStage::IterableDefineElement;
                if let Some(pending) =
                    define_array_from_async_element(runtime, &mut state, value, execution_budget)?
                {
                    return begin_array_from_async_close_pending(
                        runtime,
                        state,
                        pending,
                        return_to,
                        execution_budget,
                    );
                }
                state.result = None;
                return call_array_from_async_next(runtime, state, return_to, execution_budget);
            }
            ArrayFromAsyncStage::IterableMapperCall => {
                let value = take_array_from_async_completion(&mut completion)?;
                return begin_array_from_async_await(
                    runtime,
                    state,
                    value,
                    ArrayFromAsyncStage::IterableMapperAwaitSetup,
                    return_to,
                    execution_budget,
                );
            }
            ArrayFromAsyncStage::IterableMapperAwaitSetup => {
                return finish_array_from_async_await(
                    runtime,
                    state,
                    &take_array_from_async_completion(&mut completion)?,
                    ArrayFromAsyncStage::IterableMapperAwaitReaction,
                );
            }
            ArrayFromAsyncStage::IterableMapperAwaitReaction => {
                let value = take_array_from_async_completion(&mut completion)?;
                state.stage = ArrayFromAsyncStage::IterableDefineElement;
                if let Some(pending) =
                    define_array_from_async_element(runtime, &mut state, value, execution_budget)?
                {
                    return begin_array_from_async_close_pending(
                        runtime,
                        state,
                        pending,
                        return_to,
                        execution_budget,
                    );
                }
                state.result = None;
                return call_array_from_async_next(runtime, state, return_to, execution_budget);
            }
            ArrayFromAsyncStage::IterableDefineElement
            | ArrayFromAsyncStage::ArrayLikeDefineElement => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Array.fromAsync definition stage resumed",
                }
                .into());
            }
            ArrayFromAsyncStage::ArrayLikeLength => {
                let value = take_array_from_async_completion(&mut completion)?;
                state.stage = ArrayFromAsyncStage::ArrayLikeLengthConversion;
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    return begin_array_from_async_length_conversion(
                        runtime,
                        state,
                        value,
                        return_to,
                        execution_budget,
                    );
                }
                completion = Some(value);
            }
            ArrayFromAsyncStage::ArrayLikeLengthConversion => {
                let value = take_array_from_async_completion(&mut completion)?;
                let number = match operator_to_number(value, state.realm, &state.origin) {
                    Ok(number) => number,
                    Err(
                        NativeFailure::Abrupt(pending)
                        | NativeFailure::AbruptAfterTransient(pending),
                    ) => {
                        return reject_array_from_async_pending(runtime, state, pending, return_to);
                    }
                    Err(NativeFailure::Execution(error)) => {
                        return Err(NativeFailure::Execution(error));
                    }
                };
                state.length = number_to_length(number);
                state.stage = ArrayFromAsyncStage::ArrayLikeConstruct;
                let arguments = array_static_single_argument(state.length)?;
                return allocate_or_construct_array_from_async(
                    runtime,
                    state,
                    arguments,
                    return_to,
                    execution_budget,
                );
            }
            ArrayFromAsyncStage::ArrayLikeConstruct => {
                if let Some(value) = completion.take() {
                    let target = match require_array_from_async_object(&state, value) {
                        Ok(target) => target,
                        Err(pending) => {
                            return reject_array_from_async_pending(
                                runtime, state, pending, return_to,
                            );
                        }
                    };
                    state.target = Some(target);
                }
                if state.index >= state.length {
                    return finish_array_from_async_length(
                        runtime,
                        state,
                        return_to,
                        execution_budget,
                    );
                }
                let key = array_static_index_key(runtime, state.index)?;
                state.stage = ArrayFromAsyncStage::ArrayLikeElement;
                return read_array_from_async_property(
                    runtime,
                    state,
                    key,
                    "array-like element",
                    return_to,
                    execution_budget,
                );
            }
            ArrayFromAsyncStage::ArrayLikeElement => {
                let value = take_array_from_async_completion(&mut completion)?;
                return begin_array_from_async_await(
                    runtime,
                    state,
                    value,
                    ArrayFromAsyncStage::ArrayLikeElementAwaitSetup,
                    return_to,
                    execution_budget,
                );
            }
            ArrayFromAsyncStage::ArrayLikeElementAwaitSetup => {
                return finish_array_from_async_await(
                    runtime,
                    state,
                    &take_array_from_async_completion(&mut completion)?,
                    ArrayFromAsyncStage::ArrayLikeElementAwaitReaction,
                );
            }
            ArrayFromAsyncStage::ArrayLikeElementAwaitReaction => {
                let value = take_array_from_async_completion(&mut completion)?;
                if let Some(mapper) = state.mapper {
                    let arguments = array_static_mapper_arguments(value, state.index)?;
                    let receiver = state.this_arg.duplicate();
                    state.stage = ArrayFromAsyncStage::ArrayLikeMapperCall;
                    return call_array_from_async_function(
                        state, mapper, receiver, arguments, None, return_to,
                    );
                }
                state.stage = ArrayFromAsyncStage::ArrayLikeDefineElement;
                if let Some(pending) =
                    define_array_from_async_element(runtime, &mut state, value, execution_budget)?
                {
                    return reject_array_from_async_pending(runtime, state, pending, return_to);
                }
                state.stage = ArrayFromAsyncStage::ArrayLikeConstruct;
                completion = None;
            }
            ArrayFromAsyncStage::ArrayLikeMapperCall => {
                let value = take_array_from_async_completion(&mut completion)?;
                return begin_array_from_async_await(
                    runtime,
                    state,
                    value,
                    ArrayFromAsyncStage::ArrayLikeMapperAwaitSetup,
                    return_to,
                    execution_budget,
                );
            }
            ArrayFromAsyncStage::ArrayLikeMapperAwaitSetup => {
                return finish_array_from_async_await(
                    runtime,
                    state,
                    &take_array_from_async_completion(&mut completion)?,
                    ArrayFromAsyncStage::ArrayLikeMapperAwaitReaction,
                );
            }
            ArrayFromAsyncStage::ArrayLikeMapperAwaitReaction => {
                let value = take_array_from_async_completion(&mut completion)?;
                state.stage = ArrayFromAsyncStage::ArrayLikeDefineElement;
                if let Some(pending) =
                    define_array_from_async_element(runtime, &mut state, value, execution_budget)?
                {
                    return reject_array_from_async_pending(runtime, state, pending, return_to);
                }
                state.stage = ArrayFromAsyncStage::ArrayLikeConstruct;
                completion = None;
            }
            ArrayFromAsyncStage::LengthSetter => {
                let _ = take_array_from_async_completion(&mut completion)?;
                let target = array_from_async_target(&state)?.duplicate();
                return resolve_array_from_async(state, target, return_to);
            }
            ArrayFromAsyncStage::CloseReturnMethod => {
                let method = take_array_from_async_completion(&mut completion)?;
                let StoredValue::Function(method) = method else {
                    return reject_array_from_async_original(state, return_to);
                };
                let receiver = state
                    .iterator
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Array.fromAsync close lost its iterator",
                    })?
                    .duplicate();
                state.stage = ArrayFromAsyncStage::CloseReturnCall;
                return call_array_from_async_function(
                    state,
                    method,
                    receiver,
                    Vec::new(),
                    None,
                    return_to,
                );
            }
            ArrayFromAsyncStage::CloseReturnCall => {
                let value = take_array_from_async_completion(&mut completion)?;
                return begin_array_from_async_await(
                    runtime,
                    state,
                    value,
                    ArrayFromAsyncStage::CloseAwaitSetup,
                    return_to,
                    execution_budget,
                );
            }
            ArrayFromAsyncStage::CloseAwaitSetup => {
                return finish_array_from_async_await(
                    runtime,
                    state,
                    &take_array_from_async_completion(&mut completion)?,
                    ArrayFromAsyncStage::CloseAwaitReaction,
                );
            }
            ArrayFromAsyncStage::CloseAwaitReaction => {
                let _ = take_array_from_async_completion(&mut completion)?;
                return reject_array_from_async_original(state, return_to);
            }
        }
    }
}

fn allocate_or_construct_array_from_async(
    runtime: &mut Runtime,
    mut state: ArrayFromAsyncRecord,
    arguments: Vec<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Function(constructor) = state.constructor
        && function_is_constructor(runtime, constructor)?
    {
        return call_array_from_async_function(
            state,
            constructor,
            StoredValue::Undefined,
            arguments,
            Some(constructor),
            return_to,
        );
    }

    let length = match state.stage {
        ArrayFromAsyncStage::IterableConstruct => 0,
        ArrayFromAsyncStage::ArrayLikeConstruct => {
            let Ok(length) = u32::try_from(state.length) else {
                let pending = array_from_async_exception(
                    state.realm,
                    state.origin.clone(),
                    ExceptionKind::RangeError,
                    "invalid array length",
                )?;
                return reject_array_from_async_pending(runtime, state, pending, return_to);
            };
            length
        }
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Array.fromAsync allocation started outside construction",
            }
            .into());
        }
    };
    execution_budget.charge_instructions(1)?;
    let prototype = runtime.realm_array_prototype(state.realm)?;
    let target =
        runtime.allocate_sparse_array_with_prototype(HeapReference::Object(prototype), length)?;
    state.target = Some(StoredValue::Object(target));
    advance_array_from_async(runtime, state, None, return_to, execution_budget)
}

fn call_array_from_async_next(
    runtime: &mut Runtime,
    mut state: ArrayFromAsyncRecord,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.index >= MAX_SAFE_INTEGER {
        let pending = array_from_async_exception(
            state.realm,
            state.origin.clone(),
            ExceptionKind::TypeError,
            "array too long",
        )?;
        return begin_array_from_async_close_pending(
            runtime,
            state,
            pending,
            return_to,
            execution_budget,
        );
    }
    let StoredValue::Function(next) = state.next.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "Array.fromAsync iterator advance lost its next method",
    })?
    else {
        let pending = array_from_async_exception(
            state.realm,
            state.origin.clone(),
            ExceptionKind::TypeError,
            "not a function",
        )?;
        return reject_array_from_async_pending(runtime, state, pending, return_to);
    };
    execution_budget.charge_instructions(1)?;
    let function = *next;
    let receiver = state
        .iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Array.fromAsync iterator advance lost its iterator",
        })?
        .duplicate();
    state.stage = ArrayFromAsyncStage::NextCall;
    call_array_from_async_function(state, function, receiver, Vec::new(), None, return_to)
}

fn begin_array_from_async_await(
    runtime: &mut Runtime,
    mut state: ArrayFromAsyncRecord,
    value: StoredValue,
    setup_stage: ArrayFromAsyncStage,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = setup_stage;
    let constructor = runtime.realm_promise_constructor(state.realm)?;
    let dispatch = match begin_promise_resolve_with_constructor(
        runtime,
        state.realm,
        constructor,
        value,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending)) => {
            return resume_array_from_async_abrupt(
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
    };
    match dispatch {
        NativeDispatch::Immediate(value) => {
            let reaction_stage = reaction_stage_for_setup(setup_stage)?;
            finish_array_from_async_await(runtime, state, &value, reaction_stage)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(&mut call, one_array_from_async_continuation(state)?)?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(&mut frame, one_array_from_async_continuation(state)?)?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Array.fromAsync PromiseResolve produced a structured result",
        }
        .into()),
    }
}

fn reaction_stage_for_setup(
    stage: ArrayFromAsyncStage,
) -> Result<ArrayFromAsyncStage, NativeFailure> {
    match stage {
        ArrayFromAsyncStage::NextAwaitSetup => Ok(ArrayFromAsyncStage::NextAwaitReaction),
        ArrayFromAsyncStage::IterableMapperAwaitSetup => {
            Ok(ArrayFromAsyncStage::IterableMapperAwaitReaction)
        }
        ArrayFromAsyncStage::ArrayLikeElementAwaitSetup => {
            Ok(ArrayFromAsyncStage::ArrayLikeElementAwaitReaction)
        }
        ArrayFromAsyncStage::ArrayLikeMapperAwaitSetup => {
            Ok(ArrayFromAsyncStage::ArrayLikeMapperAwaitReaction)
        }
        ArrayFromAsyncStage::CloseAwaitSetup => Ok(ArrayFromAsyncStage::CloseAwaitReaction),
        _ => Err(EngineFault::RuntimeInvariant {
            message: "Array.fromAsync await started from a non-setup stage",
        }
        .into()),
    }
}

fn finish_array_from_async_await(
    runtime: &mut Runtime,
    mut state: ArrayFromAsyncRecord,
    value: &StoredValue,
    reaction_stage: ArrayFromAsyncStage,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(promise) = value else {
        return Err(EngineFault::RuntimeInvariant {
            message: "Array.fromAsync PromiseResolve produced a non-object",
        }
        .into());
    };
    state.stage = reaction_stage;
    state.awaiting = Some(*promise);
    let operation = array_from_async_operation(&state)?;
    store_array_from_async_record(runtime, operation, state)?;
    if let Err(error) = perform_array_from_async_await(runtime, *promise, operation) {
        let removed = runtime.array_from_async_states.remove(&operation);
        debug_assert!(removed.is_some());
        return Err(error);
    }
    runtime.collection_pending = true;
    Ok(NativeDispatch::Immediate(StoredValue::Object(operation)))
}

pub(super) fn begin_array_from_async_resume(
    runtime: &mut Runtime,
    operation: ObjectId,
    kind: crate::object::PromiseReactionKind,
    argument: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut state = take_array_from_async_record(runtime, operation)?;
    let awaiting = state.awaiting.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Array.fromAsync Promise reaction resumed a non-awaiting state",
    })?;
    if !runtime.objects.contains(awaiting) {
        return Err(EngineFault::StaleHeapEdge {
            edge: "Array.fromAsync awaited Promise",
            index: awaiting.index(),
            generation: awaiting.generation(),
        }
        .into());
    }
    runtime.collection_pending = true;
    match kind {
        crate::object::PromiseReactionKind::Fulfill => {
            advance_array_from_async(runtime, state, Some(argument), None, execution_budget)
        }
        crate::object::PromiseReactionKind::Reject => {
            if state.stage == ArrayFromAsyncStage::IterableMapperAwaitReaction {
                begin_array_from_async_close_reason(
                    runtime,
                    state,
                    argument,
                    None,
                    execution_budget,
                )
            } else if state.stage == ArrayFromAsyncStage::CloseAwaitReaction {
                reject_array_from_async_original(state, None)
            } else {
                reject_array_from_async_reason(state, argument, None)
            }
        }
    }
}

fn define_array_from_async_element(
    runtime: &mut Runtime,
    state: &mut ArrayFromAsyncRecord,
    value: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<Option<PendingException>, NativeFailure> {
    let key = array_static_index_key(runtime, state.index)?;
    let target = array_from_async_target(state)?.duplicate();
    charge_heap_property_lookup(runtime, &target, execution_budget)?;
    match define_static_property(runtime, &target, key, value, execution_budget)? {
        PropertyWriteOutcome::Complete => {
            state.index = state.index.saturating_add(1);
            Ok(None)
        }
        PropertyWriteOutcome::Failed(failure) => Ok(Some(property_exception_at(
            state.realm,
            state.origin.clone(),
            None,
            failure,
        )?)),
        PropertyWriteOutcome::Setter { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Array.fromAsync CreateDataPropertyOrThrow called a setter",
        }
        .into()),
    }
}

fn finish_array_from_async_length(
    runtime: &mut Runtime,
    mut state: ArrayFromAsyncRecord,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = array_from_async_target(&state)?.duplicate();
    let key = runtime.predefined_property_key(PredefinedAtom::Length);
    let name = JsString::from_utf8("length")?;
    if let StoredValue::Object(object) = target
        && runtime.is_array_object(object)?
    {
        let Ok(length) = u32::try_from(state.length) else {
            let pending = array_from_async_exception(
                state.realm,
                state.origin.clone(),
                ExceptionKind::RangeError,
                "invalid array length",
            )?;
            return reject_array_from_async_pending(runtime, state, pending, return_to);
        };
        let work = runtime.preview_array_length_write_work(object, length)?;
        execution_budget.charge_instructions(work)?;
        return match runtime.set_array_length(object, length)? {
            ArrayLengthWriteOutcome::Complete => resolve_array_from_async(state, target, return_to),
            ArrayLengthWriteOutcome::ReadOnly => {
                let pending = property_exception_at(
                    state.realm,
                    state.origin.clone(),
                    Some(&name),
                    PropertyFailure::ReadOnly,
                )?;
                reject_array_from_async_pending(runtime, state, pending, return_to)
            }
            ArrayLengthWriteOutcome::BlockedByNonConfigurable { .. } => {
                let pending = property_exception_at(
                    state.realm,
                    state.origin.clone(),
                    Some(&name),
                    PropertyFailure::NotConfigurable,
                )?;
                reject_array_from_async_pending(runtime, state, pending, return_to)
            }
        };
    }

    charge_heap_property_lookup(runtime, &target, execution_budget)?;
    match write_static_property(
        runtime,
        state.realm,
        &target,
        key,
        StoredValue::Number(JsNumber::from_f64(array_static_index_as_f64(state.length))),
        true,
        execution_budget,
    )? {
        PropertyWriteOutcome::Complete => resolve_array_from_async(state, target, return_to),
        PropertyWriteOutcome::Setter {
            function,
            receiver,
            value,
        } => {
            state.stage = ArrayFromAsyncStage::LengthSetter;
            call_array_from_async_function(
                state,
                function,
                receiver,
                array_static_one_value(value)?,
                None,
                return_to,
            )
        }
        PropertyWriteOutcome::Failed(failure) => {
            let pending =
                property_exception_at(state.realm, state.origin.clone(), Some(&name), failure)?;
            reject_array_from_async_pending(runtime, state, pending, return_to)
        }
    }
}

fn begin_array_from_async_length_conversion(
    runtime: &mut Runtime,
    state: ArrayFromAsyncRecord,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let operation = array_from_async_operation(&state)?;
    store_array_from_async_record(runtime, operation, state)?;
    let result = begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::ArrayFromAsyncLength { operation },
        runtime
            .array_from_async_states
            .get(&operation)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Array.fromAsync length conversion lost its state",
            })?
            .realm,
        return_to,
        runtime
            .array_from_async_states
            .get(&operation)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Array.fromAsync length conversion lost its state",
            })?
            .origin
            .clone(),
        execution_budget,
    );
    match result {
        Ok(dispatch) => Ok(dispatch),
        Err(NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending)) => {
            resume_array_from_async_length_conversion_abrupt(
                runtime,
                operation,
                pending,
                return_to,
                execution_budget,
            )
        }
        Err(NativeFailure::Execution(error)) => {
            abandon_array_from_async_length_conversion(runtime, operation);
            Err(NativeFailure::Execution(error))
        }
    }
}

pub(super) fn resume_array_from_async_length_conversion(
    runtime: &mut Runtime,
    operation: ObjectId,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = take_array_from_async_record(runtime, operation)?;
    advance_array_from_async(runtime, state, Some(value), return_to, execution_budget)
}

pub(super) fn resume_array_from_async_length_conversion_abrupt(
    runtime: &mut Runtime,
    operation: ObjectId,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = take_array_from_async_record(runtime, operation)?;
    resume_array_from_async_abrupt(runtime, state, pending, return_to, execution_budget)
}

pub(super) fn abandon_array_from_async_length_conversion(
    runtime: &mut Runtime,
    operation: ObjectId,
) {
    let removed = runtime.array_from_async_states.remove(&operation);
    debug_assert!(removed.is_some());
    runtime.collection_pending = true;
}

fn store_array_from_async_record(
    runtime: &mut Runtime,
    operation: ObjectId,
    state: ArrayFromAsyncRecord,
) -> Result<(), NativeFailure> {
    if runtime.array_from_async_states.contains_key(&operation) {
        return Err(EngineFault::RuntimeInvariant {
            message: "Array.fromAsync operation was already suspended",
        }
        .into());
    }
    runtime
        .array_from_async_states
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    let previous = runtime.array_from_async_states.insert(operation, state);
    debug_assert!(previous.is_none());
    runtime.collection_pending = true;
    Ok(())
}

fn take_array_from_async_record(
    runtime: &mut Runtime,
    operation: ObjectId,
) -> Result<ArrayFromAsyncRecord, NativeFailure> {
    let state = runtime.array_from_async_states.remove(&operation).ok_or(
        EngineFault::RuntimeInvariant {
            message: "Array.fromAsync operation lost its suspended state",
        },
    )?;
    runtime.collection_pending = true;
    Ok(state)
}

fn read_array_from_async_property(
    runtime: &mut Runtime,
    state: ArrayFromAsyncRecord,
    key: PropertyKey,
    diagnostic_name: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = match state.stage {
        ArrayFromAsyncStage::AsyncIteratorMethod
        | ArrayFromAsyncStage::SyncIteratorMethod
        | ArrayFromAsyncStage::ArrayLikeLength
        | ArrayFromAsyncStage::ArrayLikeElement => &state.source,
        ArrayFromAsyncStage::NextMethod | ArrayFromAsyncStage::CloseReturnMethod => state
            .iterator
            .as_ref()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Array.fromAsync iterator property read lost its iterator",
            })?,
        ArrayFromAsyncStage::Done | ArrayFromAsyncStage::Value => {
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "Array.fromAsync result property read lost its result",
            })?
        }
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Array.fromAsync attempted a property read from the wrong stage",
            }
            .into());
        }
    };
    let base = base.duplicate();
    charge_array_from_async_lookup(runtime, &base, execution_budget)?;
    let name = JsString::from_utf8(diagnostic_name)?;
    let dispatch = match begin_value_get(
        runtime,
        &base,
        key,
        Some(&name),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending)) => {
            return resume_array_from_async_abrupt(
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
        array_from_async_continuation,
        |state, value| {
            advance_array_from_async(runtime, state, Some(value), return_to, execution_budget)
        },
        "Array.fromAsync Get produced a structured result",
    )
}

fn array_from_async_continuation(state: ArrayFromAsyncRecord) -> NativeContinuation {
    NativeContinuation::ArrayFromAsync(Box::new(state))
}

fn call_array_from_async_function(
    state: ArrayFromAsyncRecord,
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
    new_target: Option<FunctionId>,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin,
        continuations: one_array_from_async_continuation(state)?,
        pre_call: None,
        new_target,
        native_caller: None,
    }))
}

fn one_array_from_async_continuation(
    state: ArrayFromAsyncRecord,
) -> Result<Vec<NativeContinuation>, NativeFailure> {
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::ArrayFromAsync(Box::new(state)));
    Ok(continuations)
}

fn begin_array_from_async_close_pending(
    runtime: &mut Runtime,
    state: ArrayFromAsyncRecord,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let reason = pending_exception_value(runtime, pending)?;
    begin_array_from_async_close_reason(runtime, state, reason, return_to, execution_budget)
}

fn begin_array_from_async_close_reason(
    runtime: &mut Runtime,
    mut state: ArrayFromAsyncRecord,
    reason: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.iterator.is_none() {
        return Err(EngineFault::RuntimeInvariant {
            message: "Array.fromAsync AsyncIteratorClose started without an iterator",
        }
        .into());
    }
    state.original_reason = Some(reason);
    state.stage = ArrayFromAsyncStage::CloseReturnMethod;
    read_array_from_async_property(
        runtime,
        state,
        runtime.predefined_property_key(PredefinedAtom::Return),
        "return",
        return_to,
        execution_budget,
    )
}

pub(super) fn resume_array_from_async_abrupt(
    runtime: &mut Runtime,
    state: ArrayFromAsyncRecord,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.is_closing() {
        return reject_array_from_async_original(state, return_to);
    }
    if state.closes_on_abrupt() {
        return begin_array_from_async_close_pending(
            runtime,
            state,
            pending,
            return_to,
            execution_budget,
        );
    }
    reject_array_from_async_pending(runtime, state, pending, return_to)
}

fn resolve_array_from_async(
    state: ArrayFromAsyncRecord,
    value: StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin;
    call_capability_settlement(state.capability, true, value, return_to, origin)
}

fn reject_array_from_async_pending(
    runtime: &mut Runtime,
    state: ArrayFromAsyncRecord,
    pending: PendingException,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let reason = pending_exception_value(runtime, pending)?;
    reject_array_from_async_reason(state, reason, return_to)
}

fn reject_array_from_async_reason(
    state: ArrayFromAsyncRecord,
    reason: StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin;
    call_capability_settlement(state.capability, false, reason, return_to, origin)
}

fn reject_array_from_async_original(
    mut state: ArrayFromAsyncRecord,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let reason = state
        .original_reason
        .take()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Array.fromAsync close lost its original reason",
        })?;
    reject_array_from_async_reason(state, reason, return_to)
}

fn array_from_async_operation(state: &ArrayFromAsyncRecord) -> Result<ObjectId, NativeFailure> {
    let StoredValue::Object(operation) = state.capability.promise else {
        return Err(EngineFault::RuntimeInvariant {
            message: "Array.fromAsync capability has no Promise object",
        }
        .into());
    };
    Ok(operation)
}

fn array_from_async_target(state: &ArrayFromAsyncRecord) -> Result<&StoredValue, EngineFault> {
    state.target.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "Array.fromAsync lost its constructed target",
    })
}

fn require_array_from_async_object(
    state: &ArrayFromAsyncRecord,
    value: StoredValue,
) -> Result<StoredValue, PendingException> {
    if value.heap_reference().is_some() {
        return Ok(value);
    }
    Err(array_from_async_exception(
        state.realm,
        state.origin.clone(),
        ExceptionKind::TypeError,
        "not an object",
    )
    .expect("static Array.fromAsync error message is valid"))
}

fn take_array_from_async_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Array.fromAsync resumed without a completion",
    })
}

fn charge_array_from_async_lookup(
    runtime: &Runtime,
    base: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    if base.heap_reference().is_none() {
        execution_budget.charge_instructions(1)?;
        return Ok(());
    }
    charge_heap_property_lookup(runtime, base, execution_budget)
}

fn array_from_async_exception(
    realm: RealmId,
    origin: JsStackFrame,
    kind: ExceptionKind,
    message: &str,
) -> Result<PendingException, JsStringError> {
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin,
    })
}
