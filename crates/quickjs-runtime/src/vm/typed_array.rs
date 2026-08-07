//! Resumable integer-indexed typed-array element writes.
//!
//! `TypedArraySetElement` converts its value before it performs the final
//! `IsValidIntegerIndex` witness. Keeping that sequence in an explicit
//! `OperatorPrimitiveTarget` lets a user-defined conversion resize a backing
//! `ArrayBuffer` without publishing an old bounds decision.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) struct TypedArrayElementSetState {
    object: ObjectId,
    index: Option<usize>,
    completion: TypedArraySetCompletion,
    realm: RealmId,
    origin: JsStackFrame,
}

#[derive(Clone, Copy)]
pub(super) enum TypedArraySetCompletion {
    LanguageWrite,
    ReflectSet,
    Define(DefinePropertyResult),
}

/// The non-observable prefix of typed-array `[[DefineOwnProperty]]`.
///
/// A `Store` has already passed its first `IsValidIntegerIndex` and all
/// descriptor shape restrictions. Its eventual store still takes a new
/// buffer witness after value coercion, as required by `TypedArraySetElement`.
pub(super) enum TypedArrayDefineAction {
    Ordinary,
    Rejected,
    Complete,
    Store(usize),
}

/// `%Int8Array%`-style construction after the initial `ToIndex(length)`.
pub(super) struct TypedArrayConstructorLengthState {
    new_target: FunctionId,
    element: TypedArrayElementType,
    realm: RealmId,
    origin: JsStackFrame,
}

/// `%TypedArray%` construction from an `ArrayBuffer`, awaiting `ToIndex` for
/// the optional byte offset. The length operand remains rooted because its
/// conversion must happen only after the offset has been validated.
pub(super) struct TypedArrayConstructorBufferOffsetState {
    prototype: HeapReference,
    buffer: ObjectId,
    byte_length: StoredValue,
    element: TypedArrayElementType,
    realm: RealmId,
    origin: JsStackFrame,
}

/// `%TypedArray%` construction from an `ArrayBuffer`, awaiting `ToIndex` for
/// the explicit element length.
pub(super) struct TypedArrayConstructorBufferLengthState {
    prototype: HeapReference,
    buffer: ObjectId,
    byte_offset: usize,
    element: TypedArrayElementType,
    realm: RealmId,
    origin: JsStackFrame,
}

/// `%TypedArray%` construction from an object. `AllocateTypedArray` performs
/// the `newTarget.prototype` lookup before it dispatches to the typed-array,
/// ArrayBuffer, iterable, or array-like initializer, so all object operands
/// stay rooted in one continuation across that lookup.
pub(super) struct TypedArrayConstructorObjectState {
    new_target: FunctionId,
    source: StoredValue,
    byte_offset: StoredValue,
    byte_length: StoredValue,
    element: TypedArrayElementType,
    realm: RealmId,
    origin: JsStackFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedArrayConstructorSequenceStage {
    AwaitIteratorMethod,
    AwaitIterator,
    AwaitNextMethod,
    AwaitNextResult,
    AwaitDone,
    AwaitIteratorValue,
    AwaitArrayLikeLength,
    AwaitArrayLikeLengthConversion,
    AwaitArrayLikeElement,
}

/// Resumable initialization of a freshly allocated typed array from an
/// iterable or array-like object. Iterable values are collected before the
/// destination buffer is allocated; array-like values are read only after the
/// length-established destination exists.
pub(super) struct TypedArrayConstructorSequenceState {
    prototype: HeapReference,
    source: StoredValue,
    element: TypedArrayElementType,
    values: Vec<StoredValue>,
    iterator: Option<StoredValue>,
    next: Option<StoredValue>,
    result: Option<StoredValue>,
    target: Option<ObjectId>,
    length: usize,
    index: usize,
    realm: RealmId,
    stage: TypedArrayConstructorSequenceStage,
    origin: JsStackFrame,
}

impl TypedArrayConstructorLengthState {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
    }
}

impl TypedArrayConstructorBufferOffsetState {
    pub(super) const fn retained_values() -> u64 {
        3
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(self.prototype));
        mark(CollectionRoot::Heap(HeapReference::Object(self.buffer)));
        trace_stored_value_root(&self.byte_length, mark);
    }
}

impl TypedArrayConstructorBufferLengthState {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(self.prototype));
        mark(CollectionRoot::Heap(HeapReference::Object(self.buffer)));
    }
}

impl TypedArrayConstructorObjectState {
    pub(super) const fn retained_values() -> u64 {
        4
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
        trace_stored_value_root(&self.source, mark);
        trace_stored_value_root(&self.byte_offset, mark);
        trace_stored_value_root(&self.byte_length, mark);
    }
}

