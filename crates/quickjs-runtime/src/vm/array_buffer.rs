/*
 * ArrayBuffer semantics derived from ECMA-262 and QuickJS.
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

//! Resumable `%ArrayBuffer%` construction and branded prototype operations.
//!
//! The constructor deliberately retains its `newTarget` and options across the
//! separate `ToIndex(length)`, `Get(options, "maxByteLength")`, and optional
//! `ToIndex(maxByteLength)` boundaries.  In particular, a user getter cannot
//! observe the prototype lookup before the options conversion completes.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) struct ArrayBufferConstructorLengthContinuation {
    new_target: FunctionId,
    options: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

pub(super) struct ArrayBufferConstructorContinuation {
    new_target: FunctionId,
    byte_length: usize,
    realm: RealmId,
    origin: JsStackFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayBufferSliceStage {
    Constructor,
    Species,
    Construct,
}

pub(super) struct ArrayBufferSliceContinuation {
    object: ObjectId,
    initial_length: usize,
    end: StoredValue,
    first: usize,
    new_length: usize,
    realm: RealmId,
    stage: ArrayBufferSliceStage,
    origin: JsStackFrame,
}

impl ArrayBufferConstructorLengthContinuation {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
        trace_stored_value_root(&self.options, mark);
    }
}

impl ArrayBufferConstructorContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
    }
}

impl ArrayBufferSliceContinuation {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.object)));
        trace_stored_value_root(&self.end, mark);
    }
}

pub(super) fn begin_array_buffer_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = inputs.new_target else {
        return array_buffer_type_error(realm, &origin, "ArrayBuffer constructor requires 'new'");
    };
    let mut arguments = inputs.arguments;
    let length = arguments.take_first_or_undefined();
    let options = arguments.take_first_or_undefined();
    begin_operator_primitive_conversion(
        runtime,
        length,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::ArrayBufferConstructorLength(Box::new(
            ArrayBufferConstructorLengthContinuation {
                new_target,
                options,
                realm,
                origin: origin.clone(),
            },
        )),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_array_buffer_constructor_length(
    runtime: &mut Runtime,
    state: ArrayBufferConstructorLengthContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let byte_length = array_buffer_to_index(value, state.realm, &state.origin)?;
    if !matches!(state.options, StoredValue::Object(_)) {
        return begin_array_buffer_constructor_wrapper(
            runtime,
            state.realm,
            state.new_target,
            byte_length,
            None,
            return_to,
            state.origin,
            execution_budget,
        );
    }
    let options = state.options.duplicate();
    begin_array_buffer_max_byte_length_get(
        runtime,
        ArrayBufferConstructorContinuation {
            new_target: state.new_target,
            byte_length,
            realm: state.realm,
            origin: state.origin,
        },
        &options,
        return_to,
        execution_budget,
    )
}

fn begin_array_buffer_max_byte_length_get(
    runtime: &mut Runtime,
    state: ArrayBufferConstructorContinuation,
    options: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    debug_assert!(matches!(options, StoredValue::Object(_)));
    charge_heap_property_lookup(runtime, options, execution_budget)?;
    let name = JsString::from_utf8("maxByteLength")?;
    let key = runtime.predefined_property_key(PredefinedAtom::MaxByteLength);
    let dispatch = begin_value_get(
        runtime,
        options,
        key,
        Some(&name),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        array_buffer_constructor_continuation,
        |state, value| {
            advance_array_buffer_constructor_max(runtime, state, value, return_to, execution_budget)
        },
        "ArrayBuffer maxByteLength Get produced a structured result",
    )
}

pub(super) fn advance_array_buffer_constructor_max(
    runtime: &mut Runtime,
    state: ArrayBufferConstructorContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(value, StoredValue::Undefined) {
        return begin_array_buffer_constructor_wrapper(
            runtime,
            state.realm,
            state.new_target,
            state.byte_length,
            None,
            return_to,
            state.origin,
            execution_budget,
        );
    }
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::ArrayBufferConstructorMax {
            new_target: state.new_target,
            byte_length: state.byte_length,
        },
        state.realm,
        return_to,
        state.origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the native conversion continuation preserves construction state and caller return state"
)]
pub(super) fn finish_array_buffer_constructor_max(
    runtime: &mut Runtime,
    new_target: FunctionId,
    byte_length: usize,
    value: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let max_byte_length = array_buffer_to_index(value, realm, origin)?;
    if byte_length > max_byte_length {
        return array_buffer_range_error(realm, origin, "ArrayBuffer length exceeds maxByteLength");
    }
    begin_array_buffer_constructor_wrapper(
        runtime,
        realm,
        new_target,
        byte_length,
        Some(max_byte_length),
        return_to,
        origin.clone(),
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the wrapper retains construction state across an observable prototype Get"
)]
fn begin_array_buffer_constructor_wrapper(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: FunctionId,
    byte_length: usize,
    max_byte_length: Option<usize>,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    begin_intrinsic_get(
        runtime,
        realm,
        HeapReference::Function(new_target),
        StoredValue::Function(new_target),
        &prototype_key,
        IntrinsicGetContinuation::ArrayBufferConstructor {
            new_target,
            byte_length,
            max_byte_length,
        },
        return_to,
        Some(origin),
        execution_budget,
    )
}

pub(super) fn finish_array_buffer_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    byte_length: usize,
    max_byte_length: Option<usize>,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_array_buffer_prototype(realm)?)
        }
    };
    let object = runtime
        .allocate_array_buffer(prototype, byte_length, max_byte_length)
        .map_err(NativeFailure::Execution)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(super) fn array_buffer_is_view(mut arguments: CallArguments) -> NativeDispatch {
    let _ = arguments.take_first_or_undefined();
    // No view exotic has been allocated before DataView and typed-array
    // support arrives.  Returning false here is the exact current predicate.
    NativeDispatch::Immediate(StoredValue::Boolean(false))
}

#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch supplies the method, realm, receiver, arguments, source origin, and shared budget"
)]
pub(super) fn dispatch_array_buffer_prototype(
    runtime: &mut Runtime,
    method: ArrayBufferPrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (object, detached, byte_length, max_byte_length, resizable) =
        array_buffer_receiver_state(runtime, receiver, realm, &origin)?;
    match method {
        ArrayBufferPrototypeMethod::ByteLength => Ok(NativeDispatch::Immediate(
            StoredValue::Number(JsNumber::from_f64(array_buffer_length_as_f64(byte_length))),
        )),
        ArrayBufferPrototypeMethod::Detached => {
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(detached)))
        }
        ArrayBufferPrototypeMethod::MaxByteLength => Ok(NativeDispatch::Immediate(
            StoredValue::Number(JsNumber::from_f64(array_buffer_length_as_f64(
                if detached { 0 } else { max_byte_length },
            ))),
        )),
        ArrayBufferPrototypeMethod::Resizable => {
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(resizable)))
        }
        ArrayBufferPrototypeMethod::Resize => {
            if !resizable {
                return array_buffer_type_error(realm, &origin, "ArrayBuffer is not resizable");
            }
            begin_operator_primitive_conversion(
                runtime,
                arguments.take_first_or_undefined(),
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::ArrayBufferResize { object },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        ArrayBufferPrototypeMethod::Transfer
        | ArrayBufferPrototypeMethod::TransferToFixedLength => {
            let preserve_resizability = method == ArrayBufferPrototypeMethod::Transfer;
            let requested = arguments.take_first_or_undefined();
            if matches!(requested, StoredValue::Undefined) {
                return finish_array_buffer_transfer(
                    runtime,
                    object,
                    preserve_resizability,
                    StoredValue::Number(JsNumber::from_f64(array_buffer_length_as_f64(
                        byte_length,
                    ))),
                    realm,
                    &origin,
                );
            }
            begin_operator_primitive_conversion(
                runtime,
                requested,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::ArrayBufferTransfer {
                    object,
                    preserve_resizability,
                },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        ArrayBufferPrototypeMethod::Slice => {
            if detached {
                return array_buffer_type_error(realm, &origin, "ArrayBuffer is detached");
            }
            begin_array_buffer_slice(
                runtime,
                object,
                byte_length,
                arguments.take_first_or_undefined(),
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "ArrayBuffer slice retains both user arguments and the original length across its observable conversion and species boundaries"
)]
fn begin_array_buffer_slice(
    runtime: &mut Runtime,
    object: ObjectId,
    initial_length: usize,
    start: StoredValue,
    end: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_operator_primitive_conversion(
        runtime,
        start,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::ArrayBufferSliceStart(Box::new(ArrayBufferSliceContinuation {
            object,
            initial_length,
            end,
            first: 0,
            new_length: 0,
            realm,
            stage: ArrayBufferSliceStage::Constructor,
            origin: origin.clone(),
        })),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_array_buffer_slice_start(
    runtime: &mut Runtime,
    mut state: ArrayBufferSliceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.first =
        array_buffer_to_clamped_index(value, state.initial_length, state.realm, &state.origin)?;
    if matches!(state.end, StoredValue::Undefined) {
        state.new_length = state.initial_length.saturating_sub(state.first);
        return begin_array_buffer_slice_constructor_get(
            runtime,
            state,
            return_to,
            execution_budget,
        );
    }
    let end = state.end.duplicate();
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        end,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::ArrayBufferSliceEnd(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_array_buffer_slice_end(
    runtime: &mut Runtime,
    mut state: ArrayBufferSliceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let final_index =
        array_buffer_to_clamped_index(value, state.initial_length, state.realm, &state.origin)?;
    state.new_length = final_index.saturating_sub(state.first);
    begin_array_buffer_slice_constructor_get(runtime, state, return_to, execution_budget)
}

fn begin_array_buffer_slice_constructor_get(
    runtime: &mut Runtime,
    mut state: ArrayBufferSliceContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = ArrayBufferSliceStage::Constructor;
    let receiver = StoredValue::Object(state.object);
    charge_heap_property_lookup(runtime, &receiver, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::Constructor);
    let dispatch = begin_value_get(
        runtime,
        &receiver,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        array_buffer_slice_continuation,
        |state, value| {
            advance_array_buffer_slice(runtime, state, value, return_to, execution_budget)
        },
        "ArrayBuffer slice constructor Get produced a structured result",
    )
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the shared resumable Get continuation passes its completion value by value"
)]
pub(super) fn advance_array_buffer_slice(
    runtime: &mut Runtime,
    mut state: ArrayBufferSliceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ArrayBufferSliceStage::Constructor => {
            if let StoredValue::Function(function) = value
                && function_is_constructor(runtime, function)?
            {
                let function_realm = runtime.function_realm(function)?;
                if function_realm != state.realm
                    && function == runtime.realm_array_buffer_constructor(function_realm)?
                {
                    let constructor = runtime.realm_array_buffer_constructor(state.realm)?;
                    return begin_array_buffer_slice_construct(state, constructor, return_to);
                }
            }
            if matches!(value, StoredValue::Undefined) {
                let constructor = runtime.realm_array_buffer_constructor(state.realm)?;
                return begin_array_buffer_slice_construct(state, constructor, return_to);
            }
            if !matches!(value, StoredValue::Object(_) | StoredValue::Function(_)) {
                return array_buffer_type_error(state.realm, &state.origin, "not a constructor");
            }
            state.stage = ArrayBufferSliceStage::Species;
            begin_array_buffer_slice_get_species(
                runtime,
                state,
                &value,
                return_to,
                execution_budget,
            )
        }
        ArrayBufferSliceStage::Species => {
            let constructor = if matches!(value, StoredValue::Undefined | StoredValue::Null) {
                runtime.realm_array_buffer_constructor(state.realm)?
            } else if let StoredValue::Function(function) = value {
                if !function_is_constructor(runtime, function)? {
                    return array_buffer_type_error(
                        state.realm,
                        &state.origin,
                        "not a constructor",
                    );
                }
                function
            } else {
                return array_buffer_type_error(state.realm, &state.origin, "not a constructor");
            };
            begin_array_buffer_slice_construct(state, constructor, return_to)
        }
        ArrayBufferSliceStage::Construct => {
            let StoredValue::Object(target) = value else {
                return array_buffer_type_error(
                    state.realm,
                    &state.origin,
                    "ArrayBuffer species constructor returned a primitive",
                );
            };
            if target == state.object {
                return array_buffer_type_error(
                    state.realm,
                    &state.origin,
                    "ArrayBuffer species constructor returned its source",
                );
            }
            let Some(target_state) = runtime.array_buffer_state(target)? else {
                return array_buffer_type_error(
                    state.realm,
                    &state.origin,
                    "ArrayBuffer species constructor returned a non-ArrayBuffer",
                );
            };
            if target_state.is_detached() || target_state.byte_length() < state.new_length {
                return array_buffer_type_error(
                    state.realm,
                    &state.origin,
                    "ArrayBuffer species constructor returned an invalid buffer",
                );
            }
            let Some(source_state) = runtime.array_buffer_state(state.object)? else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "ArrayBuffer slice source lost its internal slots",
                }
                .into());
            };
            if source_state.is_detached() {
                return array_buffer_type_error(
                    state.realm,
                    &state.origin,
                    "ArrayBuffer is detached",
                );
            }
            let count = state
                .new_length
                .min(source_state.byte_length().saturating_sub(state.first));
            runtime
                .copy_array_buffer_bytes(state.object, state.first, target, count)
                .map_err(NativeFailure::Execution)?;
            Ok(NativeDispatch::Immediate(StoredValue::Object(target)))
        }
    }
}

fn begin_array_buffer_slice_get_species(
    runtime: &mut Runtime,
    state: ArrayBufferSliceContinuation,
    constructor: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_heap_property_lookup(runtime, constructor, execution_budget)?;
    let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolSpecies);
    let dispatch = begin_value_get(
        runtime,
        constructor,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        array_buffer_slice_continuation,
        |state, value| {
            advance_array_buffer_slice(runtime, state, value, return_to, execution_budget)
        },
        "ArrayBuffer slice species Get produced a structured result",
    )
}

fn begin_array_buffer_slice_construct(
    mut state: ArrayBufferSliceContinuation,
    constructor: FunctionId,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = ArrayBufferSliceStage::Construct;
    let origin = state.origin.clone();
    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 1,
        })?;
    arguments.push(StoredValue::Number(JsNumber::from_f64(
        array_buffer_length_as_f64(state.new_length),
    )));
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(array_buffer_slice_continuation(state));
    Ok(NativeDispatch::Call(NativeCall {
        function: constructor,
        receiver: StoredValue::Undefined,
        arguments: CallArguments::from_values(arguments),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: Some(constructor),
        native_caller: None,
    }))
}

fn array_buffer_slice_continuation(state: ArrayBufferSliceContinuation) -> NativeContinuation {
    NativeContinuation::ArrayBufferSlice(Box::new(state))
}

fn array_buffer_to_clamped_index(
    value: StoredValue,
    length: usize,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<usize, NativeFailure> {
    let integer = number_to_integer_or_infinity(operator_to_number(value, realm, origin)?);
    if integer.is_sign_negative() {
        if integer.is_infinite() {
            return Ok(0);
        }
        let relative = integer.abs();
        if relative >= array_buffer_length_as_f64(length) {
            return Ok(0);
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the prior bound proves the finite relative index is inside usize"
        )]
        return Ok(length.saturating_sub(relative as usize));
    }
    if integer.is_infinite() {
        return Ok(length);
    }
    if integer >= array_buffer_length_as_f64(length) {
        return Ok(length);
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the prior bounds prove the finite clamped index is inside usize"
    )]
    Ok(integer as usize)
}

/// Converts a `ToIndex` result to the binary64 `Number` representation.
///
/// `ToIndex` admits at most `2^53 - 1`, which is represented exactly by an
/// ECMAScript Number on every supported pointer width.
#[expect(
    clippy::cast_precision_loss,
    reason = "ToIndex bounds array buffer lengths by 2^53 - 1, exactly representable in binary64"
)]
fn array_buffer_length_as_f64(length: usize) -> f64 {
    length as f64
}

pub(super) fn finish_array_buffer_resize(
    runtime: &mut Runtime,
    object: ObjectId,
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let new_byte_length = array_buffer_to_index(value, realm, origin)?;
    let Some(state) = runtime.array_buffer_state(object)? else {
        return Err(EngineFault::RuntimeInvariant {
            message: "ArrayBuffer resize target lost its internal slots",
        }
        .into());
    };
    if state.is_detached() || !state.is_resizable() {
        return array_buffer_type_error(realm, origin, "ArrayBuffer is not resizable");
    }
    let maximum = state
        .resizable_max_byte_length()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "resizable ArrayBuffer lost its maxByteLength",
        })?;
    if new_byte_length > maximum {
        return array_buffer_range_error(realm, origin, "ArrayBuffer length exceeds maxByteLength");
    }
    runtime
        .resize_array_buffer(object, new_byte_length)
        .map_err(NativeFailure::Execution)?;
    Ok(NativeDispatch::Immediate(StoredValue::Undefined))
}

pub(super) fn finish_array_buffer_transfer(
    runtime: &mut Runtime,
    object: ObjectId,
    preserve_resizability: bool,
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let new_byte_length = array_buffer_to_index(value, realm, origin)?;
    let Some(state) = runtime.array_buffer_state(object)? else {
        return Err(EngineFault::RuntimeInvariant {
            message: "ArrayBuffer transfer target lost its internal slots",
        }
        .into());
    };
    if state.is_detached() {
        return array_buffer_type_error(realm, origin, "ArrayBuffer is detached");
    }
    let max_byte_length = if preserve_resizability {
        state.resizable_max_byte_length()
    } else {
        None
    };
    if max_byte_length.is_some_and(|maximum| new_byte_length > maximum) {
        return array_buffer_range_error(realm, origin, "ArrayBuffer length exceeds maxByteLength");
    }
    let prototype = HeapReference::Object(runtime.realm_array_buffer_prototype(realm)?);
    let target = runtime
        .transfer_array_buffer(object, prototype, new_byte_length, preserve_resizability)
        .map_err(NativeFailure::Execution)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(target)))
}

fn array_buffer_receiver_state(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<(ObjectId, bool, usize, usize, bool), NativeFailure> {
    let StoredValue::Object(object) = receiver else {
        return array_buffer_type_error(realm, origin, "not an ArrayBuffer");
    };
    let Some(state) = runtime.array_buffer_state(*object)? else {
        return array_buffer_type_error(realm, origin, "not an ArrayBuffer");
    };
    Ok((
        *object,
        state.is_detached(),
        state.byte_length(),
        state.max_byte_length(),
        state.is_resizable(),
    ))
}

fn array_buffer_to_index(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<usize, NativeFailure> {
    let number = operator_to_number(value, realm, origin)?;
    let Some(index) = number_to_index(number) else {
        return array_buffer_range_error(realm, origin, "invalid ArrayBuffer length");
    };
    usize::try_from(index).map_err(|_| {
        NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("invalid ArrayBuffer length")
                    .expect("static ArrayBuffer length message is valid UTF-8"),
            },
            origin: origin.clone(),
        })
    })
}

fn array_buffer_constructor_continuation(
    state: ArrayBufferConstructorContinuation,
) -> NativeContinuation {
    NativeContinuation::ArrayBufferConstructor(Box::new(state))
}

fn array_buffer_type_error<T>(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<T, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    }))
}

fn array_buffer_range_error<T>(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<T, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::RangeError,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    }))
}
