/*
 * DataView semantics derived from ECMA-262 and QuickJS.
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

//! Resumable `%DataView%` construction and binary view access.
//!
//! Every user-coercible argument is kept in an explicit continuation.  In
//! particular, `set*` converts `byteOffset`, then `value`, and only then takes
//! the buffer witness and checks bounds.  That ordering lets a user conversion
//! resize or detach a resizable backing `ArrayBuffer` exactly where ECMA-262
//! permits it.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) struct DataViewConstructorOffsetState {
    new_target: FunctionId,
    buffer: ObjectId,
    byte_length: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

pub(super) struct DataViewConstructorByteLengthState {
    new_target: FunctionId,
    buffer: ObjectId,
    byte_offset: usize,
    realm: RealmId,
    origin: JsStackFrame,
}

pub(super) struct DataViewConstructorState {
    new_target: FunctionId,
    buffer: ObjectId,
    byte_offset: usize,
    byte_length: DataViewByteLength,
    realm: RealmId,
    origin: JsStackFrame,
}

pub(super) struct DataViewGetState {
    view: ObjectId,
    element: DataViewElementType,
    little_endian: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

pub(super) struct DataViewSetOffsetState {
    view: ObjectId,
    element: DataViewElementType,
    value: StoredValue,
    little_endian: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

pub(super) struct DataViewSetValueState {
    view: ObjectId,
    element: DataViewElementType,
    byte_index: usize,
    little_endian: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

impl DataViewConstructorOffsetState {
    pub(super) const fn retained_values() -> u64 {
        3
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
        mark(CollectionRoot::Heap(HeapReference::Object(self.buffer)));
        trace_stored_value_root(&self.byte_length, mark);
    }
}

impl DataViewConstructorByteLengthState {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
        mark(CollectionRoot::Heap(HeapReference::Object(self.buffer)));
    }
}

impl DataViewConstructorState {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
        mark(CollectionRoot::Heap(HeapReference::Object(self.buffer)));
    }
}

impl DataViewGetState {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.view)));
        trace_stored_value_root(&self.little_endian, mark);
    }
}

impl DataViewSetOffsetState {
    pub(super) const fn retained_values() -> u64 {
        3
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.view)));
        trace_stored_value_root(&self.value, mark);
        trace_stored_value_root(&self.little_endian, mark);
    }
}

impl DataViewSetValueState {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.view)));
        trace_stored_value_root(&self.little_endian, mark);
    }
}

pub(super) fn begin_data_view_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = inputs.new_target else {
        return data_view_type_error(realm, &origin, "DataView constructor requires 'new'");
    };
    let mut arguments = inputs.arguments;
    let buffer = arguments.take_first_or_undefined();
    let byte_offset = arguments.take_first_or_undefined();
    let byte_length = arguments.take_first_or_undefined();
    let StoredValue::Object(buffer) = buffer else {
        return data_view_type_error(realm, &origin, "DataView buffer is not an ArrayBuffer");
    };
    if runtime.array_buffer_state(buffer)?.is_none() {
        return data_view_type_error(realm, &origin, "DataView buffer is not an ArrayBuffer");
    }
    begin_operator_primitive_conversion(
        runtime,
        byte_offset,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::DataViewConstructorOffset(Box::new(
            DataViewConstructorOffsetState {
                new_target,
                buffer,
                byte_length,
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

pub(super) fn finish_data_view_constructor_offset(
    runtime: &mut Runtime,
    state: DataViewConstructorOffsetState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let byte_offset = data_view_to_index(value, state.realm, &state.origin)?;
    let (buffer_byte_length, resizable) =
        data_view_buffer_length(runtime, state.buffer, state.realm, &state.origin)?;
    if byte_offset > buffer_byte_length {
        return data_view_range_error(
            state.realm,
            &state.origin,
            "DataView offset is outside buffer",
        );
    }
    if matches!(state.byte_length, StoredValue::Undefined) {
        let byte_length = if resizable {
            DataViewByteLength::Auto
        } else {
            DataViewByteLength::Fixed(buffer_byte_length.saturating_sub(byte_offset))
        };
        return begin_data_view_constructor_prototype_get(
            runtime,
            DataViewConstructorState {
                new_target: state.new_target,
                buffer: state.buffer,
                byte_offset,
                byte_length,
                realm: state.realm,
                origin: state.origin,
            },
            return_to,
            execution_budget,
        );
    }
    let byte_length = state.byte_length.duplicate();
    begin_operator_primitive_conversion(
        runtime,
        byte_length,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::DataViewConstructorByteLength(Box::new(
            DataViewConstructorByteLengthState {
                new_target: state.new_target,
                buffer: state.buffer,
                byte_offset,
                realm: state.realm,
                origin: state.origin.clone(),
            },
        )),
        state.realm,
        return_to,
        state.origin,
        execution_budget,
    )
}

pub(super) fn finish_data_view_constructor_byte_length(
    runtime: &mut Runtime,
    state: DataViewConstructorByteLengthState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let byte_length = data_view_to_index(value, state.realm, &state.origin)?;
    let (buffer_byte_length, _) =
        data_view_buffer_length(runtime, state.buffer, state.realm, &state.origin)?;
    if state
        .byte_offset
        .checked_add(byte_length)
        .is_none_or(|end| end > buffer_byte_length)
    {
        return data_view_range_error(
            state.realm,
            &state.origin,
            "DataView length is outside buffer",
        );
    }
    begin_data_view_constructor_prototype_get(
        runtime,
        DataViewConstructorState {
            new_target: state.new_target,
            buffer: state.buffer,
            byte_offset: state.byte_offset,
            byte_length: DataViewByteLength::Fixed(byte_length),
            realm: state.realm,
            origin: state.origin,
        },
        return_to,
        execution_budget,
    )
}

fn begin_data_view_constructor_prototype_get(
    runtime: &mut Runtime,
    state: DataViewConstructorState,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let receiver = StoredValue::Function(state.new_target);
    charge_heap_property_lookup(runtime, &receiver, execution_budget)?;
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let dispatch = begin_value_get(
        runtime,
        &receiver,
        prototype_key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        data_view_constructor_continuation,
        |state, value| finish_data_view_constructor_prototype(runtime, &state, &value),
        "DataView constructor prototype Get produced a structured result",
    )
}

pub(super) fn finish_data_view_constructor_prototype(
    runtime: &mut Runtime,
    state: &DataViewConstructorState,
    value: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match value {
        StoredValue::Object(object) => HeapReference::Object(*object),
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let realm = runtime.function_realm(state.new_target)?;
            HeapReference::Object(runtime.realm_data_view_prototype(realm)?)
        }
    };
    let (buffer_byte_length, _) =
        data_view_buffer_length(runtime, state.buffer, state.realm, &state.origin)?;
    let end = match state.byte_length {
        DataViewByteLength::Fixed(length) => state.byte_offset.checked_add(length),
        DataViewByteLength::Auto => Some(buffer_byte_length),
    };
    if state.byte_offset > buffer_byte_length || end.is_none_or(|end| end > buffer_byte_length) {
        return data_view_type_error(
            state.realm,
            &state.origin,
            "DataView backing buffer changed",
        );
    }
    let view = runtime
        .allocate_data_view(
            prototype,
            DataViewState::new(state.buffer, state.byte_offset, state.byte_length),
        )
        .map_err(NativeFailure::Execution)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(view)))
}

#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch supplies the method, realm, receiver, arguments, source origin, and shared budget"
)]
pub(super) fn dispatch_data_view_prototype(
    runtime: &mut Runtime,
    method: DataViewPrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let view = data_view_receiver(runtime, receiver, realm, &origin)?;
    match method {
        DataViewPrototypeMethod::Buffer => {
            let state = runtime
                .data_view_state(view)?
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "DataView receiver lost its internal slots",
                })?;
            Ok(NativeDispatch::Immediate(StoredValue::Object(
                state.buffer(),
            )))
        }
        DataViewPrototypeMethod::ByteLength => {
            let state = copied_data_view_state(runtime, view)?;
            let (_, _, byte_length) = data_view_live_bounds(runtime, state, realm, &origin)?;
            Ok(NativeDispatch::Immediate(StoredValue::Number(
                data_view_length_number(byte_length),
            )))
        }
        DataViewPrototypeMethod::ByteOffset => {
            let state = copied_data_view_state(runtime, view)?;
            let (_, byte_offset, _) = data_view_live_bounds(runtime, state, realm, &origin)?;
            Ok(NativeDispatch::Immediate(StoredValue::Number(
                data_view_length_number(byte_offset),
            )))
        }
        _ => {
            let element = method.element_type().ok_or(EngineFault::RuntimeInvariant {
                message: "DataView method has neither accessor nor element type",
            })?;
            let byte_offset = arguments.take_first_or_undefined();
            if method.is_setter() {
                let value = arguments.take_first_or_undefined();
                let little_endian = arguments.take_first_or_undefined();
                begin_operator_primitive_conversion(
                    runtime,
                    byte_offset,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::DataViewSetOffset(Box::new(DataViewSetOffsetState {
                        view,
                        element,
                        value,
                        little_endian,
                        realm,
                        origin: origin.clone(),
                    })),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                )
            } else {
                let little_endian = arguments.take_first_or_undefined();
                begin_operator_primitive_conversion(
                    runtime,
                    byte_offset,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::DataViewGetIndex(Box::new(DataViewGetState {
                        view,
                        element,
                        little_endian,
                        realm,
                        origin: origin.clone(),
                    })),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                )
            }
        }
    }
}

pub(super) fn finish_data_view_get_index(
    runtime: &mut Runtime,
    state: &DataViewGetState,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let byte_index = data_view_to_index(value, state.realm, &state.origin)?;
    let little_endian = data_view_to_boolean(&state.little_endian);
    let view = copied_data_view_state(runtime, state.view)?;
    let (buffer, byte_offset, byte_length) =
        data_view_live_bounds(runtime, view, state.realm, &state.origin)?;
    let buffer_index = data_view_checked_element_index(
        byte_offset,
        byte_length,
        byte_index,
        state.element,
        state.realm,
        &state.origin,
    )?;
    data_view_read(runtime, buffer, buffer_index, state.element, little_endian)
}

pub(super) fn finish_data_view_set_offset(
    runtime: &mut Runtime,
    state: DataViewSetOffsetState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let byte_index = data_view_to_index(value, state.realm, &state.origin)?;
    let value = state.value.duplicate();
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::DataViewSetValue(Box::new(DataViewSetValueState {
            view: state.view,
            element: state.element,
            byte_index,
            little_endian: state.little_endian,
            realm: state.realm,
            origin: state.origin.clone(),
        })),
        state.realm,
        return_to,
        state.origin,
        execution_budget,
    )
}

pub(super) fn finish_data_view_set_value(
    runtime: &mut Runtime,
    state: &DataViewSetValueState,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let number = if state.element.is_bigint() {
        DataViewNumeric::BigInt(to_bigint_from_primitive(
            &value,
            state.realm,
            &state.origin,
        )?)
    } else {
        DataViewNumeric::Number(operator_to_number(value, state.realm, &state.origin)?)
    };
    let little_endian = data_view_to_boolean(&state.little_endian);
    let view = copied_data_view_state(runtime, state.view)?;
    let (buffer, byte_offset, byte_length) =
        data_view_live_bounds(runtime, view, state.realm, &state.origin)?;
    let buffer_index = data_view_checked_element_index(
        byte_offset,
        byte_length,
        state.byte_index,
        state.element,
        state.realm,
        &state.origin,
    )?;
    data_view_write(
        runtime,
        buffer,
        buffer_index,
        state.element,
        number,
        little_endian,
        state.realm,
        &state.origin,
    )
}

fn data_view_constructor_continuation(state: DataViewConstructorState) -> NativeContinuation {
    NativeContinuation::DataViewConstructor(Box::new(state))
}

fn copied_data_view_state(
    runtime: &Runtime,
    view: ObjectId,
) -> Result<DataViewState, NativeFailure> {
    runtime.data_view_state(view)?.copied().ok_or(
        EngineFault::RuntimeInvariant {
            message: "DataView receiver lost its internal slots",
        }
        .into(),
    )
}

fn data_view_receiver(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<ObjectId, NativeFailure> {
    let StoredValue::Object(object) = receiver else {
        return data_view_type_error(realm, origin, "not a DataView");
    };
    if runtime.data_view_state(*object)?.is_none() {
        return data_view_type_error(realm, origin, "not a DataView");
    }
    Ok(*object)
}

fn data_view_buffer_length(
    runtime: &Runtime,
    buffer: ObjectId,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<(usize, bool), NativeFailure> {
    let state = runtime
        .array_buffer_state(buffer)?
        .ok_or(EngineFault::RuntimeInvariant {
            message: "DataView backing buffer lost its ArrayBuffer slots",
        })?;
    if state.is_detached() {
        return data_view_type_error(realm, origin, "DataView backing buffer is detached");
    }
    Ok((state.byte_length(), state.is_resizable()))
}

fn data_view_live_bounds(
    runtime: &Runtime,
    view: DataViewState,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<(ObjectId, usize, usize), NativeFailure> {
    let (buffer_byte_length, _) = data_view_buffer_length(runtime, view.buffer(), realm, origin)?;
    let byte_offset = view.byte_offset();
    let byte_length = match view.byte_length() {
        DataViewByteLength::Auto => buffer_byte_length.checked_sub(byte_offset),
        DataViewByteLength::Fixed(length) => byte_offset
            .checked_add(length)
            .filter(|end| *end <= buffer_byte_length)
            .map(|_| length),
    };
    let Some(byte_length) = byte_length else {
        return data_view_type_error(realm, origin, "DataView is outside its backing buffer");
    };
    Ok((view.buffer(), byte_offset, byte_length))
}

fn data_view_checked_element_index(
    byte_offset: usize,
    byte_length: usize,
    byte_index: usize,
    element: DataViewElementType,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<usize, NativeFailure> {
    let width = element.byte_width();
    if byte_index
        .checked_add(width)
        .is_none_or(|end| end > byte_length)
    {
        return data_view_range_error(realm, origin, "DataView element is outside view bounds");
    }
    byte_offset.checked_add(byte_index).ok_or_else(|| {
        NativeFailure::Abrupt(data_view_range_pending(
            realm,
            origin,
            "DataView byte index overflowed",
        ))
    })
}

fn data_view_read(
    runtime: &Runtime,
    buffer: ObjectId,
    byte_index: usize,
    element: DataViewElementType,
    little_endian: bool,
) -> Result<NativeDispatch, NativeFailure> {
    let data = runtime
        .array_buffer_state(buffer)?
        .and_then(|state| state.data())
        .ok_or(EngineFault::RuntimeInvariant {
            message: "DataView read lost a live backing store",
        })?;
    let value = match element {
        DataViewElementType::BigInt64 => {
            let bytes = data_view_read_bytes::<8>(data, byte_index)?;
            let value = if little_endian {
                i64::from_le_bytes(bytes)
            } else {
                i64::from_be_bytes(bytes)
            };
            bigint_value(JsBigInt::from_i64(value))
        }
        DataViewElementType::BigUint64 => {
            let bytes = data_view_read_bytes::<8>(data, byte_index)?;
            let value = if little_endian {
                u64::from_le_bytes(bytes)
            } else {
                u64::from_be_bytes(bytes)
            };
            bigint_value(JsBigInt::from_u64(value))
        }
        DataViewElementType::Float16 => {
            let bytes = data_view_read_bytes::<2>(data, byte_index)?;
            let bits = if little_endian {
                u16::from_le_bytes(bytes)
            } else {
                u16::from_be_bytes(bytes)
            };
            StoredValue::Number(JsNumber::from_f64(data_view_f16_to_f64(bits)))
        }
        DataViewElementType::Float32 => {
            let bytes = data_view_read_bytes::<4>(data, byte_index)?;
            let bits = if little_endian {
                u32::from_le_bytes(bytes)
            } else {
                u32::from_be_bytes(bytes)
            };
            StoredValue::Number(JsNumber::from_f64(f64::from(f32::from_bits(bits))))
        }
        DataViewElementType::Float64 => {
            let bytes = data_view_read_bytes::<8>(data, byte_index)?;
            let bits = if little_endian {
                u64::from_le_bytes(bytes)
            } else {
                u64::from_be_bytes(bytes)
            };
            StoredValue::Number(JsNumber::from_f64(f64::from_bits(bits)))
        }
        DataViewElementType::Int8 => {
            let byte = data_view_read_bytes::<1>(data, byte_index)?[0];
            StoredValue::Number(JsNumber::from_i32(i32::from(i8::from_ne_bytes([byte]))))
        }
        DataViewElementType::Int16 => {
            let bytes = data_view_read_bytes::<2>(data, byte_index)?;
            let value = if little_endian {
                i16::from_le_bytes(bytes)
            } else {
                i16::from_be_bytes(bytes)
            };
            StoredValue::Number(JsNumber::from_i32(i32::from(value)))
        }
        DataViewElementType::Int32 => {
            let bytes = data_view_read_bytes::<4>(data, byte_index)?;
            let value = if little_endian {
                i32::from_le_bytes(bytes)
            } else {
                i32::from_be_bytes(bytes)
            };
            StoredValue::Number(JsNumber::from_i32(value))
        }
        DataViewElementType::Uint8 => {
            let byte = data_view_read_bytes::<1>(data, byte_index)?[0];
            StoredValue::Number(JsNumber::from_i32(i32::from(byte)))
        }
        DataViewElementType::Uint16 => {
            let bytes = data_view_read_bytes::<2>(data, byte_index)?;
            let value = if little_endian {
                u16::from_le_bytes(bytes)
            } else {
                u16::from_be_bytes(bytes)
            };
            StoredValue::Number(JsNumber::from_i32(i32::from(value)))
        }
        DataViewElementType::Uint32 => {
            let bytes = data_view_read_bytes::<4>(data, byte_index)?;
            let value = if little_endian {
                u32::from_le_bytes(bytes)
            } else {
                u32::from_be_bytes(bytes)
            };
            StoredValue::Number(JsNumber::from_u32(value))
        }
    };
    Ok(NativeDispatch::Immediate(value))
}

enum DataViewNumeric {
    Number(JsNumber),
    BigInt(Arc<JsBigInt>),
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the direct store preserves the already-converted element and explicit witness context"
)]
fn data_view_write(
    runtime: &mut Runtime,
    buffer: ObjectId,
    byte_index: usize,
    element: DataViewElementType,
    value: DataViewNumeric,
    little_endian: bool,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let bytes = match (element, value) {
        (DataViewElementType::BigInt64, DataViewNumeric::BigInt(value)) => {
            let value = match value.as_int_n(64) {
                Ok(value) => value,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(bigint_exception(
                        realm, origin, error,
                    )?));
                }
            }
            .to_i64()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "64-bit signed BigInt truncation did not fit i64",
            })?;
            if little_endian {
                value.to_le_bytes().to_vec()
            } else {
                value.to_be_bytes().to_vec()
            }
        }
        (DataViewElementType::BigUint64, DataViewNumeric::BigInt(value)) => {
            let value = match value.as_uint_n(64) {
                Ok(value) => value,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(bigint_exception(
                        realm, origin, error,
                    )?));
                }
            }
            .to_u64()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "64-bit unsigned BigInt truncation did not fit u64",
            })?;
            if little_endian {
                value.to_le_bytes().to_vec()
            } else {
                value.to_be_bytes().to_vec()
            }
        }
        (DataViewElementType::Float16, DataViewNumeric::Number(value)) => {
            let bits = data_view_f64_to_f16(value.as_f64());
            if little_endian {
                bits.to_le_bytes().to_vec()
            } else {
                bits.to_be_bytes().to_vec()
            }
        }
        (DataViewElementType::Float32, DataViewNumeric::Number(value)) => {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "DataView float32 write intentionally rounds an ECMAScript Number to IEEE binary32"
            )]
            let bits = (value.as_f64() as f32).to_bits();
            if little_endian {
                bits.to_le_bytes().to_vec()
            } else {
                bits.to_be_bytes().to_vec()
            }
        }
        (DataViewElementType::Float64, DataViewNumeric::Number(value)) => {
            let bits = value.as_f64().to_bits();
            if little_endian {
                bits.to_le_bytes().to_vec()
            } else {
                bits.to_be_bytes().to_vec()
            }
        }
        (DataViewElementType::Int8, DataViewNumeric::Number(value)) => {
            vec![number_to_int8(value).to_ne_bytes()[0]]
        }
        (DataViewElementType::Int16, DataViewNumeric::Number(value)) => {
            let value = number_to_int16(value);
            if little_endian {
                value.to_le_bytes().to_vec()
            } else {
                value.to_be_bytes().to_vec()
            }
        }
        (DataViewElementType::Int32, DataViewNumeric::Number(value)) => {
            let value = number_to_int32(value);
            if little_endian {
                value.to_le_bytes().to_vec()
            } else {
                value.to_be_bytes().to_vec()
            }
        }
        (DataViewElementType::Uint8, DataViewNumeric::Number(value)) => {
            vec![number_to_uint8(value)]
        }
        (DataViewElementType::Uint16, DataViewNumeric::Number(value)) => {
            let value = number_to_uint16(value);
            if little_endian {
                value.to_le_bytes().to_vec()
            } else {
                value.to_be_bytes().to_vec()
            }
        }
        (DataViewElementType::Uint32, DataViewNumeric::Number(value)) => {
            let value = number_to_uint32(value);
            if little_endian {
                value.to_le_bytes().to_vec()
            } else {
                value.to_be_bytes().to_vec()
            }
        }
        (element, value) => {
            return Err(EngineFault::RuntimeInvariant {
                message: match (element, value) {
                    (element, DataViewNumeric::Number(_)) if element.is_bigint() => {
                        "DataView BigInt write received a Number"
                    }
                    (element, DataViewNumeric::BigInt(_)) if !element.is_bigint() => {
                        "DataView Number write received a BigInt"
                    }
                    _ => "DataView write reached an impossible element/value pairing",
                },
            }
            .into());
        }
    };
    let state = runtime
        .objects
        .get_mut(buffer)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "DataView write buffer",
            index: buffer.index(),
            generation: buffer.generation(),
        })?
        .array_buffer_state_mut()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "DataView write buffer lost ArrayBuffer slots",
        })?;
    let data = state.data_mut().ok_or(EngineFault::RuntimeInvariant {
        message: "DataView write buffer detached after bounds check",
    })?;
    let end = byte_index
        .checked_add(bytes.len())
        .ok_or(EngineFault::RuntimeInvariant {
            message: "DataView write byte range overflowed",
        })?;
    let target = data
        .get_mut(byte_index..end)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "DataView write escaped validated backing-store bounds",
        })?;
    target.copy_from_slice(&bytes);
    Ok(NativeDispatch::Immediate(StoredValue::Undefined))
}

fn data_view_read_bytes<const WIDTH: usize>(
    data: &[u8],
    byte_index: usize,
) -> Result<[u8; WIDTH], NativeFailure> {
    let end = byte_index
        .checked_add(WIDTH)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "DataView read byte range overflowed",
        })?;
    let source = data
        .get(byte_index..end)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "DataView read escaped validated backing-store bounds",
        })?;
    let mut bytes = [0; WIDTH];
    bytes.copy_from_slice(source);
    Ok(bytes)
}

fn data_view_to_index(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<usize, NativeFailure> {
    let number = operator_to_number(value, realm, origin)?;
    let Some(index) = number_to_index(number) else {
        return data_view_range_error(realm, origin, "invalid DataView byte offset");
    };
    usize::try_from(index).map_err(|_| {
        NativeFailure::Abrupt(data_view_range_pending(
            realm,
            origin,
            "DataView byte offset exceeds implementation range",
        ))
    })
}

fn data_view_to_boolean(value: &StoredValue) -> bool {
    value.is_truthy()
}

fn data_view_length_number(length: usize) -> JsNumber {
    #[expect(
        clippy::cast_precision_loss,
        reason = "DataView lengths are bounded by ToIndex and therefore exactly binary64 representable"
    )]
    let length = length as f64;
    JsNumber::from_f64(length)
}

fn data_view_f16_to_f64(bits: u16) -> f64 {
    let sign = if bits >> 15 == 0 { 1.0 } else { -1.0 };
    let exponent = (bits >> 10) & 0x1f;
    let fraction = bits & 0x03ff;
    match exponent {
        0 if fraction == 0 => 0.0_f64.copysign(sign),
        0 => sign * f64::from(fraction) * 2.0_f64.powi(-24),
        0x1f if fraction == 0 => f64::INFINITY.copysign(sign),
        0x1f => f64::NAN.copysign(sign),
        exponent => {
            sign * (1.0 + f64::from(fraction) / 1024.0) * 2.0_f64.powi(i32::from(exponent) - 15)
        }
    }
}

fn data_view_f64_to_f16(value: f64) -> u16 {
    let sign = u16::try_from((value.to_bits() >> 63) << 15)
        .expect("one-bit sign shifted into a u16 always fits");
    if value.is_nan() {
        return sign | 0x7e00;
    }
    if value.is_infinite() {
        return sign | 0x7c00;
    }
    let magnitude = value.abs();
    if magnitude == 0.0 {
        return sign;
    }
    let rounded = data_view_f16round(magnitude);
    if rounded.is_infinite() {
        return sign | 0x7c00;
    }
    if rounded < 2.0_f64.powi(-14) {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a rounded binary16 subnormal significand lies in 1..1023"
        )]
        let fraction = (rounded * 2.0_f64.powi(24)).round_ties_even() as u16;
        return sign | fraction;
    }
    let exponent = ((rounded.to_bits() >> 52) & 0x7ff) as i32 - 1023;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the rounded binary16 fraction lies in 0..1023"
    )]
    let fraction = ((rounded / 2.0_f64.powi(exponent) - 1.0) * 1024.0).round_ties_even() as u16;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the binary16 biased exponent lies in 1..30"
    )]
    let exponent = (exponent + 15) as u16;
    sign | (exponent << 10) | fraction
}

fn data_view_f16round(magnitude: f64) -> f64 {
    let rounded = if magnitude < 2.0_f64.powi(-14) {
        magnitude.mul_add(2.0_f64.powi(24), 0.0).round_ties_even() * 2.0_f64.powi(-24)
    } else {
        let biased_exponent = ((magnitude.to_bits() >> 52) & 0x7ff) as i32;
        let exponent = biased_exponent - 1023;
        let quantum = 2.0_f64.powi(exponent - 10);
        (magnitude / quantum).round_ties_even() * quantum
    };
    if rounded > 65_504.0 {
        f64::INFINITY
    } else {
        rounded
    }
}

fn data_view_type_error<T>(
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

fn data_view_range_pending(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> PendingException {
    PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::RangeError,
            message: JsString::from_utf8(message)
                .expect("static DataView range messages are valid UTF-8"),
        },
        origin: origin.clone(),
    }
}

fn data_view_range_error<T>(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<T, NativeFailure> {
    Err(NativeFailure::Abrupt(data_view_range_pending(
        realm, origin, message,
    )))
}