impl TypedArrayConstructorSequenceState {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(usize_to_u64(self.values.len()))
            .saturating_add(u64::from(self.iterator.is_some()))
            .saturating_add(u64::from(self.next.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
            .saturating_add(u64::from(self.target.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(self.prototype));
        trace_stored_value_root(&self.source, mark);
        for value in &self.values {
            trace_stored_value_root(value, mark);
        }
        for value in [
            self.iterator.as_ref(),
            self.next.as_ref(),
            self.result.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            trace_stored_value_root(value, mark);
        }
        if let Some(target) = self.target {
            mark(CollectionRoot::Heap(HeapReference::Object(target)));
        }
    }
}

impl TypedArrayElementSetState {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.object)));
    }
}

/// Recognizes a canonical numeric key on a direct typed-array receiver. The
/// returned key deliberately preserves `Invalid`: it must still run the value
/// conversion for a self-receiver write before the final bounds decision.
pub(super) fn typed_array_indexed_key(
    runtime: &Runtime,
    base: &StoredValue,
    key: &PropertyKey,
) -> Result<Option<(ObjectId, TypedArrayPropertyKey)>, ExecutionError> {
    let StoredValue::Object(object) = base else {
        return Ok(None);
    };
    let Some(key) = runtime.typed_array_property_key(*object, key)? else {
        return Ok(None);
    };
    Ok((key != TypedArrayPropertyKey::Ordinary).then_some((*object, key)))
}

pub(super) fn typed_array_define_own_property_action(
    runtime: &Runtime,
    object: ObjectId,
    key: &PropertyKey,
    definition: &PropertyDefinition,
) -> Result<Option<TypedArrayDefineAction>, ExecutionError> {
    let Some(key) = runtime.typed_array_property_key(object, key)? else {
        return Ok(None);
    };
    let TypedArrayPropertyKey::Index(index) = key else {
        return Ok(Some(match key {
            TypedArrayPropertyKey::Ordinary => TypedArrayDefineAction::Ordinary,
            TypedArrayPropertyKey::Invalid => TypedArrayDefineAction::Rejected,
            TypedArrayPropertyKey::Index(_) => unreachable!("matched above"),
        }));
    };
    if runtime.typed_array_read_index(object, index)?.is_none()
        || definition.requested_configurable() == Some(false)
        || definition.requested_enumerable() == Some(false)
        || definition.is_accessor_descriptor()
        || definition.requested_writable() == Some(false)
    {
        return Ok(Some(TypedArrayDefineAction::Rejected));
    }
    Ok(Some(if definition.has_present_data_value() {
        TypedArrayDefineAction::Store(index)
    } else {
        TypedArrayDefineAction::Complete
    }))
}

