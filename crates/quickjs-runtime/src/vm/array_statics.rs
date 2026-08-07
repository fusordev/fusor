/*
 * JavaScript Array constructor factory semantics derived from QuickJS.
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

//! Resumable `Array.from` and `Array.of` generic factory semantics.
//!
//! The implementation keeps the two specification paths explicit. In
//! particular, `Array.from` validates its mapper before probing the iterator,
//! allocates the result before calling the iterator method, closes only for
//! mapper and element-definition failures, and does not close for an abrupt
//! `IteratorStepValue` or final `length` assignment.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::enum_variant_names,
    reason = "the Await prefix makes every observable Array factory suspension boundary explicit"
)]
enum ArrayStaticStage {
    AwaitIteratorMethod,
    AwaitIterableConstruct,
    AwaitIterator,
    AwaitNextMethod,
    AwaitNextResult,
    AwaitDone,
    AwaitIteratorValue,
    AwaitIterableMapper,
    AwaitArrayLikeLength,
    AwaitArrayLikeLengthConversion,
    AwaitArrayLikeConstruct,
    AwaitArrayLikeElement,
    AwaitArrayLikeMapper,
    AwaitOfConstruct,
    AwaitLengthSetter,
}

/// One suspended `Array.from` or `Array.of` execution.
pub(crate) struct ArrayStaticContinuation {
    constructor: StoredValue,
    source: StoredValue,
    mapper: Option<FunctionId>,
    this_arg: StoredValue,
    items: Vec<StoredValue>,
    target: Option<StoredValue>,
    iterator_method: Option<FunctionId>,
    iterator: Option<StoredValue>,
    next: Option<StoredValue>,
    result: Option<StoredValue>,
    length: u64,
    index: u64,
    realm: RealmId,
    stage: ArrayStaticStage,
    origin: JsStackFrame,
}

impl ArrayStaticContinuation {
    pub(crate) fn retained_values(&self) -> u64 {
        3_u64
            .saturating_add(usize_to_u64(self.items.len()))
            .saturating_add(u64::from(self.mapper.is_some()))
            .saturating_add(u64::from(self.target.is_some()))
            .saturating_add(u64::from(self.iterator_method.is_some()))
            .saturating_add(u64::from(self.iterator.is_some()))
            .saturating_add(u64::from(self.next.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
    }

    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.constructor, mark);
        trace_stored_value_root(&self.source, mark);
        trace_stored_value_root(&self.this_arg, mark);
        if let Some(mapper) = self.mapper {
            mark(CollectionRoot::Heap(HeapReference::Function(mapper)));
        }
        for value in &self.items {
            trace_stored_value_root(value, mark);
        }
        for value in [
            self.target.as_ref(),
            self.iterator.as_ref(),
            self.next.as_ref(),
            self.result.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            trace_stored_value_root(value, mark);
        }
        if let Some(method) = self.iterator_method {
            mark(CollectionRoot::Heap(HeapReference::Function(method)));
        }
    }

    const fn closes_on_abrupt(&self) -> bool {
        matches!(self.stage, ArrayStaticStage::AwaitIterableMapper)
    }
}

/// Starts one generic `Array` constructor factory.
#[expect(
    clippy::too_many_arguments,
    reason = "native dispatch supplies the factory, realm, call values, return target, origin, and shared budget"
)]
pub(super) fn begin_array_static(
    runtime: &mut Runtime,
    method: ArrayStatic,
    realm: RealmId,
    constructor: StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if method == ArrayStatic::FromAsync {
        return begin_array_from_async(
            runtime,
            realm,
            constructor,
            arguments,
            return_to,
            origin,
            execution_budget,
        );
    }
    if method == ArrayStatic::Of {
        let items = arguments.into_remaining_values();
        let length = usize_to_u64(items.len());
        let state = ArrayStaticContinuation {
            constructor,
            source: StoredValue::Undefined,
            mapper: None,
            this_arg: StoredValue::Undefined,
            items,
            target: None,
            iterator_method: None,
            iterator: None,
            next: None,
            result: None,
            length,
            index: 0,
            realm,
            stage: ArrayStaticStage::AwaitOfConstruct,
            origin,
        };
        return allocate_or_construct_array_static(
            runtime,
            state,
            array_static_single_argument(length)?,
            return_to,
            execution_budget,
        );
    }

    let source = arguments.take_first_or_undefined();
    let mapper_value = arguments.take_first_or_undefined();
    let mapper = match mapper_value {
        StoredValue::Undefined => None,
        StoredValue::Function(function) => Some(function),
        _ => return array_static_type_error(realm, origin, "not a function"),
    };
    let this_arg = arguments.take_first_or_undefined();
    let state = ArrayStaticContinuation {
        constructor,
        source,
        mapper,
        this_arg,
        items: Vec::new(),
        target: None,
        iterator_method: None,
        iterator: None,
        next: None,
        result: None,
        length: 0,
        index: 0,
        realm,
        stage: ArrayStaticStage::AwaitIteratorMethod,
        origin,
    };
    read_array_static_property(
        runtime,
        state,
        runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        "Symbol.iterator",
        return_to,
        execution_budget,
    )
}

/// Advances one constructor, iterator, mapper, getter, or setter boundary.
#[allow(
    clippy::too_many_lines,
    reason = "one explicit state machine keeps the two Array factory algorithms and their different IteratorClose boundaries auditable"
)]
pub(super) fn advance_array_static(
    runtime: &mut Runtime,
    mut state: ArrayStaticContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            ArrayStaticStage::AwaitIteratorMethod => {
                let method = take_array_static_completion(&mut completion)?;
                match method {
                    StoredValue::Undefined | StoredValue::Null => {
                        state.source = match to_object_value(
                            runtime,
                            state.realm,
                            state.source,
                            state.origin.clone(),
                        )? {
                            Ok(object) => object,
                            Err(exception) => return Err(NativeFailure::Abrupt(exception)),
                        };
                        state.stage = ArrayStaticStage::AwaitArrayLikeLength;
                        return read_array_static_property(
                            runtime,
                            state,
                            runtime.predefined_property_key(PredefinedAtom::Length),
                            "length",
                            return_to,
                            execution_budget,
                        );
                    }
                    StoredValue::Function(method) => {
                        state.iterator_method = Some(method);
                        state.stage = ArrayStaticStage::AwaitIterableConstruct;
                        return allocate_or_construct_array_static(
                            runtime,
                            state,
                            Vec::new(),
                            return_to,
                            execution_budget,
                        );
                    }
                    _ => {
                        return array_static_type_error(
                            state.realm,
                            state.origin,
                            "not a function",
                        );
                    }
                }
            }
            ArrayStaticStage::AwaitIterableConstruct => {
                if let Some(value) = completion.take() {
                    state.target = Some(require_array_static_object(&state, value)?);
                }
                let method = state.iterator_method.ok_or(EngineFault::RuntimeInvariant {
                    message: "Array.from iterable path lost its iterator method",
                })?;
                let receiver = state.source.duplicate();
                state.stage = ArrayStaticStage::AwaitIterator;
                return call_array_static_function(
                    state,
                    method,
                    receiver,
                    Vec::new(),
                    None,
                    return_to,
                );
            }
            ArrayStaticStage::AwaitIterator => {
                let iterator = require_array_static_object(
                    &state,
                    take_array_static_completion(&mut completion)?,
                )?;
                state.iterator = Some(iterator);
                state.stage = ArrayStaticStage::AwaitNextMethod;
                return read_array_static_property(
                    runtime,
                    state,
                    runtime.predefined_property_key(PredefinedAtom::Next),
                    "next",
                    return_to,
                    execution_budget,
                );
            }
            ArrayStaticStage::AwaitNextMethod => {
                state.next = Some(take_array_static_completion(&mut completion)?);
                return call_array_static_next(runtime, state, return_to, execution_budget);
            }
            ArrayStaticStage::AwaitNextResult => {
                let result = require_array_static_object(
                    &state,
                    take_array_static_completion(&mut completion)?,
                )?;
                state.result = Some(result);
                state.stage = ArrayStaticStage::AwaitDone;
                return read_array_static_property(
                    runtime,
                    state,
                    runtime.predefined_property_key(PredefinedAtom::Done),
                    "done",
                    return_to,
                    execution_budget,
                );
            }
            ArrayStaticStage::AwaitDone => {
                if take_array_static_completion(&mut completion)?.is_truthy() {
                    state.iterator = None;
                    state.next = None;
                    state.result = None;
                    state.length = state.index;
                    return finish_array_static_length(runtime, state, return_to, execution_budget);
                }
                state.stage = ArrayStaticStage::AwaitIteratorValue;
                return read_array_static_property(
                    runtime,
                    state,
                    runtime.predefined_property_key(PredefinedAtom::Value),
                    "value",
                    return_to,
                    execution_budget,
                );
            }
            ArrayStaticStage::AwaitIteratorValue => {
                let value = take_array_static_completion(&mut completion)?;
                if let Some(mapper) = state.mapper {
                    let arguments = array_static_mapper_arguments(value, state.index)?;
                    let receiver = state.this_arg.duplicate();
                    state.stage = ArrayStaticStage::AwaitIterableMapper;
                    return call_array_static_function(
                        state, mapper, receiver, arguments, None, return_to,
                    );
                }
                if let Some(pending) =
                    define_array_static_element(runtime, &mut state, value, execution_budget)?
                {
                    return begin_array_static_close(
                        runtime,
                        state,
                        pending,
                        return_to,
                        execution_budget,
                    );
                }
                state.result = None;
                return call_array_static_next(runtime, state, return_to, execution_budget);
            }
            ArrayStaticStage::AwaitIterableMapper => {
                let value = take_array_static_completion(&mut completion)?;
                if let Some(pending) =
                    define_array_static_element(runtime, &mut state, value, execution_budget)?
                {
                    return begin_array_static_close(
                        runtime,
                        state,
                        pending,
                        return_to,
                        execution_budget,
                    );
                }
                state.result = None;
                return call_array_static_next(runtime, state, return_to, execution_budget);
            }
            ArrayStaticStage::AwaitArrayLikeLength => {
                let value = take_array_static_completion(&mut completion)?;
                state.stage = ArrayStaticStage::AwaitArrayLikeLengthConversion;
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::Number,
                        OperatorPrimitiveTarget::ArrayStaticLength(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                completion = Some(value);
            }
            ArrayStaticStage::AwaitArrayLikeLengthConversion => {
                state.length = number_to_length(operator_to_number(
                    take_array_static_completion(&mut completion)?,
                    state.realm,
                    &state.origin,
                )?);
                state.stage = ArrayStaticStage::AwaitArrayLikeConstruct;
                let arguments = array_static_single_argument(state.length)?;
                return allocate_or_construct_array_static(
                    runtime,
                    state,
                    arguments,
                    return_to,
                    execution_budget,
                );
            }
            ArrayStaticStage::AwaitArrayLikeConstruct => {
                if let Some(value) = completion.take() {
                    state.target = Some(require_array_static_object(&state, value)?);
                }
                if state.index >= state.length {
                    return finish_array_static_length(runtime, state, return_to, execution_budget);
                }
                let key = array_static_index_key(runtime, state.index)?;
                state.stage = ArrayStaticStage::AwaitArrayLikeElement;
                return read_array_static_property(
                    runtime,
                    state,
                    key,
                    "array-like element",
                    return_to,
                    execution_budget,
                );
            }
            ArrayStaticStage::AwaitArrayLikeElement => {
                let value = take_array_static_completion(&mut completion)?;
                if let Some(mapper) = state.mapper {
                    let arguments = array_static_mapper_arguments(value, state.index)?;
                    let receiver = state.this_arg.duplicate();
                    state.stage = ArrayStaticStage::AwaitArrayLikeMapper;
                    return call_array_static_function(
                        state, mapper, receiver, arguments, None, return_to,
                    );
                }
                if let Some(pending) =
                    define_array_static_element(runtime, &mut state, value, execution_budget)?
                {
                    return Err(NativeFailure::Abrupt(pending));
                }
                state.stage = ArrayStaticStage::AwaitArrayLikeConstruct;
            }
            ArrayStaticStage::AwaitArrayLikeMapper => {
                let value = take_array_static_completion(&mut completion)?;
                if let Some(pending) =
                    define_array_static_element(runtime, &mut state, value, execution_budget)?
                {
                    return Err(NativeFailure::Abrupt(pending));
                }
                state.stage = ArrayStaticStage::AwaitArrayLikeConstruct;
            }
            ArrayStaticStage::AwaitOfConstruct => {
                if let Some(value) = completion.take() {
                    state.target = Some(require_array_static_object(&state, value)?);
                }
                while state.index < state.length {
                    let index = usize::try_from(state.index).map_err(|_| {
                        EngineFault::RuntimeInvariant {
                            message: "Array.of item index does not fit usize",
                        }
                    })?;
                    let value = state.items[index].duplicate();
                    if let Some(pending) =
                        define_array_static_element(runtime, &mut state, value, execution_budget)?
                    {
                        return Err(NativeFailure::Abrupt(pending));
                    }
                }
                return finish_array_static_length(runtime, state, return_to, execution_budget);
            }
            ArrayStaticStage::AwaitLengthSetter => {
                let _ = take_array_static_completion(&mut completion)?;
                return Ok(NativeDispatch::Immediate(
                    array_static_target(&state)?.duplicate(),
                ));
            }
        }
    }
}

fn allocate_or_construct_array_static(
    runtime: &mut Runtime,
    mut state: ArrayStaticContinuation,
    arguments: Vec<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Function(constructor) = state.constructor
        && function_is_constructor(runtime, constructor)?
    {
        let receiver = StoredValue::Undefined;
        return call_array_static_function(
            state,
            constructor,
            receiver,
            arguments,
            Some(constructor),
            return_to,
        );
    }

    let length = match state.stage {
        ArrayStaticStage::AwaitIterableConstruct => 0,
        ArrayStaticStage::AwaitArrayLikeConstruct | ArrayStaticStage::AwaitOfConstruct => {
            u32::try_from(state.length).map_err(|_| {
                NativeFailure::Abrupt(
                    array_static_exception(
                        state.realm,
                        state.origin.clone(),
                        ExceptionKind::RangeError,
                        "invalid array length",
                    )
                    .expect("static Array error message is valid"),
                )
            })?
        }
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Array factory allocation started from a non-construction stage",
            }
            .into());
        }
    };
    execution_budget.charge_instructions(1)?;
    let prototype = runtime.realm_array_prototype(state.realm)?;
    let target =
        runtime.allocate_sparse_array_with_prototype(HeapReference::Object(prototype), length)?;
    state.target = Some(StoredValue::Object(target));
    advance_array_static(runtime, state, None, return_to, execution_budget)
}

fn call_array_static_next(
    runtime: &mut Runtime,
    mut state: ArrayStaticContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.index >= MAX_SAFE_INTEGER {
        let pending = array_static_exception(
            state.realm,
            state.origin.clone(),
            ExceptionKind::TypeError,
            "array too long",
        )?;
        return begin_array_static_close(runtime, state, pending, return_to, execution_budget);
    }
    let StoredValue::Function(next) = state.next.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "Array.from iterator advance has no retained next method",
    })?
    else {
        return array_static_type_error(state.realm, state.origin, "not a function");
    };
    execution_budget.charge_instructions(1)?;
    let receiver = state
        .iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Array.from iterator advance has no iterator",
        })?
        .duplicate();
    let function = *next;
    state.stage = ArrayStaticStage::AwaitNextResult;
    call_array_static_function(state, function, receiver, Vec::new(), None, return_to)
}

fn define_array_static_element(
    runtime: &mut Runtime,
    state: &mut ArrayStaticContinuation,
    value: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<Option<PendingException>, NativeFailure> {
    let key = array_static_index_key(runtime, state.index)?;
    let target = array_static_target(state)?;
    charge_heap_property_lookup(runtime, target, execution_budget)?;
    match define_static_property(runtime, target, key, value, execution_budget)? {
        PropertyWriteOutcome::Complete => {
            state.index = state.index.saturating_add(1);
            Ok(None)
        }
        PropertyWriteOutcome::Failed(failure) => {
            let pending = property_exception_at(state.realm, state.origin.clone(), None, failure)?;
            Ok(Some(pending))
        }
        PropertyWriteOutcome::Setter { .. } => Err(EngineFault::RuntimeInvariant {
            message: "CreateDataPropertyOrThrow attempted to call a setter",
        }
        .into()),
    }
}

fn finish_array_static_length(
    runtime: &mut Runtime,
    mut state: ArrayStaticContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = array_static_target(&state)?.duplicate();
    let key = runtime.predefined_property_key(PredefinedAtom::Length);
    let name = JsString::from_utf8("length")?;
    if let StoredValue::Object(object) = target
        && runtime.is_array_object(object)?
    {
        let length = u32::try_from(state.length).map_err(|_| {
            NativeFailure::Abrupt(
                array_static_exception(
                    state.realm,
                    state.origin.clone(),
                    ExceptionKind::RangeError,
                    "invalid array length",
                )
                .expect("static Array error message is valid"),
            )
        })?;
        let work = runtime.preview_array_length_write_work(object, length)?;
        execution_budget.charge_instructions(work)?;
        return match runtime.set_array_length(object, length)? {
            ArrayLengthWriteOutcome::Complete => Ok(NativeDispatch::Immediate(target)),
            ArrayLengthWriteOutcome::ReadOnly => Err(NativeFailure::Abrupt(property_exception_at(
                state.realm,
                state.origin,
                Some(&name),
                PropertyFailure::ReadOnly,
            )?)),
            ArrayLengthWriteOutcome::BlockedByNonConfigurable { .. } => {
                Err(NativeFailure::Abrupt(property_exception_at(
                    state.realm,
                    state.origin,
                    Some(&name),
                    PropertyFailure::NotConfigurable,
                )?))
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
        PropertyWriteOutcome::Complete => Ok(NativeDispatch::Immediate(target)),
        PropertyWriteOutcome::Setter {
            function,
            receiver,
            value,
        } => {
            state.stage = ArrayStaticStage::AwaitLengthSetter;
            call_array_static_function(
                state,
                function,
                receiver,
                array_static_one_value(value)?,
                None,
                return_to,
            )
        }
        PropertyWriteOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            state.realm,
            state.origin,
            Some(&name),
            failure,
        )?)),
    }
}

fn read_array_static_property(
    runtime: &mut Runtime,
    state: ArrayStaticContinuation,
    key: PropertyKey,
    diagnostic_name: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = match state.stage {
        ArrayStaticStage::AwaitIteratorMethod
        | ArrayStaticStage::AwaitArrayLikeLength
        | ArrayStaticStage::AwaitArrayLikeElement => &state.source,
        ArrayStaticStage::AwaitNextMethod => {
            state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Array.from next lookup has no iterator",
                })?
        }
        ArrayStaticStage::AwaitDone | ArrayStaticStage::AwaitIteratorValue => {
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "Array.from result lookup has no iterator result",
            })?
        }
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Array factory call stage attempted a property read",
            }
            .into());
        }
    };
    let base = base.duplicate();
    charge_array_static_lookup(runtime, &base, execution_budget)?;
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
            return resume_array_static_abrupt(
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
        array_static_continuation,
        |state, value| {
            advance_array_static(runtime, state, Some(value), return_to, execution_budget)
        },
        "Array.from Get produced a structured result",
    )
}

fn array_static_continuation(state: ArrayStaticContinuation) -> NativeContinuation {
    NativeContinuation::ArrayStatic(Box::new(state))
}

fn call_array_static_function(
    state: ArrayStaticContinuation,
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
    new_target: Option<FunctionId>,
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
    continuations.push(NativeContinuation::ArrayStatic(Box::new(state)));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target,
        native_caller: None,
    }))
}

fn begin_array_static_close(
    runtime: &mut Runtime,
    state: ArrayStaticContinuation,
    original: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let iterator = state.iterator.ok_or(EngineFault::RuntimeInvariant {
        message: "Array.from IteratorClose started before iterator acquisition",
    })?;
    begin_exceptional_iterator_close(runtime, iterator, original, return_to, execution_budget)
}

pub(super) fn resume_array_static_abrupt(
    runtime: &mut Runtime,
    state: ArrayStaticContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.closes_on_abrupt() {
        let iterator = state.iterator.ok_or(EngineFault::RuntimeInvariant {
            message: "Array.from IteratorClose started before iterator acquisition",
        })?;
        begin_exceptional_iterator_close(runtime, iterator, pending, return_to, execution_budget)
    } else {
        Err(NativeFailure::Abrupt(pending))
    }
}

fn array_static_target(state: &ArrayStaticContinuation) -> Result<&StoredValue, EngineFault> {
    state.target.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "Array factory lost its constructed target",
    })
}

fn require_array_static_object(
    state: &ArrayStaticContinuation,
    value: StoredValue,
) -> Result<StoredValue, NativeFailure> {
    if value.heap_reference().is_some() {
        return Ok(value);
    }
    Err(NativeFailure::Abrupt(array_static_exception(
        state.realm,
        state.origin.clone(),
        ExceptionKind::TypeError,
        "not an object",
    )?))
}

fn take_array_static_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Array factory resumed without a completion",
    })
}

pub(super) fn array_static_index_key(
    runtime: &mut Runtime,
    index: u64,
) -> Result<PropertyKey, NativeFailure> {
    if let Ok(index) = u32::try_from(index)
        && let Some(index) = ArrayIndex::new(index)
    {
        return Ok(PropertyKey::from_index(index));
    }
    let name = JsNumber::from_f64(array_static_index_as_f64(index)).to_javascript_string()?;
    Ok(runtime.property_key_from_string(&name)?)
}

pub(super) fn array_static_mapper_arguments(
    value: StoredValue,
    index: u64,
) -> Result<Vec<StoredValue>, NativeFailure> {
    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(2)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 2,
        })?;
    arguments.push(value);
    arguments.push(StoredValue::Number(JsNumber::from_f64(
        array_static_index_as_f64(index),
    )));
    Ok(arguments)
}

pub(super) fn array_static_single_argument(length: u64) -> Result<Vec<StoredValue>, NativeFailure> {
    array_static_one_value(StoredValue::Number(JsNumber::from_f64(
        array_static_index_as_f64(length),
    )))
}

pub(super) fn array_static_one_value(
    value: StoredValue,
) -> Result<Vec<StoredValue>, NativeFailure> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    values.push(value);
    Ok(values)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "factory indices are rejected at 2^53 - 1, so every converted index is exactly representable in binary64"
)]
pub(super) fn array_static_index_as_f64(index: u64) -> f64 {
    index as f64
}

fn charge_array_static_lookup(
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

fn array_static_type_error<T>(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<T, NativeFailure> {
    Err(NativeFailure::Abrupt(array_static_exception(
        realm,
        origin,
        ExceptionKind::TypeError,
        message,
    )?))
}

fn array_static_exception(
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
