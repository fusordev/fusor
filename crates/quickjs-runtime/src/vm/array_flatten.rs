/*
 * JavaScript Array flattening semantics derived from QuickJS.
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

//! Resumable `Array.prototype.flat`, `flatMap`, and `FlattenIntoArray`.
//!
//! The specification describes flattening recursively. This implementation
//! stores an explicit depth-first frame stack in the traced continuation, so a
//! user-controlled nesting depth cannot consume the Rust call stack. Every
//! observable length read, indexed getter, mapper call, numeric conversion, and
//! species constructor call remains an interpreter suspension boundary.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[derive(Clone, Copy, Debug)]
enum FlattenDepth {
    Finite(u64),
    Infinite,
}

impl FlattenDepth {
    const fn can_descend(self) -> bool {
        matches!(self, Self::Infinite | Self::Finite(1..))
    }

    const fn descended(self) -> Self {
        match self {
            Self::Infinite => Self::Infinite,
            Self::Finite(depth) => Self::Finite(depth.saturating_sub(1)),
        }
    }
}

struct FlattenFrame {
    source: StoredValue,
    length: u64,
    next_index: u64,
    depth: FlattenDepth,
    maps: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayFlattenStage {
    AwaitSourceLength,
    AwaitSourceLengthConversion,
    AwaitDepthConversion,
    SelectSpecies,
    AwaitConstructor,
    AwaitSpecies,
    AwaitSpeciesConstruct,
    NextElement,
    AwaitElementPresence,
    AwaitElement,
    AwaitCallback,
    ProcessElement,
    AwaitNestedLength,
    AwaitNestedLengthConversion,
    Done,
}

/// One in-progress `ArraySpeciesCreate` and `FlattenIntoArray` operation.
pub(crate) struct ArrayFlattenContinuation {
    method: ArrayFlatten,
    source: StoredValue,
    argument: Option<StoredValue>,
    this_arg: StoredValue,
    mapper: Option<FunctionId>,
    destination: Option<StoredValue>,
    frames: Vec<FlattenFrame>,
    element: Option<StoredValue>,
    nested_source: Option<StoredValue>,
    source_length: u64,
    target_index: u64,
    depth: FlattenDepth,
    realm: RealmId,
    stage: ArrayFlattenStage,
    origin: JsStackFrame,
}

impl ArrayFlattenContinuation {
    pub(crate) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.argument.is_some()))
            .saturating_add(u64::from(self.mapper.is_some()))
            .saturating_add(u64::from(self.destination.is_some()))
            .saturating_add(u64::from(self.element.is_some()))
            .saturating_add(u64::from(self.nested_source.is_some()))
            .saturating_add(usize_to_u64(self.frames.len()))
    }

    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.source, mark);
        trace_stored_value_root(&self.this_arg, mark);
        if let Some(argument) = &self.argument {
            trace_stored_value_root(argument, mark);
        }
        if let Some(mapper) = self.mapper {
            mark(CollectionRoot::Heap(HeapReference::Function(mapper)));
        }
        if let Some(destination) = &self.destination {
            trace_stored_value_root(destination, mark);
        }
        if let Some(element) = &self.element {
            trace_stored_value_root(element, mark);
        }
        if let Some(source) = &self.nested_source {
            trace_stored_value_root(source, mark);
        }
        for frame in &self.frames {
            trace_stored_value_root(&frame.source, mark);
        }
    }
}

/// Starts `flat` or `flatMap` with the specification's initial `ToObject`.
#[expect(
    clippy::too_many_arguments,
    reason = "native dispatch supplies the method, realm, call values, return target, origin, and shared budget"
)]
pub(super) fn begin_array_flatten(
    runtime: &mut Runtime,
    method: ArrayFlatten,
    realm: RealmId,
    receiver: StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = match to_object_value(runtime, realm, receiver, origin.clone())? {
        Ok(source) => source,
        Err(exception) => return Err(NativeFailure::Abrupt(exception)),
    };
    let first = arguments.take_first_or_undefined();
    let argument = match method {
        ArrayFlatten::Flat if matches!(first, StoredValue::Undefined) => None,
        ArrayFlatten::Flat | ArrayFlatten::FlatMap => Some(first),
    };
    let this_arg = if method.maps() {
        arguments.take_first_or_undefined()
    } else {
        StoredValue::Undefined
    };
    let state = ArrayFlattenContinuation {
        method,
        source,
        argument,
        this_arg,
        mapper: None,
        destination: None,
        frames: Vec::new(),
        element: None,
        nested_source: None,
        source_length: 0,
        target_index: 0,
        depth: FlattenDepth::Finite(1),
        realm,
        stage: ArrayFlattenStage::AwaitSourceLength,
        origin,
    };
    advance_array_flatten(runtime, state, None, return_to, execution_budget)
}

/// Advances species selection and the explicit depth-first flattening stack.
#[allow(
    clippy::too_many_lines,
    clippy::needless_continue,
    reason = "explicit stages keep species access, conversion, callback, getter, and nested length suspension in specification order"
)]
pub(super) fn advance_array_flatten(
    runtime: &mut Runtime,
    mut state: ArrayFlattenContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    macro_rules! await_get {
        ($operation:expr) => {
            match $operation? {
                GetContinuationDispatch::Ready {
                    state: resumed,
                    value,
                } => {
                    state = resumed;
                    completion = Some(value);
                    continue;
                }
                GetContinuationDispatch::Suspended(dispatch) => return Ok(dispatch),
            }
        };
    }
    loop {
        match state.stage {
            ArrayFlattenStage::AwaitSourceLength => {
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                let source = state.source.duplicate();
                await_get!(begin_flatten_get(
                    runtime,
                    state,
                    &source,
                    key,
                    ArrayFlattenStage::AwaitSourceLengthConversion,
                    return_to,
                    execution_budget,
                ));
            }
            ArrayFlattenStage::AwaitSourceLengthConversion => {
                let value = take_flatten_completion(&mut completion)?;
                if needs_flatten_conversion(&value) {
                    return convert_flatten_value(
                        runtime,
                        state,
                        value,
                        return_to,
                        execution_budget,
                    );
                }
                state.source_length =
                    number_to_length(operator_to_number(value, state.realm, &state.origin)?);
                if state.method.maps() {
                    state.mapper = match state.argument.take() {
                        Some(StoredValue::Function(function)) => Some(function),
                        _ => {
                            return Err(flatten_type_error(
                                state.realm,
                                &state.origin,
                                "not a function",
                            ));
                        }
                    };
                    state.depth = FlattenDepth::Finite(1);
                    state.stage = ArrayFlattenStage::SelectSpecies;
                } else if let Some(depth) = state.argument.take() {
                    completion = Some(depth);
                    state.stage = ArrayFlattenStage::AwaitDepthConversion;
                } else {
                    state.depth = FlattenDepth::Finite(1);
                    state.stage = ArrayFlattenStage::SelectSpecies;
                }
            }
            ArrayFlattenStage::AwaitDepthConversion => {
                let value = take_flatten_completion(&mut completion)?;
                if needs_flatten_conversion(&value) {
                    return convert_flatten_value(
                        runtime,
                        state,
                        value,
                        return_to,
                        execution_budget,
                    );
                }
                let integer = number_to_integer_or_infinity(operator_to_number(
                    value,
                    state.realm,
                    &state.origin,
                )?);
                state.depth = flatten_depth(integer);
                state.stage = ArrayFlattenStage::SelectSpecies;
            }
            ArrayFlattenStage::SelectSpecies => {
                if !is_array_value(runtime, &state.source, state.realm, &state.origin)? {
                    allocate_flatten_destination(runtime, &mut state)?;
                    start_flattening(&mut state)?;
                    continue;
                }
                let key = runtime.predefined_property_key(PredefinedAtom::Constructor);
                let source = state.source.duplicate();
                await_get!(begin_flatten_get(
                    runtime,
                    state,
                    &source,
                    key,
                    ArrayFlattenStage::AwaitConstructor,
                    return_to,
                    execution_budget,
                ));
            }
            ArrayFlattenStage::AwaitConstructor => {
                let constructor = take_flatten_completion(&mut completion)?;
                if let StoredValue::Function(function) = constructor
                    && function_is_constructor(runtime, function)?
                {
                    let constructor_realm = runtime.function_realm(function)?;
                    if constructor_realm != state.realm
                        && function == runtime.realm_array_constructor(constructor_realm)?
                    {
                        allocate_flatten_destination(runtime, &mut state)?;
                        start_flattening(&mut state)?;
                        continue;
                    }
                }
                if matches!(
                    constructor,
                    StoredValue::Function(_) | StoredValue::Object(_)
                ) {
                    let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolSpecies);
                    await_get!(begin_flatten_get(
                        runtime,
                        state,
                        &constructor,
                        key,
                        ArrayFlattenStage::AwaitSpecies,
                        return_to,
                        execution_budget,
                    ));
                } else if matches!(constructor, StoredValue::Undefined) {
                    allocate_flatten_destination(runtime, &mut state)?;
                    start_flattening(&mut state)?;
                } else {
                    return Err(flatten_type_error(
                        state.realm,
                        &state.origin,
                        "not a constructor",
                    ));
                }
            }
            ArrayFlattenStage::AwaitSpecies => {
                let species = take_flatten_completion(&mut completion)?;
                if matches!(species, StoredValue::Undefined | StoredValue::Null) {
                    allocate_flatten_destination(runtime, &mut state)?;
                    start_flattening(&mut state)?;
                    continue;
                }
                let StoredValue::Function(constructor) = species else {
                    return Err(flatten_type_error(
                        state.realm,
                        &state.origin,
                        "not a constructor",
                    ));
                };
                if !function_is_constructor(runtime, constructor)? {
                    return Err(flatten_type_error(
                        state.realm,
                        &state.origin,
                        "not a constructor",
                    ));
                }
                state.stage = ArrayFlattenStage::AwaitSpeciesConstruct;
                return suspend_flatten(
                    state,
                    constructor,
                    StoredValue::Undefined,
                    single_flatten_argument(StoredValue::Number(JsNumber::from_i32(0)))?,
                    Some(constructor),
                    return_to,
                );
            }
            ArrayFlattenStage::AwaitSpeciesConstruct => {
                let destination = take_flatten_completion(&mut completion)?;
                if destination.heap_reference().is_none() {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "ArraySpeciesCreate constructor returned a primitive",
                    }
                    .into());
                }
                state.destination = Some(destination);
                start_flattening(&mut state)?;
            }
            ArrayFlattenStage::NextElement => {
                let Some(frame) = state.frames.last_mut() else {
                    state.stage = ArrayFlattenStage::Done;
                    continue;
                };
                if frame.next_index >= frame.length {
                    state.frames.pop();
                    continue;
                }
                execution_budget.charge_instructions(1)?;
                let index = frame.next_index;
                frame.next_index = frame.next_index.saturating_add(1);
                let source = frame.source.duplicate();
                let key = flatten_element_key(runtime, index)?;
                charge_flatten_lookup(runtime, &source, execution_budget)?;
                state.stage = ArrayFlattenStage::AwaitElementPresence;
                let dispatch = begin_value_has(
                    runtime,
                    &source,
                    key,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                await_get!(continue_get_state_after(
                    dispatch,
                    state,
                    array_flatten_continuation,
                    "FlattenIntoArray HasProperty produced a structured result",
                ));
            }
            ArrayFlattenStage::AwaitElementPresence => {
                if !take_flatten_completion(&mut completion)?.is_truthy() {
                    state.stage = ArrayFlattenStage::NextElement;
                    continue;
                }
                let frame = state.frames.last().ok_or(EngineFault::RuntimeInvariant {
                    message: "FlattenIntoArray lost its source after HasProperty",
                })?;
                let index = frame.next_index.saturating_sub(1);
                let source = frame.source.duplicate();
                let key = flatten_element_key(runtime, index)?;
                await_get!(begin_flatten_get(
                    runtime,
                    state,
                    &source,
                    key,
                    ArrayFlattenStage::AwaitElement,
                    return_to,
                    execution_budget,
                ));
            }
            ArrayFlattenStage::AwaitElement => {
                if state.element.is_none() {
                    state.element = Some(take_flatten_completion(&mut completion)?);
                }
                let frame = state.frames.last().ok_or(EngineFault::RuntimeInvariant {
                    message: "FlattenIntoArray lost the source frame for an element",
                })?;
                if frame.maps {
                    let mapper = state.mapper.ok_or(EngineFault::RuntimeInvariant {
                        message: "flatMap lost its validated mapper",
                    })?;
                    let index = frame.next_index.saturating_sub(1);
                    let mut arguments = Vec::new();
                    arguments.try_reserve_exact(3).map_err(|_| {
                        ExecutionError::AllocationFailed {
                            resource: RuntimeResource::Frames,
                            additional: 3,
                        }
                    })?;
                    arguments.push(
                        state
                            .element
                            .as_ref()
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "flatMap lost the callback element",
                            })?
                            .duplicate(),
                    );
                    arguments.push(StoredValue::Number(JsNumber::from_f64(index_as_f64(index))));
                    arguments.push(frame.source.duplicate());
                    state.stage = ArrayFlattenStage::AwaitCallback;
                    let this_arg = state.this_arg.duplicate();
                    return suspend_flatten(state, mapper, this_arg, arguments, None, return_to);
                }
                state.stage = ArrayFlattenStage::ProcessElement;
            }
            ArrayFlattenStage::AwaitCallback => {
                state.element = Some(take_flatten_completion(&mut completion)?);
                state.stage = ArrayFlattenStage::ProcessElement;
            }
            ArrayFlattenStage::ProcessElement => {
                let element = state.element.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "FlattenIntoArray lost the element being processed",
                })?;
                let depth = state
                    .frames
                    .last()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "FlattenIntoArray lost the active depth",
                    })?
                    .depth;
                if depth.can_descend()
                    && is_array_value(runtime, &element, state.realm, &state.origin)?
                {
                    state.nested_source = Some(element);
                    state.stage = ArrayFlattenStage::AwaitNestedLength;
                    continue;
                }
                create_flattened_element(runtime, &mut state, element, execution_budget)?;
                state.stage = ArrayFlattenStage::NextElement;
            }
            ArrayFlattenStage::AwaitNestedLength => {
                let source = state
                    .nested_source
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "FlattenIntoArray lost its nested source",
                    })?
                    .duplicate();
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                await_get!(begin_flatten_get(
                    runtime,
                    state,
                    &source,
                    key,
                    ArrayFlattenStage::AwaitNestedLengthConversion,
                    return_to,
                    execution_budget,
                ));
            }
            ArrayFlattenStage::AwaitNestedLengthConversion => {
                let value = take_flatten_completion(&mut completion)?;
                if needs_flatten_conversion(&value) {
                    return convert_flatten_value(
                        runtime,
                        state,
                        value,
                        return_to,
                        execution_budget,
                    );
                }
                let length =
                    number_to_length(operator_to_number(value, state.realm, &state.origin)?);
                push_nested_frame(&mut state, length)?;
                state.stage = ArrayFlattenStage::NextElement;
            }
            ArrayFlattenStage::Done => {
                return Ok(NativeDispatch::Immediate(state.destination.take().ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "FlattenIntoArray completed without a destination",
                    },
                )?));
            }
        }
    }
}

fn start_flattening(state: &mut ArrayFlattenContinuation) -> Result<(), NativeFailure> {
    state
        .frames
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    state.frames.push(FlattenFrame {
        source: state.source.duplicate(),
        length: state.source_length,
        next_index: 0,
        depth: state.depth,
        maps: state.method.maps(),
    });
    state.stage = ArrayFlattenStage::NextElement;
    Ok(())
}

fn push_nested_frame(
    state: &mut ArrayFlattenContinuation,
    length: u64,
) -> Result<(), NativeFailure> {
    let source = state
        .nested_source
        .take()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "FlattenIntoArray lost the nested source before descent",
        })?;
    let depth = state
        .frames
        .last()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "FlattenIntoArray lost the parent frame before descent",
        })?
        .depth
        .descended();
    state
        .frames
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    state.frames.push(FlattenFrame {
        source,
        length,
        next_index: 0,
        depth,
        maps: false,
    });
    Ok(())
}

fn allocate_flatten_destination(
    runtime: &mut Runtime,
    state: &mut ArrayFlattenContinuation,
) -> Result<(), NativeFailure> {
    let prototype = runtime.realm_array_prototype(state.realm)?;
    let destination =
        runtime.allocate_sparse_array_with_prototype(HeapReference::Object(prototype), 0)?;
    state.destination = Some(StoredValue::Object(destination));
    Ok(())
}

fn create_flattened_element(
    runtime: &mut Runtime,
    state: &mut ArrayFlattenContinuation,
    element: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    if state.target_index >= MAX_SAFE_INTEGER {
        return Err(flatten_type_error(
            state.realm,
            &state.origin,
            "array too long",
        ));
    }
    let key = flatten_element_key(runtime, state.target_index)?;
    let destination = state
        .destination
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "FlattenIntoArray lost its destination",
        })?;
    match define_static_property(runtime, destination, key, element, execution_budget)? {
        PropertyWriteOutcome::Complete => {
            state.target_index = state.target_index.saturating_add(1);
            Ok(())
        }
        PropertyWriteOutcome::Failed(failure) => Err(flatten_property_failure(state, failure)),
        PropertyWriteOutcome::Setter { .. } => Err(EngineFault::RuntimeInvariant {
            message: "CreateDataPropertyOrThrow attempted to call a setter",
        }
        .into()),
    }
}

fn is_array_value(
    runtime: &Runtime,
    value: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<bool, NativeFailure> {
    proxy_aware_is_array(runtime, value.duplicate(), realm, origin.clone())
}

fn convert_flatten_value(
    runtime: &mut Runtime,
    state: ArrayFlattenContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::ArrayFlattenValue(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn array_flatten_continuation(state: ArrayFlattenContinuation) -> NativeContinuation {
    NativeContinuation::ArrayFlatten(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    reason = "one reusable flattening Get boundary carries its source, next stage, caller continuation, and execution authority"
)]
fn begin_flatten_get(
    runtime: &mut Runtime,
    mut state: ArrayFlattenContinuation,
    base: &StoredValue,
    key: PropertyKey,
    next_stage: ArrayFlattenStage,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<GetContinuationDispatch<ArrayFlattenContinuation>, NativeFailure> {
    charge_flatten_lookup(runtime, base, execution_budget)?;
    state.stage = next_stage;
    let dispatch = begin_value_get(
        runtime,
        base,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_state_after(
        dispatch,
        state,
        array_flatten_continuation,
        "array flatten Get produced a structured result",
    )
}

fn suspend_flatten(
    state: ArrayFlattenContinuation,
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
    continuations.push(NativeContinuation::ArrayFlatten(Box::new(state)));
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

fn single_flatten_argument(value: StoredValue) -> Result<Vec<StoredValue>, NativeFailure> {
    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    arguments.push(value);
    Ok(arguments)
}

fn flatten_element_key(runtime: &mut Runtime, index: u64) -> Result<PropertyKey, NativeFailure> {
    if let Ok(index) = u32::try_from(index)
        && let Some(index) = ArrayIndex::new(index)
    {
        return Ok(PropertyKey::from_index(index));
    }
    let name = JsNumber::from_f64(index_as_f64(index)).to_javascript_string()?;
    Ok(runtime.property_key_from_string(&name)?)
}

fn charge_flatten_lookup(
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

const fn needs_flatten_conversion(value: &StoredValue) -> bool {
    matches!(value, StoredValue::Function(_) | StoredValue::Object(_))
}

fn flatten_depth(integer: f64) -> FlattenDepth {
    if integer.is_infinite() && integer.is_sign_positive() {
        return FlattenDepth::Infinite;
    }
    if integer <= 0.0 {
        return FlattenDepth::Finite(0);
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "positive finite depths above u64::MAX saturate and are indistinguishable for a resource-bounded explicit frame stack"
    )]
    let depth = integer as u64;
    FlattenDepth::Finite(depth)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "ToLength and target-index checks bound every index by 2^53 - 1, which binary64 represents exactly"
)]
fn index_as_f64(index: u64) -> f64 {
    index as f64
}

fn take_flatten_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        EngineFault::RuntimeInvariant {
            message: "array flatten resumed without its awaited completion",
        }
        .into()
    })
}

fn flatten_property_failure(
    state: &ArrayFlattenContinuation,
    failure: PropertyFailure,
) -> NativeFailure {
    match property_exception_at(state.realm, state.origin.clone(), None, failure) {
        Ok(exception) => NativeFailure::Abrupt(exception),
        Err(error) => error.into(),
    }
}

fn flatten_type_error(realm: RealmId, origin: &JsStackFrame, message: &str) -> NativeFailure {
    match JsString::from_utf8(message) {
        Ok(message) => NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message,
            },
            origin: origin.clone(),
        }),
        Err(error) => error.into(),
    }
}