pub(super) fn begin_typed_array_constructor(
    runtime: &mut Runtime,
    element: TypedArrayElementType,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = inputs.new_target else {
        return typed_array_type_error(realm, &origin, "TypedArray constructor requires 'new'");
    };
    let mut arguments = inputs.arguments;
    let source = arguments.take_first_or_undefined();
    let byte_offset = arguments.take_first_or_undefined();
    let byte_length = arguments.take_first_or_undefined();
    if matches!(source, StoredValue::Object(_) | StoredValue::Function(_)) {
        return begin_typed_array_constructor_object_prototype_get(
            runtime,
            TypedArrayConstructorObjectState {
                new_target,
                source,
                byte_offset,
                byte_length,
                element,
                realm,
                origin: origin.clone(),
            },
            return_to,
            execution_budget,
        );
    }
    begin_operator_primitive_conversion(
        runtime,
        source,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayConstructorLength(Box::new(
            TypedArrayConstructorLengthState {
                new_target,
                element,
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

#[allow(
    clippy::too_many_arguments,
    reason = "the ArrayBuffer initializer has already selected its constructor prototype before coercing byteOffset"
)]
pub(super) fn begin_typed_array_constructor_buffer_offset(
    runtime: &mut Runtime,
    prototype: HeapReference,
    buffer: ObjectId,
    byte_offset: StoredValue,
    byte_length: StoredValue,
    element: TypedArrayElementType,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_operator_primitive_conversion(
        runtime,
        byte_offset,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayConstructorBufferOffset(Box::new(
            TypedArrayConstructorBufferOffsetState {
                prototype,
                buffer,
                byte_length,
                element,
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

fn begin_typed_array_constructor_object_prototype_get(
    runtime: &mut Runtime,
    state: TypedArrayConstructorObjectState,
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
        typed_array_constructor_object_continuation,
        |state, value| {
            advance_typed_array_constructor_object(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "TypedArray constructor prototype Get produced a structured result",
    )
}

pub(super) fn advance_typed_array_constructor_object(
    runtime: &mut Runtime,
    state: TypedArrayConstructorObjectState,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let requested = completion.ok_or(EngineFault::RuntimeInvariant {
        message: "TypedArray constructor prototype lookup resumed without a completion",
    })?;
    let prototype =
        typed_array_constructor_prototype(runtime, state.new_target, state.element, &requested)?;
    if let StoredValue::Object(source) = state.source {
        if runtime.array_buffer_state(source)?.is_some() {
            return begin_typed_array_constructor_buffer_offset(
                runtime,
                prototype,
                source,
                state.byte_offset,
                state.byte_length,
                state.element,
                state.realm,
                return_to,
                state.origin,
                execution_budget,
            );
        }
        if runtime.typed_array_state(source)?.is_some() {
            return finish_typed_array_constructor_from_typed_array(
                runtime,
                prototype,
                source,
                state.element,
                state.realm,
                &state.origin,
            );
        }
    }
    begin_typed_array_constructor_sequence(
        runtime,
        TypedArrayConstructorSequenceState {
            prototype,
            source: state.source,
            element: state.element,
            values: Vec::new(),
            iterator: None,
            next: None,
            result: None,
            target: None,
            length: 0,
            index: 0,
            realm: state.realm,
            stage: TypedArrayConstructorSequenceStage::AwaitIteratorMethod,
            origin: state.origin,
        },
        return_to,
        execution_budget,
    )
}

fn begin_typed_array_constructor_sequence(
    runtime: &mut Runtime,
    state: TypedArrayConstructorSequenceState,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    typed_array_sequence_read(
        runtime,
        state,
        runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "one explicit state machine preserves the distinct iterable-list and array-like typed-array initialization orders"
)]
pub(super) fn advance_typed_array_constructor_sequence(
    runtime: &mut Runtime,
    mut state: TypedArrayConstructorSequenceState,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TypedArrayConstructorSequenceStage::AwaitIteratorMethod => {
                let method = typed_array_take_completion(&mut completion)?;
                match method {
                    StoredValue::Undefined | StoredValue::Null => {
                        state.stage = TypedArrayConstructorSequenceStage::AwaitArrayLikeLength;
                        return typed_array_sequence_read(
                            runtime,
                            state,
                            runtime.predefined_property_key(PredefinedAtom::Length),
                            return_to,
                            execution_budget,
                        );
                    }
                    StoredValue::Function(function) => {
                        state.stage = TypedArrayConstructorSequenceStage::AwaitIterator;
                        let receiver = state.source.duplicate();
                        return typed_array_sequence_call(
                            state,
                            function,
                            receiver,
                            Vec::new(),
                            return_to,
                        );
                    }
                    _ => {
                        return typed_array_type_error(
                            state.realm,
                            &state.origin,
                            "TypedArray Symbol.iterator is not callable",
                        );
                    }
                }
            }
            TypedArrayConstructorSequenceStage::AwaitIterator => {
                let iterator = typed_array_require_object(
                    &state,
                    typed_array_take_completion(&mut completion)?,
                )?;
                state.iterator = Some(iterator);
                state.stage = TypedArrayConstructorSequenceStage::AwaitNextMethod;
                return typed_array_sequence_read(
                    runtime,
                    state,
                    runtime.predefined_property_key(PredefinedAtom::Next),
                    return_to,
                    execution_budget,
                );
            }
            TypedArrayConstructorSequenceStage::AwaitNextMethod => {
                state.next = Some(typed_array_take_completion(&mut completion)?);
                return typed_array_sequence_call_next(state, return_to, execution_budget);
            }
            TypedArrayConstructorSequenceStage::AwaitNextResult => {
                state.result = Some(typed_array_require_object(
                    &state,
                    typed_array_take_completion(&mut completion)?,
                )?);
                state.stage = TypedArrayConstructorSequenceStage::AwaitDone;
                return typed_array_sequence_read(
                    runtime,
                    state,
                    runtime.predefined_property_key(PredefinedAtom::Done),
                    return_to,
                    execution_budget,
                );
            }
            TypedArrayConstructorSequenceStage::AwaitDone => {
                if typed_array_take_completion(&mut completion)?.is_truthy() {
                    state.iterator = None;
                    state.next = None;
                    state.result = None;
                    state.length = state.values.len();
                    typed_array_sequence_allocate(runtime, &mut state)?;
                    state.stage = TypedArrayConstructorSequenceStage::AwaitIteratorValue;
                    return typed_array_sequence_begin_next_element(
                        runtime,
                        state,
                        return_to,
                        execution_budget,
                    );
                }
                state.stage = TypedArrayConstructorSequenceStage::AwaitIteratorValue;
                return typed_array_sequence_read(
                    runtime,
                    state,
                    runtime.predefined_property_key(PredefinedAtom::Value),
                    return_to,
                    execution_budget,
                );
            }
            TypedArrayConstructorSequenceStage::AwaitIteratorValue => {
                let value = typed_array_take_completion(&mut completion)?;
                state
                    .values
                    .try_reserve(1)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::Frames,
                        additional: 1,
                    })?;
                state.values.push(value);
                state.result = None;
                return typed_array_sequence_call_next(state, return_to, execution_budget);
            }
            TypedArrayConstructorSequenceStage::AwaitArrayLikeLength => {
                let value = typed_array_take_completion(&mut completion)?;
                state.stage = TypedArrayConstructorSequenceStage::AwaitArrayLikeLengthConversion;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::TypedArrayConstructorArrayLikeLength(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TypedArrayConstructorSequenceStage::AwaitArrayLikeLengthConversion => {
                let length = number_to_length(operator_to_number(
                    typed_array_take_completion(&mut completion)?,
                    state.realm,
                    &state.origin,
                )?);
                let Ok(length) = usize::try_from(length) else {
                    return typed_array_range_error(
                        state.realm,
                        &state.origin,
                        "TypedArray length exceeds implementation range",
                    );
                };
                state.length = length;
                typed_array_sequence_allocate(runtime, &mut state)?;
                return typed_array_sequence_begin_next_element(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                );
            }
            TypedArrayConstructorSequenceStage::AwaitArrayLikeElement => {
                return typed_array_sequence_begin_element_conversion(
                    runtime,
                    state,
                    typed_array_take_completion(&mut completion)?,
                    return_to,
                    execution_budget,
                );
            }
        }
    }
}

pub(super) fn finish_typed_array_constructor_array_like_length(
    runtime: &mut Runtime,
    mut state: TypedArrayConstructorSequenceState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = TypedArrayConstructorSequenceStage::AwaitArrayLikeLengthConversion;
    advance_typed_array_constructor_sequence(
        runtime,
        state,
        Some(value),
        return_to,
        execution_budget,
    )
}

pub(super) fn finish_typed_array_constructor_element(
    runtime: &mut Runtime,
    mut state: TypedArrayConstructorSequenceState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = state.target.ok_or(EngineFault::RuntimeInvariant {
        message: "TypedArray constructor element conversion lost its target",
    })?;
    let stored = if state.element.is_bigint() {
        let value = to_bigint_from_primitive(&value, state.realm, &state.origin)?;
        runtime.typed_array_store_index(
            target,
            state.index,
            TypedArrayElementValue::BigInt(value.as_ref()),
        )?
    } else {
        let value = operator_to_number(value, state.realm, &state.origin)?;
        runtime.typed_array_store_index(
            target,
            state.index,
            TypedArrayElementValue::Number(value),
        )?
    };
    if stored != TypedArrayStoreOutcome::Stored {
        return Err(EngineFault::RuntimeInvariant {
            message: "TypedArray constructor destination lost its element slot",
        }
        .into());
    }
    state.index = state.index.saturating_add(1);
    typed_array_sequence_begin_next_element(runtime, state, return_to, execution_budget)
}

fn typed_array_sequence_allocate(
    runtime: &mut Runtime,
    state: &mut TypedArrayConstructorSequenceState,
) -> Result<(), NativeFailure> {
    let Some(byte_length) = state.length.checked_mul(state.element.byte_width()) else {
        return typed_array_range_error(
            state.realm,
            &state.origin,
            "TypedArray length exceeds implementation range",
        );
    };
    let buffer = runtime
        .allocate_array_buffer(
            HeapReference::Object(runtime.realm_array_buffer_prototype(state.realm)?),
            byte_length,
            None,
        )
        .map_err(NativeFailure::Execution)?;
    let target = runtime
        .allocate_typed_array(
            state.prototype,
            TypedArrayState::new(
                buffer,
                0,
                TypedArrayLength::Fixed(state.length),
                state.element,
            ),
        )
        .map_err(NativeFailure::Execution)?;
    state.target = Some(target);
    Ok(())
}

fn typed_array_sequence_begin_next_element(
    runtime: &mut Runtime,
    mut state: TypedArrayConstructorSequenceState,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.index >= state.length {
        let target = state.target.ok_or(EngineFault::RuntimeInvariant {
            message: "TypedArray constructor completed without a target",
        })?;
        return Ok(NativeDispatch::Immediate(StoredValue::Object(target)));
    }
    match state.stage {
        TypedArrayConstructorSequenceStage::AwaitIteratorValue => {
            let value = state.values[state.index].duplicate();
            typed_array_sequence_begin_element_conversion(
                runtime,
                state,
                value,
                return_to,
                execution_budget,
            )
        }
        TypedArrayConstructorSequenceStage::AwaitArrayLikeLengthConversion
        | TypedArrayConstructorSequenceStage::AwaitArrayLikeElement => {
            let index = u64::try_from(state.index).map_err(|_| EngineFault::RuntimeInvariant {
                message: "TypedArray array-like index does not fit u64",
            })?;
            let key = array_static_index_key(runtime, index)?;
            state.stage = TypedArrayConstructorSequenceStage::AwaitArrayLikeElement;
            typed_array_sequence_read(
                runtime,
                state,
                key,
                return_to,
                execution_budget,
            )
        }
        _ => Err(EngineFault::RuntimeInvariant {
            message: "TypedArray constructor attempted element initialization from an invalid stage",
        }
        .into()),
    }
}

fn typed_array_sequence_begin_element_conversion(
    runtime: &mut Runtime,
    state: TypedArrayConstructorSequenceState,
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
        OperatorPrimitiveTarget::TypedArrayConstructorElement(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn typed_array_sequence_read(
    runtime: &mut Runtime,
    state: TypedArrayConstructorSequenceState,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = match state.stage {
        TypedArrayConstructorSequenceStage::AwaitIteratorMethod
        | TypedArrayConstructorSequenceStage::AwaitArrayLikeLength
        | TypedArrayConstructorSequenceStage::AwaitArrayLikeElement => state.source.duplicate(),
        TypedArrayConstructorSequenceStage::AwaitNextMethod => state
            .iterator
            .as_ref()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "TypedArray iterable initializer lost its iterator",
            })?
            .duplicate(),
        TypedArrayConstructorSequenceStage::AwaitDone
        | TypedArrayConstructorSequenceStage::AwaitIteratorValue => state
            .result
            .as_ref()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "TypedArray iterable initializer lost its iterator result",
            })?
            .duplicate(),
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "TypedArray constructor attempted a property read from an invalid stage",
            }
            .into());
        }
    };
    charge_heap_property_lookup(runtime, &base, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        &base,
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
        typed_array_constructor_sequence_continuation,
        |state, value| {
            advance_typed_array_constructor_sequence(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "TypedArray constructor Get produced a structured result",
    )
}

fn typed_array_sequence_call(
    state: TypedArrayConstructorSequenceState,
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
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
    continuations.push(typed_array_constructor_sequence_continuation(state));
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

fn typed_array_sequence_call_next(
    mut state: TypedArrayConstructorSequenceState,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(next) = state.next.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "TypedArray iterable initializer lost its next method",
    })?
    else {
        return typed_array_type_error(
            state.realm,
            &state.origin,
            "TypedArray iterator next is not callable",
        );
    };
    execution_budget.charge_instructions(1)?;
    let function = *next;
    let receiver = state
        .iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "TypedArray iterable initializer lost its iterator",
        })?
        .duplicate();
    state.stage = TypedArrayConstructorSequenceStage::AwaitNextResult;
    typed_array_sequence_call(state, function, receiver, Vec::new(), return_to)
}

fn typed_array_require_object(
    state: &TypedArrayConstructorSequenceState,
    value: StoredValue,
) -> Result<StoredValue, NativeFailure> {
    if value.heap_reference().is_some() {
        Ok(value)
    } else {
        typed_array_type_error(
            state.realm,
            &state.origin,
            "TypedArray iterator result is not an object",
        )
    }
}

fn typed_array_take_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or(
        EngineFault::RuntimeInvariant {
            message: "TypedArray constructor resumed without a completion",
        }
        .into(),
    )
}

pub(super) fn finish_typed_array_constructor_buffer_offset(
    runtime: &mut Runtime,
    state: TypedArrayConstructorBufferOffsetState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let byte_offset = typed_array_to_index(value, state.realm, &state.origin)?;
    let element_width = state.element.byte_width();
    if byte_offset % element_width != 0 {
        return typed_array_range_error(
            state.realm,
            &state.origin,
            "TypedArray byte offset is not aligned to its element size",
        );
    }
    if !matches!(state.byte_length, StoredValue::Undefined) {
        let byte_length = state.byte_length.duplicate();
        return begin_operator_primitive_conversion(
            runtime,
            byte_length,
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::TypedArrayConstructorBufferLength(Box::new(
                TypedArrayConstructorBufferLengthState {
                    prototype: state.prototype,
                    buffer: state.buffer,
                    byte_offset,
                    element: state.element,
                    realm: state.realm,
                    origin: state.origin.clone(),
                },
            )),
            state.realm,
            return_to,
            state.origin,
            execution_budget,
        );
    }
    let (buffer_byte_length, resizable) =
        typed_array_buffer_length(runtime, state.buffer, state.realm, &state.origin)?;
    if byte_offset > buffer_byte_length {
        return typed_array_range_error(
            state.realm,
            &state.origin,
            "TypedArray byte offset is outside buffer",
        );
    }
    let length = if resizable {
        TypedArrayLength::Auto
    } else {
        let remainder = buffer_byte_length.saturating_sub(byte_offset);
        if buffer_byte_length % element_width != 0 {
            return typed_array_range_error(
                state.realm,
                &state.origin,
                "TypedArray byte length is not aligned to its element size",
            );
        }
        TypedArrayLength::Fixed(remainder / element_width)
    };
    let _ = (return_to, execution_budget);
    finish_typed_array_constructor_buffer(
        runtime,
        state.prototype,
        state.element,
        state.buffer,
        byte_offset,
        length,
        state.realm,
        &state.origin,
    )
}

pub(super) fn finish_typed_array_constructor_buffer_length(
    runtime: &mut Runtime,
    state: TypedArrayConstructorBufferLengthState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let length = typed_array_to_index(value, state.realm, &state.origin)?;
    let Some(byte_length) = length.checked_mul(state.element.byte_width()) else {
        return typed_array_range_error(
            state.realm,
            &state.origin,
            "TypedArray length exceeds implementation range",
        );
    };
    let (buffer_byte_length, _) =
        typed_array_buffer_length(runtime, state.buffer, state.realm, &state.origin)?;
    if state
        .byte_offset
        .checked_add(byte_length)
        .is_none_or(|end| end > buffer_byte_length)
    {
        return typed_array_range_error(
            state.realm,
            &state.origin,
            "TypedArray length is outside buffer",
        );
    }
    let _ = (return_to, execution_budget);
    finish_typed_array_constructor_buffer(
        runtime,
        state.prototype,
        state.element,
        state.buffer,
        state.byte_offset,
        TypedArrayLength::Fixed(length),
        state.realm,
        &state.origin,
    )
}

pub(super) fn finish_typed_array_constructor_length(
    runtime: &mut Runtime,
    state: TypedArrayConstructorLengthState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let length = typed_array_to_index(value, state.realm, &state.origin)?;
    let Some(_byte_length) = length.checked_mul(state.element.byte_width()) else {
        return typed_array_range_error(
            state.realm,
            &state.origin,
            "TypedArray length exceeds implementation range",
        );
    };
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    begin_intrinsic_get(
        runtime,
        state.realm,
        HeapReference::Function(state.new_target),
        StoredValue::Function(state.new_target),
        &prototype_key,
        IntrinsicGetContinuation::TypedArrayConstructor {
            new_target: state.new_target,
            element: state.element,
            length,
        },
        return_to,
        Some(state.origin),
        execution_budget,
    )
}

pub(super) fn finish_typed_array_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    element: TypedArrayElementType,
    length: usize,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let realm = runtime.function_realm(new_target)?;
    let prototype = typed_array_constructor_prototype(runtime, new_target, element, requested)?;
    let byte_length =
        length
            .checked_mul(element.byte_width())
            .ok_or(EngineFault::RuntimeInvariant {
                message: "validated typed-array length overflowed its byte length",
            })?;
    let buffer = runtime
        .allocate_array_buffer(
            HeapReference::Object(runtime.realm_array_buffer_prototype(realm)?),
            byte_length,
            None,
        )
        .map_err(NativeFailure::Execution)?;
    let object = runtime
        .allocate_typed_array(
            prototype,
            TypedArrayState::new(buffer, 0, TypedArrayLength::Fixed(length), element),
        )
        .map_err(NativeFailure::Execution)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the ArrayBuffer view allocation carries its selected prototype, element kind, view slots, realm, and source origin"
)]
pub(super) fn finish_typed_array_constructor_buffer(
    runtime: &mut Runtime,
    prototype: HeapReference,
    element: TypedArrayElementType,
    buffer: ObjectId,
    byte_offset: usize,
    length: TypedArrayLength,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let (buffer_byte_length, _) = typed_array_buffer_length(runtime, buffer, realm, origin)?;
    let valid = match length {
        TypedArrayLength::Auto => byte_offset <= buffer_byte_length,
        TypedArrayLength::Fixed(length) => length
            .checked_mul(element.byte_width())
            .and_then(|byte_length| byte_offset.checked_add(byte_length))
            .is_some_and(|end| end <= buffer_byte_length),
    };
    if !valid {
        return typed_array_type_error(
            realm,
            origin,
            "TypedArray backing buffer changed during construction",
        );
    }
    let object = runtime
        .allocate_typed_array(
            prototype,
            TypedArrayState::new(buffer, byte_offset, length, element),
        )
        .map_err(NativeFailure::Execution)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

#[allow(
    clippy::too_many_arguments,
    reason = "typed-array cloning retains the selected prototype, source identity, destination element kind, realm, and source origin"
)]
fn finish_typed_array_constructor_from_typed_array(
    runtime: &mut Runtime,
    prototype: HeapReference,
    source: ObjectId,
    element: TypedArrayElementType,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let source_state =
        runtime
            .typed_array_state(source)?
            .copied()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "TypedArray source lost its internal slots",
            })?;
    let TypedArrayView::InBounds {
        buffer: source_buffer,
        byte_offset: source_offset,
        length,
        element: source_element,
    } = runtime.typed_array_view(source)?
    else {
        return typed_array_type_error(realm, origin, "TypedArray source is out of bounds");
    };
    let Some(byte_length) = length.checked_mul(element.byte_width()) else {
        return typed_array_range_error(
            realm,
            origin,
            "TypedArray length exceeds implementation range",
        );
    };
    if source_state.element().is_bigint() != element.is_bigint() {
        return typed_array_type_error(
            realm,
            origin,
            "TypedArray source and destination content types differ",
        );
    }
    let target_buffer = runtime
        .allocate_array_buffer(
            HeapReference::Object(runtime.realm_array_buffer_prototype(realm)?),
            byte_length,
            None,
        )
        .map_err(NativeFailure::Execution)?;
    if source_element == element {
        runtime
            .copy_array_buffer_bytes(source_buffer, source_offset, target_buffer, byte_length)
            .map_err(NativeFailure::Execution)?;
    }
    let target = runtime
        .allocate_typed_array(
            prototype,
            TypedArrayState::new(target_buffer, 0, TypedArrayLength::Fixed(length), element),
        )
        .map_err(NativeFailure::Execution)?;
    if source_element != element {
        for index in 0..length {
            let value = runtime.typed_array_read_index(source, index)?.ok_or(
                EngineFault::RuntimeInvariant {
                    message: "typed-array source view changed during internal copy",
                },
            )?;
            let outcome = match value {
                StoredValue::Number(value) => runtime.typed_array_store_index(
                    target,
                    index,
                    TypedArrayElementValue::Number(value),
                )?,
                StoredValue::BigInt(value) => runtime.typed_array_store_index(
                    target,
                    index,
                    TypedArrayElementValue::BigInt(value.as_ref()),
                )?,
                StoredValue::Undefined
                | StoredValue::Null
                | StoredValue::Boolean(_)
                | StoredValue::String(_)
                | StoredValue::Symbol(_)
                | StoredValue::Object(_)
                | StoredValue::Function(_) => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "typed-array source read produced a non-numeric value",
                    }
                    .into());
                }
            };
            if outcome != TypedArrayStoreOutcome::Stored {
                return Err(EngineFault::RuntimeInvariant {
                    message: "typed-array destination lost its fresh element slot",
                }
                .into());
            }
        }
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(target)))
}

fn typed_array_constructor_prototype(
    runtime: &Runtime,
    new_target: FunctionId,
    element: TypedArrayElementType,
    requested: &StoredValue,
) -> Result<HeapReference, NativeFailure> {
    Ok(match requested {
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
            HeapReference::Object(runtime.realm_typed_array_prototype(realm, element)?)
        }
    })
}

fn typed_array_constructor_object_continuation(
    state: TypedArrayConstructorObjectState,
) -> NativeContinuation {
    NativeContinuation::TypedArrayConstructorObject(Box::new(state))
}

fn typed_array_constructor_sequence_continuation(
    state: TypedArrayConstructorSequenceState,
) -> NativeContinuation {
    NativeContinuation::TypedArrayConstructorSequence(Box::new(state))
}

pub(super) fn dispatch_typed_array_prototype(
    runtime: &Runtime,
    method: TypedArrayPrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(object) = receiver else {
        return typed_array_type_error(realm, &origin, "not a TypedArray");
    };
    let state =
        runtime
            .typed_array_state(*object)?
            .copied()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "typed-array prototype receiver lost its internal slots",
            })?;
    let view = runtime.typed_array_view(*object)?;
    let (byte_length, byte_offset, length) = match view {
        TypedArrayView::InBounds {
            byte_offset,
            length,
            ..
        } => (
            length.saturating_mul(state.element().byte_width()),
            byte_offset,
            length,
        ),
        TypedArrayView::Detached | TypedArrayView::OutOfBounds => (0, 0, 0),
    };
    let number = |value: usize| {
        #[expect(
            clippy::cast_precision_loss,
            reason = "typed-array byte lengths and indices are bounded by ToIndex"
        )]
        let value = value as f64;
        StoredValue::Number(JsNumber::from_f64(value))
    };
    Ok(NativeDispatch::Immediate(match method {
        TypedArrayPrototypeMethod::Buffer => StoredValue::Object(state.buffer()),
        TypedArrayPrototypeMethod::ByteLength => number(byte_length),
        TypedArrayPrototypeMethod::ByteOffset => number(byte_offset),
        TypedArrayPrototypeMethod::Length => number(length),
        TypedArrayPrototypeMethod::ToStringTag => {
            StoredValue::String(JsString::from_utf8(typed_array_name(state.element()))?)
        }
    }))
}

fn typed_array_to_index(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<usize, NativeFailure> {
    let number = operator_to_number(value, realm, origin)?;
    let Some(index) = number_to_index(number) else {
        return typed_array_range_error(realm, origin, "invalid TypedArray length");
    };
    usize::try_from(index).map_err(|_| {
        NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("TypedArray length exceeds implementation range")
                    .expect("static TypedArray range message is valid UTF-8"),
            },
            origin: origin.clone(),
        })
    })
}

fn typed_array_buffer_length(
    runtime: &Runtime,
    buffer: ObjectId,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<(usize, bool), NativeFailure> {
    let state = runtime
        .array_buffer_state(buffer)?
        .ok_or(EngineFault::RuntimeInvariant {
            message: "TypedArray backing buffer lost its ArrayBuffer slots",
        })?;
    if state.is_detached() {
        return typed_array_type_error(realm, origin, "TypedArray backing buffer is detached");
    }
    Ok((state.byte_length(), state.is_resizable()))
}

fn typed_array_name(element: TypedArrayElementType) -> &'static str {
    match element {
        TypedArrayElementType::Int8 => "Int8Array",
        TypedArrayElementType::Uint8 => "Uint8Array",
        TypedArrayElementType::Uint8Clamped => "Uint8ClampedArray",
        TypedArrayElementType::Int16 => "Int16Array",
        TypedArrayElementType::Uint16 => "Uint16Array",
        TypedArrayElementType::Int32 => "Int32Array",
        TypedArrayElementType::Uint32 => "Uint32Array",
        TypedArrayElementType::BigInt64 => "BigInt64Array",
        TypedArrayElementType::BigUint64 => "BigUint64Array",
        TypedArrayElementType::Float16 => "Float16Array",
        TypedArrayElementType::Float32 => "Float32Array",
        TypedArrayElementType::Float64 => "Float64Array",
    }
}

fn typed_array_type_error<T>(
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

fn typed_array_range_error<T>(
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

/// Starts `TypedArraySetElement` for a canonical numeric key. `Invalid` is
/// still converted: the conversion precedes `IsValidIntegerIndex` in the
/// normative abstract operation, while a non-canonical key never reaches this
/// path.
#[allow(
    clippy::too_many_arguments,
    reason = "the receiver-independent typed-array write carries its key classification, completion shape, and VM resume authority explicitly"
)]
pub(super) fn begin_typed_array_element_set(
    runtime: &mut Runtime,
    object: ObjectId,
    key: TypedArrayPropertyKey,
    value: StoredValue,
    completion: TypedArraySetCompletion,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let index = match key {
        TypedArrayPropertyKey::Index(index) => Some(index),
        TypedArrayPropertyKey::Invalid => None,
        TypedArrayPropertyKey::Ordinary => {
            return Err(EngineFault::RuntimeInvariant {
                message: "typed-array element set received an ordinary property key",
            }
            .into());
        }
    };
    if runtime.typed_array_state(object)?.is_none() {
        return Err(EngineFault::RuntimeInvariant {
            message: "typed-array element set receiver lost its internal slots",
        }
        .into());
    }
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayElementSet(Box::new(TypedArrayElementSetState {
            object,
            index,
            completion,
            realm,
            origin: origin.clone(),
        })),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_typed_array_element_set(
    runtime: &mut Runtime,
    state: TypedArrayElementSetState,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let element = runtime
        .typed_array_state(state.object)?
        .copied()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "typed-array element set receiver lost its internal slots",
        })?
        .element();
    let stored = if element.is_bigint() {
        let value = to_bigint_from_primitive(&value, state.realm, &state.origin)?;
        state
            .index
            .map_or(Ok(TypedArrayStoreOutcome::Missing), |index| {
                runtime.typed_array_store_index(
                    state.object,
                    index,
                    TypedArrayElementValue::BigInt(value.as_ref()),
                )
            })?
    } else {
        let value = operator_to_number(value, state.realm, &state.origin)?;
        state
            .index
            .map_or(Ok(TypedArrayStoreOutcome::Missing), |index| {
                runtime.typed_array_store_index(
                    state.object,
                    index,
                    TypedArrayElementValue::Number(value),
                )
            })?
    };
    if stored == TypedArrayStoreOutcome::ContentTypeMismatch {
        return Err(EngineFault::RuntimeInvariant {
            message: "typed-array element content type changed during conversion",
        }
        .into());
    }
    Ok(NativeDispatch::Immediate(match state.completion {
        TypedArraySetCompletion::LanguageWrite => StoredValue::Undefined,
        TypedArraySetCompletion::ReflectSet => StoredValue::Boolean(true),
        TypedArraySetCompletion::Define(DefinePropertyResult::Target) => {
            StoredValue::Object(state.object)
        }
        TypedArraySetCompletion::Define(DefinePropertyResult::Boolean) => {
            StoredValue::Boolean(true)
        }
    }))
}
