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

pub(super) enum TypedArraySetCompletion {
    LanguageWrite,
    ReflectSet,
    Define(DefinePropertyResult),
    /// Resume `%TypedArray%.prototype.map` after its mapped value's indexed
    /// write has completed its own observable numeric conversion.
    Map(Box<TypedArrayPrototypeMapState>),
    /// Resume `%TypedArray%.prototype.filter` after a collected value's
    /// indexed write has completed its own observable numeric conversion.
    Filter(Box<TypedArrayPrototypeFilterState>),
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
/// `ArrayBuffer`, iterable, or array-like initializer, so all object operands
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

#[allow(
    clippy::enum_variant_names,
    reason = "each name states the observable completion the constructor state machine awaits"
)]
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

#[allow(
    clippy::enum_variant_names,
    reason = "each name states the observable source completion the set state machine awaits"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedArrayPrototypeSetStage {
    AwaitSourceLength,
    AwaitSourceLengthConversion,
    AwaitSourceElement,
}

/// Resumable `%TypedArray%.prototype.set` from an array-like source.
///
/// A typed-array source follows the non-observable copy path. Every other
/// source uses the array-like algorithm: it snapshots the target length before
/// reading `length`, then reads and converts one source element before taking
/// that store's fresh buffer witness.
pub(super) struct TypedArrayPrototypeSetState {
    target: ObjectId,
    source: StoredValue,
    target_offset: usize,
    target_length: usize,
    source_length: usize,
    source_index: usize,
    realm: RealmId,
    stage: TypedArrayPrototypeSetStage,
    origin: JsStackFrame,
}

#[allow(
    clippy::enum_variant_names,
    reason = "each name identifies the observable conversion or species boundary being awaited"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedArrayPrototypeSubarrayStage {
    AwaitConstructor,
    AwaitSpecies,
    AwaitConstruct,
}

/// Resumable `%TypedArray%.prototype.subarray` construction.
///
/// The initial witness fixes source length before either relative-index
/// conversion can run user code; an out-of-bounds source contributes zero
/// while retaining its internal byte offset. Species lookup and construction
/// then follow the same observable ordering as `TypedArraySpeciesCreate`.
pub(super) struct TypedArrayPrototypeSubarrayState {
    source: ObjectId,
    buffer: ObjectId,
    source_byte_offset: usize,
    source_length: usize,
    begin: usize,
    new_length: usize,
    length_tracking: bool,
    end: StoredValue,
    element: TypedArrayElementType,
    realm: RealmId,
    stage: TypedArrayPrototypeSubarrayStage,
    origin: JsStackFrame,
}

#[allow(
    clippy::enum_variant_names,
    reason = "each name identifies the observable conversion or species boundary being awaited"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedArrayPrototypeSliceStage {
    AwaitConstructor,
    AwaitSpecies,
    AwaitConstruct,
}

/// Resumable `%TypedArray%.prototype.slice` construction and copy.
///
/// The original validated source length determines the requested species
/// result length. A fresh source witness is intentionally deferred until the
/// species constructor returns, so resizable-buffer changes use the specified
/// truncated copy range (and an initially empty range does not revalidate).
pub(super) struct TypedArrayPrototypeSliceState {
    source: ObjectId,
    source_length: usize,
    start: usize,
    end: StoredValue,
    end_index: usize,
    count: usize,
    element: TypedArrayElementType,
    realm: RealmId,
    stage: TypedArrayPrototypeSliceStage,
    origin: JsStackFrame,
}

#[allow(
    clippy::enum_variant_names,
    reason = "each name identifies the observable species, read, callback, or write boundary being awaited"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedArrayPrototypeMapStage {
    AwaitConstructor,
    AwaitSpecies,
    AwaitConstruct,
    NextElement,
    AwaitElement,
    AwaitCallback,
}

/// Resumable `%TypedArray%.prototype.map`.
///
/// `map` constructs its species result before it observes any source element.
/// Source element reads remain fresh during traversal, while each result write
/// uses `TypedArraySetElement` so a mapped value's own coercion can resize the
/// destination before its final integer-index witness.
pub(super) struct TypedArrayPrototypeMapState {
    source: ObjectId,
    source_length: usize,
    source_element: TypedArrayElementType,
    target: Option<ObjectId>,
    callback: FunctionId,
    this_argument: StoredValue,
    index: usize,
    realm: RealmId,
    stage: TypedArrayPrototypeMapStage,
    origin: JsStackFrame,
}

#[allow(
    clippy::enum_variant_names,
    reason = "each name identifies the observable read, callback, species, or write boundary being awaited"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedArrayPrototypeFilterStage {
    NextElement,
    AwaitElement,
    AwaitCallback,
    AwaitConstructor,
    AwaitSpecies,
    AwaitConstruct,
    NextKeptValue,
}

/// Resumable `%TypedArray%.prototype.filter`.
///
/// `filter` reads and selects every source value before it observes the
/// species constructor. The retained values therefore remain rooted across
/// callback calls, species construction, and destination element conversion.
pub(super) struct TypedArrayPrototypeFilterState {
    source: ObjectId,
    source_length: usize,
    source_element: TypedArrayElementType,
    callback: FunctionId,
    this_argument: StoredValue,
    index: usize,
    element: Option<StoredValue>,
    kept: Vec<StoredValue>,
    target: Option<ObjectId>,
    write_index: usize,
    realm: RealmId,
    stage: TypedArrayPrototypeFilterStage,
    origin: JsStackFrame,
}

#[allow(
    clippy::enum_variant_names,
    reason = "each name identifies the observable relative-index or numeric-conversion boundary being awaited"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedArrayPrototypeWithStage {
    AwaitIndex,
    AwaitValue,
}

/// Resumable `%TypedArray%.prototype.with` across index and replacement-value
/// conversion. The replacement conversion deliberately precedes the final
/// `IsValidIntegerIndex` check, so a resizable buffer can change that check.
pub(super) struct TypedArrayPrototypeWithState {
    source: ObjectId,
    length: usize,
    value: StoredValue,
    actual_index: Option<usize>,
    element: TypedArrayElementType,
    realm: RealmId,
    stage: TypedArrayPrototypeWithStage,
    origin: JsStackFrame,
}

/// `%TypedArray%.prototype.at` after the initial validated length and before
/// `ToIntegerOrInfinity(index)` has completed.
pub(super) struct TypedArrayPrototypeAtState {
    object: ObjectId,
    length: usize,
    realm: RealmId,
    origin: JsStackFrame,
}

/// `%TypedArray%.prototype.includes` after its validated internal length and
/// before `ToIntegerOrInfinity(fromIndex)` has completed.
pub(super) struct TypedArrayPrototypeIncludesState {
    object: ObjectId,
    length: usize,
    needle: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

/// `%TypedArray%.prototype.indexOf` after its validated internal length and
/// before `ToIntegerOrInfinity(fromIndex)` has completed.
pub(super) struct TypedArrayPrototypeIndexOfState {
    object: ObjectId,
    length: usize,
    needle: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

/// `%TypedArray%.prototype.lastIndexOf` after its validated internal length
/// and before an explicitly supplied `fromIndex` has been converted.
pub(super) struct TypedArrayPrototypeLastIndexOfState {
    object: ObjectId,
    length: usize,
    needle: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

#[allow(
    clippy::enum_variant_names,
    reason = "each name identifies the observable conversion boundary being awaited"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedArrayPrototypeFillStage {
    AwaitValue,
    AwaitStart,
    AwaitEnd,
}

/// Resumable `%TypedArray%.prototype.fill` across the value, start, and end
/// conversions. The raw range operands remain rooted until their prescribed
/// conversion turn, while the converted element value is reused for every
/// store after the final resizable-view witness.
pub(super) struct TypedArrayPrototypeFillState {
    object: ObjectId,
    length: usize,
    value: StoredValue,
    start: StoredValue,
    end: StoredValue,
    start_index: usize,
    realm: RealmId,
    stage: TypedArrayPrototypeFillStage,
    origin: JsStackFrame,
}

#[allow(
    clippy::enum_variant_names,
    reason = "each name identifies the observable conversion boundary being awaited"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedArrayPrototypeCopyWithinStage {
    AwaitTarget,
    AwaitStart,
    AwaitEnd,
}

/// Resumable `%TypedArray%.prototype.copyWithin` across its three relative
/// index conversions. Its final byte copy happens only after the conditional
/// resizable-view witness required for a non-empty copied range.
pub(super) struct TypedArrayPrototypeCopyWithinState {
    object: ObjectId,
    length: usize,
    target: StoredValue,
    start: StoredValue,
    end: StoredValue,
    target_index: usize,
    start_index: usize,
    realm: RealmId,
    stage: TypedArrayPrototypeCopyWithinStage,
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

impl TypedArrayPrototypeSetState {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.target)));
        trace_stored_value_root(&self.source, mark);
    }
}

impl TypedArrayPrototypeSubarrayState {
    pub(super) const fn retained_values() -> u64 {
        3
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.source)));
        mark(CollectionRoot::Heap(HeapReference::Object(self.buffer)));
        trace_stored_value_root(&self.end, mark);
    }
}

impl TypedArrayPrototypeSliceState {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.source)));
        trace_stored_value_root(&self.end, mark);
    }
}

impl TypedArrayPrototypeMapState {
    pub(super) const fn retained_values() -> u64 {
        4
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.source)));
        mark(CollectionRoot::Heap(HeapReference::Function(self.callback)));
        trace_stored_value_root(&self.this_argument, mark);
        if let Some(target) = self.target {
            mark(CollectionRoot::Heap(HeapReference::Object(target)));
        }
    }
}

impl TypedArrayPrototypeFilterState {
    pub(super) fn retained_values(&self) -> u64 {
        3_u64
            .saturating_add(u64::from(self.element.is_some()))
            .saturating_add(usize_to_u64(self.kept.len()))
            .saturating_add(u64::from(self.target.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.source)));
        mark(CollectionRoot::Heap(HeapReference::Function(self.callback)));
        trace_stored_value_root(&self.this_argument, mark);
        if let Some(element) = &self.element {
            trace_stored_value_root(element, mark);
        }
        for value in &self.kept {
            trace_stored_value_root(value, mark);
        }
        if let Some(target) = self.target {
            mark(CollectionRoot::Heap(HeapReference::Object(target)));
        }
    }
}

impl TypedArrayPrototypeWithState {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.source)));
        trace_stored_value_root(&self.value, mark);
    }
}

impl TypedArrayPrototypeAtState {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.object)));
    }
}

impl TypedArrayPrototypeIncludesState {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.object)));
        trace_stored_value_root(&self.needle, mark);
    }
}

impl TypedArrayPrototypeIndexOfState {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.object)));
        trace_stored_value_root(&self.needle, mark);
    }
}

impl TypedArrayPrototypeLastIndexOfState {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.object)));
        trace_stored_value_root(&self.needle, mark);
    }
}

impl TypedArrayPrototypeFillState {
    pub(super) const fn retained_values() -> u64 {
        4
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.object)));
        trace_stored_value_root(&self.value, mark);
        trace_stored_value_root(&self.start, mark);
        trace_stored_value_root(&self.end, mark);
    }
}

impl TypedArrayPrototypeCopyWithinState {
    pub(super) const fn retained_values() -> u64 {
        4
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.object)));
        trace_stored_value_root(&self.target, mark);
        trace_stored_value_root(&self.start, mark);
        trace_stored_value_root(&self.end, mark);
    }
}

impl TypedArrayElementSetState {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(match &self.completion {
            TypedArraySetCompletion::Map(_) => TypedArrayPrototypeMapState::retained_values(),
            TypedArraySetCompletion::Filter(state) => state.retained_values(),
            TypedArraySetCompletion::LanguageWrite
            | TypedArraySetCompletion::ReflectSet
            | TypedArraySetCompletion::Define(_) => 0,
        })
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.object)));
        match &self.completion {
            TypedArraySetCompletion::Map(state) => state.trace_roots(mark),
            TypedArraySetCompletion::Filter(state) => state.trace_roots(mark),
            TypedArraySetCompletion::LanguageWrite
            | TypedArraySetCompletion::ReflectSet
            | TypedArraySetCompletion::Define(_) => {}
        }
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
    let Some(current_value) = runtime.typed_array_read_index(object, index)? else {
        return Ok(Some(TypedArrayDefineAction::Rejected));
    };
    if runtime.is_typed_array_backing_buffer_immutable(object)? {
        let current = OwnProperty::Data {
            layout: PropertyLayout::data(false, true, false),
            value: current_value,
        };
        return Ok(Some(
            match validate_and_apply_existing(definition, &current) {
                DefinitionDecision::Rejected => TypedArrayDefineAction::Rejected,
                DefinitionDecision::Unchanged => TypedArrayDefineAction::Complete,
                DefinitionDecision::Create(_) | DefinitionDecision::Replace(_) => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "immutable TypedArray descriptor validation requested mutation",
                    }
                    .into());
                }
            },
        ));
    }
    if definition.requested_configurable() == Some(false)
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
    clippy::never_loop,
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
                let done = typed_array_take_completion(&mut completion)?;
                if runtime.to_boolean(&done)? {
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
    typed_array_sequence_store_element(runtime, &mut state, value)?;
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
    loop {
        if state.index >= state.length {
            let target = state.target.ok_or(EngineFault::RuntimeInvariant {
                message: "TypedArray constructor completed without a target",
            })?;
            return Ok(NativeDispatch::Immediate(StoredValue::Object(target)));
        }
        let value = match state.stage {
            TypedArrayConstructorSequenceStage::AwaitIteratorValue => {
                state.values[state.index].duplicate()
            }
            TypedArrayConstructorSequenceStage::AwaitArrayLikeLengthConversion
            | TypedArrayConstructorSequenceStage::AwaitArrayLikeElement => {
                let index =
                    u64::try_from(state.index).map_err(|_| EngineFault::RuntimeInvariant {
                        message: "TypedArray array-like index does not fit u64",
                    })?;
                let key = array_static_index_key(runtime, index)?;
                state.stage = TypedArrayConstructorSequenceStage::AwaitArrayLikeElement;
                return typed_array_sequence_read(runtime, state, key, return_to, execution_budget);
            }
            _ => {
                return Err(EngineFault::RuntimeInvariant {
                    message:
                        "TypedArray constructor attempted element initialization from an invalid stage",
                }
                .into());
            }
        };
        if value.heap_reference().is_some() {
            return typed_array_sequence_begin_element_conversion(
                runtime,
                state,
                value,
                return_to,
                execution_budget,
            );
        }
        typed_array_sequence_store_element(runtime, &mut state, value)?;
    }
}

fn typed_array_sequence_store_element(
    runtime: &mut Runtime,
    state: &mut TypedArrayConstructorSequenceState,
    value: StoredValue,
) -> Result<(), NativeFailure> {
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
    Ok(())
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

#[allow(
    clippy::needless_pass_by_value,
    reason = "the conversion target intentionally transfers an owned continuation state"
)]
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

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the native dispatch follows the shared receiver, argument, return, origin, and budget calling convention"
)]
pub(super) fn dispatch_typed_array_prototype(
    runtime: &mut Runtime,
    method: TypedArrayPrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(object) = receiver else {
        if method == TypedArrayPrototypeMethod::ToStringTag {
            return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
        }
        return typed_array_type_error(realm, &origin, "not a TypedArray");
    };
    let Some(state) = runtime.typed_array_state(*object)?.copied() else {
        if method == TypedArrayPrototypeMethod::ToStringTag {
            return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
        }
        return typed_array_type_error(realm, &origin, "not a TypedArray");
    };
    if matches!(
        method,
        TypedArrayPrototypeMethod::Set
            | TypedArrayPrototypeMethod::Fill
            | TypedArrayPrototypeMethod::CopyWithin
            | TypedArrayPrototypeMethod::Reverse
            | TypedArrayPrototypeMethod::Sort
    ) && typed_array_buffer_is_immutable(runtime, state)?
    {
        return typed_array_type_error(realm, &origin, "TypedArray backing buffer is immutable");
    }
    let view = runtime.typed_array_view(*object)?;
    if matches!(
        method,
        TypedArrayPrototypeMethod::Sort | TypedArrayPrototypeMethod::ToSorted
    ) {
        return begin_typed_array_sort(
            runtime,
            *object,
            method == TypedArrayPrototypeMethod::ToSorted,
            arguments,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if matches!(
        method,
        TypedArrayPrototypeMethod::At
            | TypedArrayPrototypeMethod::Includes
            | TypedArrayPrototypeMethod::IndexOf
            | TypedArrayPrototypeMethod::LastIndexOf
            | TypedArrayPrototypeMethod::Fill
            | TypedArrayPrototypeMethod::CopyWithin
            | TypedArrayPrototypeMethod::Reverse
            | TypedArrayPrototypeMethod::Slice
            | TypedArrayPrototypeMethod::Entries
            | TypedArrayPrototypeMethod::Keys
            | TypedArrayPrototypeMethod::Values
            | TypedArrayPrototypeMethod::Join
            | TypedArrayPrototypeMethod::ToReversed
            | TypedArrayPrototypeMethod::With
            | TypedArrayPrototypeMethod::Every
            | TypedArrayPrototypeMethod::Filter
            | TypedArrayPrototypeMethod::Find
            | TypedArrayPrototypeMethod::FindIndex
            | TypedArrayPrototypeMethod::FindLast
            | TypedArrayPrototypeMethod::FindLastIndex
            | TypedArrayPrototypeMethod::ForEach
            | TypedArrayPrototypeMethod::Map
            | TypedArrayPrototypeMethod::Reduce
            | TypedArrayPrototypeMethod::ReduceRight
            | TypedArrayPrototypeMethod::Some
    ) && !matches!(view, TypedArrayView::InBounds { .. })
    {
        return typed_array_type_error(realm, &origin, "TypedArray is out of bounds");
    }
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
    let callback_method = match method {
        TypedArrayPrototypeMethod::Every => Some(ArrayCallback::Every),
        TypedArrayPrototypeMethod::Find => Some(ArrayCallback::Find),
        TypedArrayPrototypeMethod::FindIndex => Some(ArrayCallback::FindIndex),
        TypedArrayPrototypeMethod::FindLast => Some(ArrayCallback::FindLast),
        TypedArrayPrototypeMethod::FindLastIndex => Some(ArrayCallback::FindLastIndex),
        TypedArrayPrototypeMethod::ForEach => Some(ArrayCallback::ForEach),
        TypedArrayPrototypeMethod::Some => Some(ArrayCallback::Some),
        _ => None,
    };
    if let Some(callback_method) = callback_method {
        return begin_typed_array_callback(
            runtime,
            callback_method,
            realm,
            StoredValue::Object(*object),
            usize_to_u64(length),
            arguments,
            return_to,
            origin,
            execution_budget,
        );
    }
    let reduction = match method {
        TypedArrayPrototypeMethod::Reduce => Some(ArrayReduction::Reduce),
        TypedArrayPrototypeMethod::ReduceRight => Some(ArrayReduction::ReduceRight),
        _ => None,
    };
    if let Some(reduction) = reduction {
        return begin_typed_array_reduction(
            runtime,
            reduction,
            realm,
            StoredValue::Object(*object),
            length,
            arguments,
            return_to,
            origin,
            execution_budget,
        );
    }
    let number = |value: usize| {
        #[expect(
            clippy::cast_precision_loss,
            reason = "typed-array byte lengths and indices are bounded by ToIndex"
        )]
        let value = value as f64;
        StoredValue::Number(JsNumber::from_f64(value))
    };
    let value = match method {
        TypedArrayPrototypeMethod::Buffer => StoredValue::Object(state.buffer()),
        TypedArrayPrototypeMethod::ByteLength => number(byte_length),
        TypedArrayPrototypeMethod::ByteOffset => number(byte_offset),
        TypedArrayPrototypeMethod::Length => number(length),
        TypedArrayPrototypeMethod::ToStringTag => {
            StoredValue::String(JsString::from_utf8(typed_array_name(state.element()))?)
        }
        TypedArrayPrototypeMethod::Set => {
            return begin_typed_array_prototype_set(
                runtime,
                *object,
                arguments.take_first_or_undefined(),
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::Subarray => {
            return begin_typed_array_prototype_subarray(
                runtime,
                *object,
                arguments.take_first_or_undefined(),
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::At => {
            return begin_typed_array_prototype_at(
                runtime,
                *object,
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::Includes => {
            return begin_typed_array_prototype_includes(
                runtime,
                *object,
                arguments.take_first_or_undefined(),
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::IndexOf => {
            return begin_typed_array_prototype_index_of(
                runtime,
                *object,
                arguments.take_first_or_undefined(),
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::LastIndexOf => {
            return begin_typed_array_prototype_last_index_of(
                runtime,
                *object,
                arguments.take_first_or_undefined(),
                arguments.take_first(),
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::Fill => {
            return begin_typed_array_prototype_fill(
                runtime,
                *object,
                arguments.take_first_or_undefined(),
                arguments.take_first_or_undefined(),
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::CopyWithin => {
            return begin_typed_array_prototype_copy_within(
                runtime,
                *object,
                arguments.take_first_or_undefined(),
                arguments.take_first_or_undefined(),
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::Reverse => {
            return typed_array_prototype_reverse(
                runtime,
                *object,
                realm,
                &origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::Slice => {
            return begin_typed_array_prototype_slice(
                runtime,
                *object,
                arguments.take_first_or_undefined(),
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::Sort | TypedArrayPrototypeMethod::ToSorted => {
            unreachable!("typed-array sorting methods returned from their dedicated dispatch")
        }
        TypedArrayPrototypeMethod::Entries => {
            return begin_array_iterator_method(
                runtime,
                StoredValue::Object(*object),
                crate::object::ArrayIteratorKind::KeyAndValue,
                realm,
                origin,
            );
        }
        TypedArrayPrototypeMethod::Keys => {
            return begin_array_iterator_method(
                runtime,
                StoredValue::Object(*object),
                crate::object::ArrayIteratorKind::Key,
                realm,
                origin,
            );
        }
        TypedArrayPrototypeMethod::Values => {
            return begin_array_iterator_method(
                runtime,
                StoredValue::Object(*object),
                crate::object::ArrayIteratorKind::Value,
                realm,
                origin,
            );
        }
        TypedArrayPrototypeMethod::Join => {
            return begin_typed_array_join(
                runtime,
                realm,
                StoredValue::Object(*object),
                length,
                arguments.take_first(),
                return_to,
                origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::ToReversed => {
            return typed_array_prototype_to_reversed(
                runtime,
                *object,
                realm,
                &origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::With => {
            return begin_typed_array_prototype_with(
                runtime,
                *object,
                arguments.take_first_or_undefined(),
                arguments.take_first_or_undefined(),
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::Map => {
            return begin_typed_array_prototype_map(
                runtime,
                *object,
                length,
                state.element(),
                arguments,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::Filter => {
            return begin_typed_array_prototype_filter(
                runtime,
                *object,
                length,
                state.element(),
                arguments,
                realm,
                return_to,
                origin,
                execution_budget,
            );
        }
        TypedArrayPrototypeMethod::Every
        | TypedArrayPrototypeMethod::Find
        | TypedArrayPrototypeMethod::FindIndex
        | TypedArrayPrototypeMethod::FindLast
        | TypedArrayPrototypeMethod::FindLastIndex
        | TypedArrayPrototypeMethod::ForEach
        | TypedArrayPrototypeMethod::Reduce
        | TypedArrayPrototypeMethod::ReduceRight
        | TypedArrayPrototypeMethod::Some => {
            unreachable!("typed-array callback methods returned from their dedicated dispatch")
        }
    };
    Ok(NativeDispatch::Immediate(value))
}

fn typed_array_prototype_reverse(
    runtime: &mut Runtime,
    object: ObjectId,
    realm: RealmId,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (state, length) = typed_array_require_in_bounds(runtime, object, realm, origin)?;
    for left in 0..(length / 2) {
        execution_budget.charge_instructions(1)?;
        let right = length.saturating_sub(left + 1);
        let left_value =
            runtime
                .typed_array_read_index(object, left)?
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "TypedArray.prototype.reverse lost an in-bounds left element",
                })?;
        let right_value = runtime.typed_array_read_index(object, right)?.ok_or(
            EngineFault::RuntimeInvariant {
                message: "TypedArray.prototype.reverse lost an in-bounds right element",
            },
        )?;
        typed_array_reverse_store(runtime, object, left, right_value, state.element())?;
        typed_array_reverse_store(runtime, object, right, left_value, state.element())?;
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn typed_array_prototype_to_reversed(
    runtime: &mut Runtime,
    source: ObjectId,
    realm: RealmId,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (source_state, length) = typed_array_require_in_bounds(runtime, source, realm, origin)?;
    let target =
        typed_array_create_same_type(runtime, realm, source_state.element(), length, origin)?;
    for target_index in 0..length {
        execution_budget.charge_instructions(1)?;
        let source_index = length.saturating_sub(target_index + 1);
        let value = runtime
            .typed_array_read_index(source, source_index)?
            .ok_or(EngineFault::RuntimeInvariant {
                message: "TypedArray.prototype.toReversed lost a validated source element",
            })?;
        typed_array_reverse_store(runtime, target, target_index, value, source_state.element())?;
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(target)))
}

pub(super) fn typed_array_create_same_type(
    runtime: &mut Runtime,
    realm: RealmId,
    element: TypedArrayElementType,
    length: usize,
    origin: &JsStackFrame,
) -> Result<ObjectId, NativeFailure> {
    let Some(byte_length) = length.checked_mul(element.byte_width()) else {
        return typed_array_range_error(
            realm,
            origin,
            "TypedArray length exceeds implementation range",
        );
    };
    let buffer = runtime
        .allocate_array_buffer(
            HeapReference::Object(runtime.realm_array_buffer_prototype(realm)?),
            byte_length,
            None,
        )
        .map_err(NativeFailure::Execution)?;
    runtime
        .allocate_typed_array(
            HeapReference::Object(runtime.realm_typed_array_prototype(realm, element)?),
            TypedArrayState::new(buffer, 0, TypedArrayLength::Fixed(length), element),
        )
        .map_err(NativeFailure::Execution)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the native entry point preserves both uncoerced operands and the standard call context"
)]
fn begin_typed_array_prototype_with(
    runtime: &mut Runtime,
    source: ObjectId,
    index: StoredValue,
    value: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (source_state, length) = typed_array_require_in_bounds(runtime, source, realm, &origin)?;
    begin_operator_primitive_conversion(
        runtime,
        index,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayPrototypeWithIndex(Box::new(
            TypedArrayPrototypeWithState {
                source,
                length,
                value,
                actual_index: None,
                element: source_state.element(),
                realm,
                stage: TypedArrayPrototypeWithStage::AwaitIndex,
                origin: origin.clone(),
            },
        )),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_typed_array_prototype_with_index(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeWithState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    debug_assert_eq!(state.stage, TypedArrayPrototypeWithStage::AwaitIndex);
    let relative =
        number_to_integer_or_infinity(operator_to_number(value, state.realm, &state.origin)?);
    state.actual_index = typed_array_with_relative_index(relative, state.length);
    state.stage = TypedArrayPrototypeWithStage::AwaitValue;
    let value = state.value.duplicate();
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayPrototypeWithValue(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_typed_array_prototype_with_value(
    runtime: &mut Runtime,
    state: &TypedArrayPrototypeWithState,
    value: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    debug_assert_eq!(state.stage, TypedArrayPrototypeWithStage::AwaitValue);
    let replacement = if state.element.is_bigint() {
        StoredValue::BigInt(to_bigint_from_primitive(
            &value,
            state.realm,
            &state.origin,
        )?)
    } else {
        StoredValue::Number(operator_to_number(value, state.realm, &state.origin)?)
    };
    let Some(actual_index) = state.actual_index else {
        return typed_array_range_error(state.realm, &state.origin, "invalid TypedArray index");
    };
    let is_valid = matches!(
        runtime.typed_array_view(state.source)?,
        TypedArrayView::InBounds { length, .. } if actual_index < length
    );
    if !is_valid {
        return typed_array_range_error(state.realm, &state.origin, "invalid TypedArray index");
    }
    let target = typed_array_create_same_type(
        runtime,
        state.realm,
        state.element,
        state.length,
        &state.origin,
    )?;
    for index in 0..state.length {
        execution_budget.charge_instructions(1)?;
        let value = if index == actual_index {
            replacement.duplicate()
        } else {
            runtime
                .typed_array_read_index(state.source, index)?
                .unwrap_or(StoredValue::Undefined)
        };
        typed_array_with_store(runtime, target, index, value, state.element, state)?;
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(target)))
}

fn typed_array_with_store(
    runtime: &mut Runtime,
    target: ObjectId,
    index: usize,
    value: StoredValue,
    element: TypedArrayElementType,
    state: &TypedArrayPrototypeWithState,
) -> Result<(), NativeFailure> {
    let stored = if element.is_bigint() {
        let value = to_bigint_from_primitive(&value, state.realm, &state.origin)?;
        runtime.typed_array_store_index(
            target,
            index,
            TypedArrayElementValue::BigInt(value.as_ref()),
        )?
    } else {
        let value = operator_to_number(value, state.realm, &state.origin)?;
        runtime.typed_array_store_index(target, index, TypedArrayElementValue::Number(value))?
    };
    if stored != TypedArrayStoreOutcome::Stored {
        return Err(EngineFault::RuntimeInvariant {
            message: "TypedArray.prototype.with lost a validated destination element",
        }
        .into());
    }
    Ok(())
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "ToIntegerOrInfinity is finite here and is explicitly bounded before conversion to usize"
)]
fn typed_array_with_relative_index(relative: f64, length: usize) -> Option<usize> {
    if !relative.is_finite() {
        return None;
    }
    if relative >= 0.0 {
        if relative > usize::MAX as f64 {
            return None;
        }
        return Some(relative as usize);
    }
    let magnitude = -relative;
    if magnitude > length as f64 {
        return None;
    }
    length.checked_sub(magnitude as usize)
}

fn typed_array_reverse_store(
    runtime: &mut Runtime,
    object: ObjectId,
    index: usize,
    value: StoredValue,
    element: TypedArrayElementType,
) -> Result<(), NativeFailure> {
    let outcome = match value {
        StoredValue::Number(value) if !element.is_bigint() => {
            runtime.typed_array_store_index(object, index, TypedArrayElementValue::Number(value))?
        }
        StoredValue::BigInt(value) if element.is_bigint() => runtime.typed_array_store_index(
            object,
            index,
            TypedArrayElementValue::BigInt(value.as_ref()),
        )?,
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "TypedArray.prototype.reverse read an element with the wrong content type",
            }
            .into());
        }
    };
    if outcome != TypedArrayStoreOutcome::Stored {
        return Err(EngineFault::RuntimeInvariant {
            message: "TypedArray.prototype.reverse lost a validated destination element",
        }
        .into());
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "the native entry point preserves the uncoerced search value and fromIndex across conversion"
)]
fn begin_typed_array_prototype_includes(
    runtime: &mut Runtime,
    object: ObjectId,
    needle: StoredValue,
    from_index: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (_, length) = typed_array_require_in_bounds(runtime, object, realm, &origin)?;
    if length == 0 {
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
    }
    begin_operator_primitive_conversion(
        runtime,
        from_index,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayPrototypeIncludesFromIndex(Box::new(
            TypedArrayPrototypeIncludesState {
                object,
                length,
                needle,
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
    clippy::needless_pass_by_value,
    reason = "the primitive-conversion target transfers its owned continuation state"
)]
pub(super) fn finish_typed_array_prototype_includes_from_index(
    runtime: &mut Runtime,
    state: TypedArrayPrototypeIncludesState,
    value: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let relative =
        number_to_integer_or_infinity(operator_to_number(value, state.realm, &state.origin)?);
    let start = typed_array_relative_bound(relative, state.length);
    for index in start..state.length {
        execution_budget.charge_instructions(1)?;
        if runtime
            .typed_array_read_index(state.object, index)?
            .is_some_and(|element| state.needle.same_value_zero(&element))
        {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)));
        }
    }
    Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the native entry point preserves the uncoerced search value and fromIndex across conversion"
)]
fn begin_typed_array_prototype_index_of(
    runtime: &mut Runtime,
    object: ObjectId,
    needle: StoredValue,
    from_index: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (_, length) = typed_array_require_in_bounds(runtime, object, realm, &origin)?;
    if length == 0 {
        return Ok(NativeDispatch::Immediate(StoredValue::Number(
            JsNumber::from_i32(-1),
        )));
    }
    begin_operator_primitive_conversion(
        runtime,
        from_index,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayPrototypeIndexOfFromIndex(Box::new(
            TypedArrayPrototypeIndexOfState {
                object,
                length,
                needle,
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
    clippy::needless_pass_by_value,
    reason = "the primitive-conversion target transfers its owned continuation state"
)]
pub(super) fn finish_typed_array_prototype_index_of_from_index(
    runtime: &mut Runtime,
    state: TypedArrayPrototypeIndexOfState,
    value: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let relative =
        number_to_integer_or_infinity(operator_to_number(value, state.realm, &state.origin)?);
    let start = typed_array_relative_bound(relative, state.length);
    for index in start..state.length {
        execution_budget.charge_instructions(1)?;
        if runtime
            .typed_array_read_index(state.object, index)?
            .is_some_and(|element| state.needle.strict_equals(&element))
        {
            return Ok(NativeDispatch::Immediate(typed_array_usize_number(index)));
        }
    }
    Ok(NativeDispatch::Immediate(StoredValue::Number(
        JsNumber::from_i32(-1),
    )))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the native entry point preserves an absent fromIndex and the uncoerced search value"
)]
fn begin_typed_array_prototype_last_index_of(
    runtime: &mut Runtime,
    object: ObjectId,
    needle: StoredValue,
    from_index: Option<StoredValue>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (_, length) = typed_array_require_in_bounds(runtime, object, realm, &origin)?;
    if length == 0 {
        return Ok(typed_array_last_index_of_not_found());
    }
    let state = TypedArrayPrototypeLastIndexOfState {
        object,
        length,
        needle,
        realm,
        origin: origin.clone(),
    };
    let Some(from_index) = from_index else {
        return typed_array_last_index_of_search(runtime, &state, length - 1, execution_budget);
    };
    begin_operator_primitive_conversion(
        runtime,
        from_index,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayPrototypeLastIndexOfFromIndex(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the primitive-conversion target transfers its owned continuation state"
)]
pub(super) fn finish_typed_array_prototype_last_index_of_from_index(
    runtime: &mut Runtime,
    state: TypedArrayPrototypeLastIndexOfState,
    value: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let relative =
        number_to_integer_or_infinity(operator_to_number(value, state.realm, &state.origin)?);
    let Some(start) = typed_array_last_index_of_start(relative, state.length) else {
        return Ok(typed_array_last_index_of_not_found());
    };
    typed_array_last_index_of_search(runtime, &state, start, execution_budget)
}

fn typed_array_last_index_of_search(
    runtime: &mut Runtime,
    state: &TypedArrayPrototypeLastIndexOfState,
    start: usize,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    for index in (0..=start).rev() {
        execution_budget.charge_instructions(1)?;
        if runtime
            .typed_array_read_index(state.object, index)?
            .is_some_and(|element| state.needle.strict_equals(&element))
        {
            return Ok(NativeDispatch::Immediate(typed_array_usize_number(index)));
        }
    }
    Ok(typed_array_last_index_of_not_found())
}

fn typed_array_last_index_of_not_found() -> NativeDispatch {
    NativeDispatch::Immediate(StoredValue::Number(JsNumber::from_i32(-1)))
}

fn typed_array_last_index_of_start(relative: f64, length: usize) -> Option<usize> {
    if relative >= 0.0 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "ToIntegerOrInfinity yields a non-negative integer and large values clamp below"
        )]
        let index = relative as usize;
        return Some(index.min(length - 1));
    }
    if relative.is_infinite() {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "ToIntegerOrInfinity yields a finite negative integer"
    )]
    let magnitude = (-relative) as usize;
    if magnitude > length {
        return None;
    }
    Some(length - magnitude)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the native entry point preserves value, start, and end until their mandated conversion turns"
)]
fn begin_typed_array_prototype_fill(
    runtime: &mut Runtime,
    object: ObjectId,
    value: StoredValue,
    start: StoredValue,
    end: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (_, length) = typed_array_require_in_bounds(runtime, object, realm, &origin)?;
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayPrototypeFill(Box::new(TypedArrayPrototypeFillState {
            object,
            length,
            value: StoredValue::Undefined,
            start,
            end,
            start_index: 0,
            realm,
            stage: TypedArrayPrototypeFillStage::AwaitValue,
            origin: origin.clone(),
        })),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the primitive-conversion target transfers its owned continuation state"
)]
pub(super) fn finish_typed_array_prototype_fill(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeFillState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TypedArrayPrototypeFillStage::AwaitValue => {
            let element = runtime
                .typed_array_state(state.object)?
                .copied()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "TypedArray.prototype.fill receiver lost its internal slots",
                })?
                .element();
            state.value = if element.is_bigint() {
                StoredValue::BigInt(to_bigint_from_primitive(
                    &value,
                    state.realm,
                    &state.origin,
                )?)
            } else {
                StoredValue::Number(operator_to_number(value, state.realm, &state.origin)?)
            };
            state.stage = TypedArrayPrototypeFillStage::AwaitStart;
            let start = std::mem::replace(&mut state.start, StoredValue::Undefined);
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                start,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::TypedArrayPrototypeFill(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TypedArrayPrototypeFillStage::AwaitStart => {
            let relative = number_to_integer_or_infinity(operator_to_number(
                value,
                state.realm,
                &state.origin,
            )?);
            state.start_index = typed_array_relative_bound(relative, state.length);
            state.stage = TypedArrayPrototypeFillStage::AwaitEnd;
            let end = std::mem::replace(&mut state.end, StoredValue::Undefined);
            if matches!(end, StoredValue::Undefined) {
                return typed_array_prototype_fill_stored(
                    runtime,
                    &state,
                    state.length,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                end,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::TypedArrayPrototypeFill(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TypedArrayPrototypeFillStage::AwaitEnd => {
            let relative = number_to_integer_or_infinity(operator_to_number(
                value,
                state.realm,
                &state.origin,
            )?);
            let end_index = typed_array_relative_bound(relative, state.length);
            typed_array_prototype_fill_stored(runtime, &state, end_index, execution_budget)
        }
    }
}

fn typed_array_prototype_fill_stored(
    runtime: &mut Runtime,
    state: &TypedArrayPrototypeFillState,
    end_index: usize,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (target, final_length) =
        typed_array_require_in_bounds(runtime, state.object, state.realm, &state.origin)?;
    let end_index = end_index.min(final_length);
    for index in state.start_index..end_index {
        execution_budget.charge_instructions(1)?;
        let outcome = match (&state.value, target.element().is_bigint()) {
            (StoredValue::Number(value), false) => runtime.typed_array_store_index(
                state.object,
                index,
                TypedArrayElementValue::Number(*value),
            )?,
            (StoredValue::BigInt(value), true) => runtime.typed_array_store_index(
                state.object,
                index,
                TypedArrayElementValue::BigInt(value.as_ref()),
            )?,
            _ => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "TypedArray.prototype.fill converted the wrong content type",
                }
                .into());
            }
        };
        if outcome != TypedArrayStoreOutcome::Stored {
            return Err(EngineFault::RuntimeInvariant {
                message: "TypedArray.prototype.fill lost a validated destination element",
            }
            .into());
        }
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(state.object)))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the native entry point preserves target, start, and end until their mandated conversion turns"
)]
fn begin_typed_array_prototype_copy_within(
    runtime: &mut Runtime,
    object: ObjectId,
    target: StoredValue,
    start: StoredValue,
    end: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (_, length) = typed_array_require_in_bounds(runtime, object, realm, &origin)?;
    begin_operator_primitive_conversion(
        runtime,
        target,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayPrototypeCopyWithin(Box::new(
            TypedArrayPrototypeCopyWithinState {
                object,
                length,
                target: StoredValue::Undefined,
                start,
                end,
                target_index: 0,
                start_index: 0,
                realm,
                stage: TypedArrayPrototypeCopyWithinStage::AwaitTarget,
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
    clippy::needless_pass_by_value,
    reason = "the primitive-conversion target transfers its owned continuation state"
)]
pub(super) fn finish_typed_array_prototype_copy_within(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeCopyWithinState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TypedArrayPrototypeCopyWithinStage::AwaitTarget => {
            let relative = number_to_integer_or_infinity(operator_to_number(
                value,
                state.realm,
                &state.origin,
            )?);
            state.target_index = typed_array_relative_bound(relative, state.length);
            state.stage = TypedArrayPrototypeCopyWithinStage::AwaitStart;
            let start = std::mem::replace(&mut state.start, StoredValue::Undefined);
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                start,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::TypedArrayPrototypeCopyWithin(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TypedArrayPrototypeCopyWithinStage::AwaitStart => {
            let relative = number_to_integer_or_infinity(operator_to_number(
                value,
                state.realm,
                &state.origin,
            )?);
            state.start_index = typed_array_relative_bound(relative, state.length);
            state.stage = TypedArrayPrototypeCopyWithinStage::AwaitEnd;
            let end = std::mem::replace(&mut state.end, StoredValue::Undefined);
            if matches!(end, StoredValue::Undefined) {
                return typed_array_prototype_copy_within_bytes(
                    runtime,
                    &state,
                    state.length,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                end,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::TypedArrayPrototypeCopyWithin(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TypedArrayPrototypeCopyWithinStage::AwaitEnd => {
            let relative = number_to_integer_or_infinity(operator_to_number(
                value,
                state.realm,
                &state.origin,
            )?);
            let end_index = typed_array_relative_bound(relative, state.length);
            typed_array_prototype_copy_within_bytes(runtime, &state, end_index, execution_budget)
        }
    }
}

fn typed_array_prototype_copy_within_bytes(
    runtime: &mut Runtime,
    state: &TypedArrayPrototypeCopyWithinState,
    end_index: usize,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let count = end_index
        .saturating_sub(state.start_index)
        .min(state.length.saturating_sub(state.target_index));
    if count == 0 {
        return Ok(NativeDispatch::Immediate(StoredValue::Object(state.object)));
    }
    let (target, final_length) =
        typed_array_require_in_bounds(runtime, state.object, state.realm, &state.origin)?;
    let count = count
        .min(final_length.saturating_sub(state.start_index))
        .min(final_length.saturating_sub(state.target_index));
    if count == 0 {
        return Ok(NativeDispatch::Immediate(StoredValue::Object(state.object)));
    }
    let width = target.element().byte_width();
    let byte_count = count
        .checked_mul(width)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "TypedArray.prototype.copyWithin byte count overflowed after range validation",
        })?;
    let source_offset = target
        .byte_offset()
        .checked_add(
            state
                .start_index
                .checked_mul(width)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "TypedArray.prototype.copyWithin source offset overflowed after range validation",
                })?,
        )
        .ok_or(EngineFault::RuntimeInvariant {
            message: "TypedArray.prototype.copyWithin source offset overflowed after range validation",
        })?;
    let target_offset = target
        .byte_offset()
        .checked_add(
            state
                .target_index
                .checked_mul(width)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "TypedArray.prototype.copyWithin target offset overflowed after range validation",
                })?,
        )
        .ok_or(EngineFault::RuntimeInvariant {
            message: "TypedArray.prototype.copyWithin target offset overflowed after range validation",
        })?;
    execution_budget.charge_instructions(1)?;
    runtime.copy_array_buffer_bytes_to(
        target.buffer(),
        source_offset,
        target.buffer(),
        target_offset,
        byte_count,
    )?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(state.object)))
}

fn begin_typed_array_prototype_at(
    runtime: &mut Runtime,
    object: ObjectId,
    index: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (_, length) = typed_array_require_in_bounds(runtime, object, realm, &origin)?;
    begin_operator_primitive_conversion(
        runtime,
        index,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayPrototypeAtIndex(Box::new(TypedArrayPrototypeAtState {
            object,
            length,
            realm,
            origin: origin.clone(),
        })),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the primitive-conversion target transfers its owned continuation state"
)]
pub(super) fn finish_typed_array_prototype_at_index(
    runtime: &Runtime,
    state: TypedArrayPrototypeAtState,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let relative =
        number_to_integer_or_infinity(operator_to_number(value, state.realm, &state.origin)?);
    let Some(index) = typed_array_at_index(relative, state.length) else {
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    };
    Ok(NativeDispatch::Immediate(
        runtime
            .typed_array_read_index(state.object, index)?
            .unwrap_or(StoredValue::Undefined),
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the native entry point preserves both relative-index operands and the standard call context"
)]
fn begin_typed_array_prototype_subarray(
    runtime: &mut Runtime,
    source: ObjectId,
    begin: StoredValue,
    end: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(source_state) = runtime.typed_array_state(source)?.copied() else {
        return typed_array_type_error(realm, &origin, "not a TypedArray");
    };
    let source_length = match runtime.typed_array_view(source)? {
        TypedArrayView::InBounds { length, .. } => length,
        TypedArrayView::Detached | TypedArrayView::OutOfBounds => 0,
    };
    begin_operator_primitive_conversion(
        runtime,
        begin,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayPrototypeSubarrayBegin(Box::new(
            TypedArrayPrototypeSubarrayState {
                source,
                buffer: source_state.buffer(),
                source_byte_offset: source_state.byte_offset(),
                source_length,
                begin: 0,
                new_length: 0,
                length_tracking: matches!(source_state.length(), TypedArrayLength::Auto),
                end,
                element: source_state.element(),
                realm,
                stage: TypedArrayPrototypeSubarrayStage::AwaitConstructor,
                origin: origin.clone(),
            },
        )),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_typed_array_prototype_subarray_begin(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeSubarrayState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let relative =
        number_to_integer_or_infinity(operator_to_number(value, state.realm, &state.origin)?);
    state.begin = typed_array_relative_bound(relative, state.source_length);
    if matches!(state.end, StoredValue::Undefined) {
        state.new_length = state.source_length.saturating_sub(state.begin);
        return begin_typed_array_subarray_constructor_get(
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
        OperatorPrimitiveTarget::TypedArrayPrototypeSubarrayEnd(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_typed_array_prototype_subarray_end(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeSubarrayState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let relative =
        number_to_integer_or_infinity(operator_to_number(value, state.realm, &state.origin)?);
    let end = typed_array_relative_bound(relative, state.source_length);
    state.new_length = end.saturating_sub(state.begin);
    begin_typed_array_subarray_constructor_get(runtime, state, return_to, execution_budget)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "native continuation completion values are transferred by value across every stage"
)]
pub(super) fn advance_typed_array_prototype_subarray(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeSubarrayState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TypedArrayPrototypeSubarrayStage::AwaitConstructor => {
            if let StoredValue::Function(function) = value
                && function_is_constructor(runtime, function)?
            {
                let function_realm = runtime.function_realm(function)?;
                if function_realm != state.realm
                    && function
                        == runtime.realm_typed_array_constructor(function_realm, state.element)?
                {
                    let constructor =
                        runtime.realm_typed_array_constructor(state.realm, state.element)?;
                    return begin_typed_array_subarray_construct(state, constructor, return_to);
                }
            }
            if matches!(value, StoredValue::Undefined) {
                let constructor =
                    runtime.realm_typed_array_constructor(state.realm, state.element)?;
                return begin_typed_array_subarray_construct(state, constructor, return_to);
            }
            if !matches!(value, StoredValue::Object(_) | StoredValue::Function(_)) {
                return typed_array_type_error(state.realm, &state.origin, "not a constructor");
            }
            state.stage = TypedArrayPrototypeSubarrayStage::AwaitSpecies;
            begin_typed_array_subarray_species_get(
                runtime,
                state,
                &value,
                return_to,
                execution_budget,
            )
        }
        TypedArrayPrototypeSubarrayStage::AwaitSpecies => {
            let constructor = if matches!(value, StoredValue::Undefined | StoredValue::Null) {
                runtime.realm_typed_array_constructor(state.realm, state.element)?
            } else if let StoredValue::Function(function) = value {
                if !function_is_constructor(runtime, function)? {
                    return typed_array_type_error(state.realm, &state.origin, "not a constructor");
                }
                function
            } else {
                return typed_array_type_error(state.realm, &state.origin, "not a constructor");
            };
            begin_typed_array_subarray_construct(state, constructor, return_to)
        }
        TypedArrayPrototypeSubarrayStage::AwaitConstruct => {
            let StoredValue::Object(result) = value else {
                return typed_array_type_error(
                    state.realm,
                    &state.origin,
                    "TypedArray species constructor returned a non-TypedArray",
                );
            };
            let Some(result_state) = runtime.typed_array_state(result)?.copied() else {
                return typed_array_type_error(
                    state.realm,
                    &state.origin,
                    "TypedArray species constructor returned a non-TypedArray",
                );
            };
            if result_state.element().is_bigint() != state.element.is_bigint() {
                return typed_array_type_error(
                    state.realm,
                    &state.origin,
                    "TypedArray species constructor returned a different content type",
                );
            }
            Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
        }
    }
}

fn begin_typed_array_subarray_constructor_get(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeSubarrayState,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = TypedArrayPrototypeSubarrayStage::AwaitConstructor;
    let source = StoredValue::Object(state.source);
    charge_heap_property_lookup(runtime, &source, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        &source,
        runtime.predefined_property_key(PredefinedAtom::Constructor),
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        typed_array_prototype_subarray_continuation,
        |state, value| {
            advance_typed_array_prototype_subarray(
                runtime,
                state,
                value,
                return_to,
                execution_budget,
            )
        },
        "TypedArray.prototype.subarray constructor Get produced a structured result",
    )
}

fn begin_typed_array_subarray_species_get(
    runtime: &mut Runtime,
    state: TypedArrayPrototypeSubarrayState,
    constructor: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_heap_property_lookup(runtime, constructor, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        constructor,
        runtime.predefined_symbol_property_key(PredefinedAtom::SymbolSpecies),
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        typed_array_prototype_subarray_continuation,
        |state, value| {
            advance_typed_array_prototype_subarray(
                runtime,
                state,
                value,
                return_to,
                execution_budget,
            )
        },
        "TypedArray.prototype.subarray species Get produced a structured result",
    )
}

fn begin_typed_array_subarray_construct(
    mut state: TypedArrayPrototypeSubarrayState,
    constructor: FunctionId,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let element_width = state.element.byte_width();
    let byte_offset = state
        .source_byte_offset
        .checked_add(state.begin.checked_mul(element_width).ok_or(
            EngineFault::RuntimeInvariant {
                message: "TypedArray subarray byte offset overflowed after relative bounds",
            },
        )?)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "TypedArray subarray byte offset overflowed after relative bounds",
        })?;
    state.stage = TypedArrayPrototypeSubarrayStage::AwaitConstruct;
    let preserves_length_tracking =
        state.length_tracking && matches!(state.end, StoredValue::Undefined);
    let argument_count = if preserves_length_tracking { 2 } else { 3 };
    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(argument_count)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: argument_count,
        })?;
    arguments.push(StoredValue::Object(state.buffer));
    arguments.push(typed_array_usize_number(byte_offset));
    if !preserves_length_tracking {
        arguments.push(typed_array_usize_number(state.new_length));
    }
    let origin = state.origin.clone();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(typed_array_prototype_subarray_continuation(state));
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

fn typed_array_prototype_subarray_continuation(
    state: TypedArrayPrototypeSubarrayState,
) -> NativeContinuation {
    NativeContinuation::TypedArrayPrototypeSubarray(Box::new(state))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the native entry point preserves both relative-index operands and the standard call context"
)]
fn begin_typed_array_prototype_slice(
    runtime: &mut Runtime,
    source: ObjectId,
    start: StoredValue,
    end: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (source_state, source_length) =
        typed_array_require_in_bounds(runtime, source, realm, &origin)?;
    begin_operator_primitive_conversion(
        runtime,
        start,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayPrototypeSliceStart(Box::new(
            TypedArrayPrototypeSliceState {
                source,
                source_length,
                start: 0,
                end,
                end_index: 0,
                count: 0,
                element: source_state.element(),
                realm,
                stage: TypedArrayPrototypeSliceStage::AwaitConstructor,
                origin: origin.clone(),
            },
        )),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_typed_array_prototype_slice_start(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeSliceState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let relative =
        number_to_integer_or_infinity(operator_to_number(value, state.realm, &state.origin)?);
    state.start = typed_array_relative_bound(relative, state.source_length);
    if matches!(state.end, StoredValue::Undefined) {
        state.end_index = state.source_length;
        state.count = state.source_length.saturating_sub(state.start);
        return begin_typed_array_slice_constructor_get(
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
        OperatorPrimitiveTarget::TypedArrayPrototypeSliceEnd(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_typed_array_prototype_slice_end(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeSliceState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let relative =
        number_to_integer_or_infinity(operator_to_number(value, state.realm, &state.origin)?);
    state.end_index = typed_array_relative_bound(relative, state.source_length);
    state.count = state.end_index.saturating_sub(state.start);
    begin_typed_array_slice_constructor_get(runtime, state, return_to, execution_budget)
}

#[expect(
    clippy::too_many_lines,
    reason = "the species protocol and post-construction copy must preserve one ordered fresh-witness state machine"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "native continuation completion values are transferred by value across every stage"
)]
pub(super) fn advance_typed_array_prototype_slice(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeSliceState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TypedArrayPrototypeSliceStage::AwaitConstructor => {
            if let StoredValue::Function(function) = value
                && function_is_constructor(runtime, function)?
            {
                let function_realm = runtime.function_realm(function)?;
                if function_realm != state.realm
                    && function
                        == runtime.realm_typed_array_constructor(function_realm, state.element)?
                {
                    let constructor =
                        runtime.realm_typed_array_constructor(state.realm, state.element)?;
                    return begin_typed_array_slice_construct(state, constructor, return_to);
                }
            }
            if matches!(value, StoredValue::Undefined) {
                let constructor =
                    runtime.realm_typed_array_constructor(state.realm, state.element)?;
                return begin_typed_array_slice_construct(state, constructor, return_to);
            }
            if !matches!(value, StoredValue::Object(_) | StoredValue::Function(_)) {
                return typed_array_type_error(state.realm, &state.origin, "not a constructor");
            }
            state.stage = TypedArrayPrototypeSliceStage::AwaitSpecies;
            begin_typed_array_slice_species_get(runtime, state, &value, return_to, execution_budget)
        }
        TypedArrayPrototypeSliceStage::AwaitSpecies => {
            let constructor = if matches!(value, StoredValue::Undefined | StoredValue::Null) {
                runtime.realm_typed_array_constructor(state.realm, state.element)?
            } else if let StoredValue::Function(function) = value {
                if !function_is_constructor(runtime, function)? {
                    return typed_array_type_error(state.realm, &state.origin, "not a constructor");
                }
                function
            } else {
                return typed_array_type_error(state.realm, &state.origin, "not a constructor");
            };
            begin_typed_array_slice_construct(state, constructor, return_to)
        }
        TypedArrayPrototypeSliceStage::AwaitConstruct => {
            let StoredValue::Object(result) = value else {
                return typed_array_type_error(
                    state.realm,
                    &state.origin,
                    "TypedArray species constructor returned a non-TypedArray",
                );
            };
            let (target_state, target_length) = typed_array_require_writable_in_bounds(
                runtime,
                result,
                state.realm,
                &state.origin,
            )?;
            if target_length < state.count {
                return typed_array_type_error(
                    state.realm,
                    &state.origin,
                    "TypedArray species constructor returned a too-short TypedArray",
                );
            }
            if target_state.element().is_bigint() != state.element.is_bigint() {
                return typed_array_type_error(
                    state.realm,
                    &state.origin,
                    "TypedArray species constructor returned a different content type",
                );
            }
            if state.count == 0 {
                return Ok(NativeDispatch::Immediate(StoredValue::Object(result)));
            }

            let (source_state, source_length) =
                typed_array_require_in_bounds(runtime, state.source, state.realm, &state.origin)?;
            let end = state.end_index.min(source_length);
            let actual_count = end.saturating_sub(state.start);
            if actual_count == 0 {
                return Ok(NativeDispatch::Immediate(StoredValue::Object(result)));
            }
            if source_state.element() == target_state.element() {
                let byte_width = source_state.element().byte_width();
                let source_offset = source_state
                    .byte_offset()
                    .checked_add(state.start.checked_mul(byte_width).ok_or(
                        EngineFault::RuntimeInvariant {
                            message: "TypedArray.prototype.slice source byte offset overflowed",
                        },
                    )?)
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "TypedArray.prototype.slice source byte offset overflowed",
                    })?;
                let byte_count =
                    actual_count
                        .checked_mul(byte_width)
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "TypedArray.prototype.slice byte count overflowed",
                        })?;
                runtime.copy_array_buffer_bytes_forward(
                    source_state.buffer(),
                    source_offset,
                    target_state.buffer(),
                    target_state.byte_offset(),
                    byte_count,
                )?;
            } else {
                for index in 0..actual_count {
                    execution_budget.charge_instructions(1)?;
                    let source_index =
                        state
                            .start
                            .checked_add(index)
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "TypedArray.prototype.slice source index overflowed",
                            })?;
                    let value = runtime
                        .typed_array_read_index(state.source, source_index)?
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "TypedArray.prototype.slice lost a fresh source element",
                        })?;
                    let stored = match value {
                        StoredValue::Number(value) => runtime.typed_array_store_index(
                            result,
                            index,
                            TypedArrayElementValue::Number(value),
                        )?,
                        StoredValue::BigInt(value) => runtime.typed_array_store_index(
                            result,
                            index,
                            TypedArrayElementValue::BigInt(value.as_ref()),
                        )?,
                        _ => {
                            return Err(EngineFault::RuntimeInvariant {
                                message: "TypedArray.prototype.slice read an element with the wrong content type",
                            }
                            .into());
                        }
                    };
                    if stored != TypedArrayStoreOutcome::Stored {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "TypedArray.prototype.slice lost a validated destination element",
                        }
                        .into());
                    }
                }
            }
            Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
        }
    }
}

fn begin_typed_array_slice_constructor_get(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeSliceState,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = TypedArrayPrototypeSliceStage::AwaitConstructor;
    let source = StoredValue::Object(state.source);
    charge_heap_property_lookup(runtime, &source, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        &source,
        runtime.predefined_property_key(PredefinedAtom::Constructor),
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        typed_array_prototype_slice_continuation,
        |state, value| {
            advance_typed_array_prototype_slice(runtime, state, value, return_to, execution_budget)
        },
        "TypedArray.prototype.slice constructor Get produced a structured result",
    )
}

fn begin_typed_array_slice_species_get(
    runtime: &mut Runtime,
    state: TypedArrayPrototypeSliceState,
    constructor: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_heap_property_lookup(runtime, constructor, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        constructor,
        runtime.predefined_symbol_property_key(PredefinedAtom::SymbolSpecies),
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        typed_array_prototype_slice_continuation,
        |state, value| {
            advance_typed_array_prototype_slice(runtime, state, value, return_to, execution_budget)
        },
        "TypedArray.prototype.slice species Get produced a structured result",
    )
}

fn begin_typed_array_slice_construct(
    mut state: TypedArrayPrototypeSliceState,
    constructor: FunctionId,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = TypedArrayPrototypeSliceStage::AwaitConstruct;
    let count = state.count;
    let origin = state.origin.clone();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(typed_array_prototype_slice_continuation(state));
    Ok(NativeDispatch::Call(NativeCall {
        function: constructor,
        receiver: StoredValue::Undefined,
        arguments: CallArguments::from_values(vec![typed_array_usize_number(count)]),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: Some(constructor),
        native_caller: None,
    }))
}

fn typed_array_prototype_slice_continuation(
    state: TypedArrayPrototypeSliceState,
) -> NativeContinuation {
    NativeContinuation::TypedArrayPrototypeSlice(Box::new(state))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the native entry preserves the fixed source witness, callback arguments, and standard call context"
)]
fn begin_typed_array_prototype_map(
    runtime: &mut Runtime,
    source: ObjectId,
    source_length: usize,
    source_element: TypedArrayElementType,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(callback) = arguments.take_first_or_undefined() else {
        return typed_array_type_error(realm, &origin, "not a function");
    };
    begin_typed_array_map_constructor_get(
        runtime,
        TypedArrayPrototypeMapState {
            source,
            source_length,
            source_element,
            target: None,
            callback,
            this_argument: arguments.take_first_or_undefined(),
            index: 0,
            realm,
            stage: TypedArrayPrototypeMapStage::AwaitConstructor,
            origin,
        },
        return_to,
        execution_budget,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "the species protocol, callback loop, and conversion-resuming indexed writes are one ordered map state machine"
)]
pub(super) fn advance_typed_array_prototype_map(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeMapState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TypedArrayPrototypeMapStage::AwaitConstructor => {
            if let StoredValue::Function(function) = value
                && function_is_constructor(runtime, function)?
            {
                let function_realm = runtime.function_realm(function)?;
                if function_realm != state.realm
                    && function
                        == runtime
                            .realm_typed_array_constructor(function_realm, state.source_element)?
                {
                    let constructor =
                        runtime.realm_typed_array_constructor(state.realm, state.source_element)?;
                    return begin_typed_array_map_construct(state, constructor, return_to);
                }
            }
            if matches!(value, StoredValue::Undefined) {
                let constructor =
                    runtime.realm_typed_array_constructor(state.realm, state.source_element)?;
                return begin_typed_array_map_construct(state, constructor, return_to);
            }
            if !matches!(value, StoredValue::Object(_) | StoredValue::Function(_)) {
                return typed_array_type_error(state.realm, &state.origin, "not a constructor");
            }
            state.stage = TypedArrayPrototypeMapStage::AwaitSpecies;
            begin_typed_array_map_species_get(runtime, state, &value, return_to, execution_budget)
        }
        TypedArrayPrototypeMapStage::AwaitSpecies => {
            let constructor = if matches!(value, StoredValue::Undefined | StoredValue::Null) {
                runtime.realm_typed_array_constructor(state.realm, state.source_element)?
            } else if let StoredValue::Function(function) = value {
                if !function_is_constructor(runtime, function)? {
                    return typed_array_type_error(state.realm, &state.origin, "not a constructor");
                }
                function
            } else {
                return typed_array_type_error(state.realm, &state.origin, "not a constructor");
            };
            begin_typed_array_map_construct(state, constructor, return_to)
        }
        TypedArrayPrototypeMapStage::AwaitConstruct => {
            let StoredValue::Object(target) = value else {
                return typed_array_type_error(
                    state.realm,
                    &state.origin,
                    "TypedArray species constructor returned a non-TypedArray",
                );
            };
            let (target_state, target_length) = typed_array_require_writable_in_bounds(
                runtime,
                target,
                state.realm,
                &state.origin,
            )?;
            if target_length < state.source_length {
                return typed_array_type_error(
                    state.realm,
                    &state.origin,
                    "TypedArray species constructor returned a too-short TypedArray",
                );
            }
            if target_state.element().is_bigint() != state.source_element.is_bigint() {
                return typed_array_type_error(
                    state.realm,
                    &state.origin,
                    "TypedArray species constructor returned a different content type",
                );
            }
            state.target = Some(target);
            state.stage = TypedArrayPrototypeMapStage::NextElement;
            advance_typed_array_prototype_map(
                runtime,
                state,
                StoredValue::Undefined,
                return_to,
                execution_budget,
            )
        }
        TypedArrayPrototypeMapStage::NextElement => {
            if state.index >= state.source_length {
                let target = state.target.ok_or(EngineFault::RuntimeInvariant {
                    message: "TypedArray.prototype.map lost its constructed target",
                })?;
                return Ok(NativeDispatch::Immediate(StoredValue::Object(target)));
            }
            execution_budget.charge_instructions(1)?;
            let key = array_static_index_key(runtime, usize_to_u64(state.index))?;
            let source = StoredValue::Object(state.source);
            charge_heap_property_lookup(runtime, &source, execution_budget)?;
            state.stage = TypedArrayPrototypeMapStage::AwaitElement;
            let dispatch = begin_value_get(
                runtime,
                &source,
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
                typed_array_prototype_map_continuation,
                |state, value| {
                    advance_typed_array_prototype_map(
                        runtime,
                        state,
                        value,
                        return_to,
                        execution_budget,
                    )
                },
                "TypedArray.prototype.map source Get produced a structured result",
            )
        }
        TypedArrayPrototypeMapStage::AwaitElement => {
            let mut callback_arguments = Vec::new();
            callback_arguments.try_reserve_exact(3).map_err(|_| {
                ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 3,
                }
            })?;
            callback_arguments.push(value);
            callback_arguments.push(typed_array_usize_number(state.index));
            callback_arguments.push(StoredValue::Object(state.source));
            state.stage = TypedArrayPrototypeMapStage::AwaitCallback;
            let callback = state.callback;
            let receiver = state.this_argument.duplicate();
            suspend_typed_array_map(state, callback, receiver, callback_arguments, return_to)
        }
        TypedArrayPrototypeMapStage::AwaitCallback => {
            let target = state.target.ok_or(EngineFault::RuntimeInvariant {
                message: "TypedArray.prototype.map lost its constructed target",
            })?;
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_typed_array_element_set(
                runtime,
                target,
                TypedArrayPropertyKey::Index(state.index),
                value,
                TypedArraySetCompletion::Map(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

pub(super) fn resume_typed_array_prototype_map_after_store(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeMapState,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.index = state
        .index
        .checked_add(1)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "TypedArray.prototype.map index overflowed within its captured length",
        })?;
    state.stage = TypedArrayPrototypeMapStage::NextElement;
    advance_typed_array_prototype_map(
        runtime,
        state,
        StoredValue::Undefined,
        return_to,
        execution_budget,
    )
}

fn begin_typed_array_map_constructor_get(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeMapState,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = TypedArrayPrototypeMapStage::AwaitConstructor;
    let source = StoredValue::Object(state.source);
    charge_heap_property_lookup(runtime, &source, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        &source,
        runtime.predefined_property_key(PredefinedAtom::Constructor),
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        typed_array_prototype_map_continuation,
        |state, value| {
            advance_typed_array_prototype_map(runtime, state, value, return_to, execution_budget)
        },
        "TypedArray.prototype.map constructor Get produced a structured result",
    )
}

fn begin_typed_array_map_species_get(
    runtime: &mut Runtime,
    state: TypedArrayPrototypeMapState,
    constructor: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_heap_property_lookup(runtime, constructor, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        constructor,
        runtime.predefined_symbol_property_key(PredefinedAtom::SymbolSpecies),
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        typed_array_prototype_map_continuation,
        |state, value| {
            advance_typed_array_prototype_map(runtime, state, value, return_to, execution_budget)
        },
        "TypedArray.prototype.map species Get produced a structured result",
    )
}

fn begin_typed_array_map_construct(
    mut state: TypedArrayPrototypeMapState,
    constructor: FunctionId,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = TypedArrayPrototypeMapStage::AwaitConstruct;
    let origin = state.origin.clone();
    let source_length = state.source_length;
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(typed_array_prototype_map_continuation(state));
    Ok(NativeDispatch::Call(NativeCall {
        function: constructor,
        receiver: StoredValue::Undefined,
        arguments: CallArguments::from_values(vec![typed_array_usize_number(source_length)]),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: Some(constructor),
        native_caller: None,
    }))
}

fn typed_array_prototype_map_continuation(
    state: TypedArrayPrototypeMapState,
) -> NativeContinuation {
    NativeContinuation::TypedArrayPrototypeMap(Box::new(state))
}

fn suspend_typed_array_map(
    state: TypedArrayPrototypeMapState,
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
    continuations.push(typed_array_prototype_map_continuation(state));
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

#[expect(
    clippy::too_many_arguments,
    reason = "the native entry preserves the fixed source witness, callback arguments, and standard call context"
)]
fn begin_typed_array_prototype_filter(
    runtime: &mut Runtime,
    source: ObjectId,
    source_length: usize,
    source_element: TypedArrayElementType,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(callback) = arguments.take_first_or_undefined() else {
        return typed_array_type_error(realm, &origin, "not a function");
    };
    advance_typed_array_prototype_filter(
        runtime,
        TypedArrayPrototypeFilterState {
            source,
            source_length,
            source_element,
            callback,
            this_argument: arguments.take_first_or_undefined(),
            index: 0,
            element: None,
            kept: Vec::new(),
            target: None,
            write_index: 0,
            realm,
            stage: TypedArrayPrototypeFilterStage::NextElement,
            origin,
        },
        StoredValue::Undefined,
        return_to,
        execution_budget,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "the callback collection, species protocol, and conversion-resuming indexed writes are one ordered filter state machine"
)]
pub(super) fn advance_typed_array_prototype_filter(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeFilterState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TypedArrayPrototypeFilterStage::NextElement => {
            if state.index >= state.source_length {
                return begin_typed_array_filter_constructor_get(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                );
            }
            execution_budget.charge_instructions(1)?;
            let key = array_static_index_key(runtime, usize_to_u64(state.index))?;
            let source = StoredValue::Object(state.source);
            charge_heap_property_lookup(runtime, &source, execution_budget)?;
            state.stage = TypedArrayPrototypeFilterStage::AwaitElement;
            let dispatch = begin_value_get(
                runtime,
                &source,
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
                typed_array_prototype_filter_continuation,
                |state, value| {
                    advance_typed_array_prototype_filter(
                        runtime,
                        state,
                        value,
                        return_to,
                        execution_budget,
                    )
                },
                "TypedArray.prototype.filter source Get produced a structured result",
            )
        }
        TypedArrayPrototypeFilterStage::AwaitElement => {
            state.element = Some(value.duplicate());
            let mut callback_arguments = Vec::new();
            callback_arguments.try_reserve_exact(3).map_err(|_| {
                ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 3,
                }
            })?;
            callback_arguments.push(value);
            callback_arguments.push(typed_array_usize_number(state.index));
            callback_arguments.push(StoredValue::Object(state.source));
            state.stage = TypedArrayPrototypeFilterStage::AwaitCallback;
            let callback = state.callback;
            let receiver = state.this_argument.duplicate();
            suspend_typed_array_filter(state, callback, receiver, callback_arguments, return_to)
        }
        TypedArrayPrototypeFilterStage::AwaitCallback => {
            if runtime.to_boolean(&value)? {
                let element = state.element.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "TypedArray.prototype.filter lost a source element before its callback completed",
                })?;
                state
                    .kept
                    .try_reserve(1)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::Frames,
                        additional: 1,
                    })?;
                state.kept.push(element);
            } else {
                state.element = None;
            }
            state.index = state
                .index
                .checked_add(1)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "TypedArray.prototype.filter index overflowed within its captured length",
                })?;
            state.stage = TypedArrayPrototypeFilterStage::NextElement;
            advance_typed_array_prototype_filter(
                runtime,
                state,
                StoredValue::Undefined,
                return_to,
                execution_budget,
            )
        }
        TypedArrayPrototypeFilterStage::AwaitConstructor => {
            if let StoredValue::Function(function) = value
                && function_is_constructor(runtime, function)?
            {
                let function_realm = runtime.function_realm(function)?;
                if function_realm != state.realm
                    && function
                        == runtime
                            .realm_typed_array_constructor(function_realm, state.source_element)?
                {
                    let constructor =
                        runtime.realm_typed_array_constructor(state.realm, state.source_element)?;
                    return begin_typed_array_filter_construct(state, constructor, return_to);
                }
            }
            if matches!(value, StoredValue::Undefined) {
                let constructor =
                    runtime.realm_typed_array_constructor(state.realm, state.source_element)?;
                return begin_typed_array_filter_construct(state, constructor, return_to);
            }
            if !matches!(value, StoredValue::Object(_) | StoredValue::Function(_)) {
                return typed_array_type_error(state.realm, &state.origin, "not a constructor");
            }
            state.stage = TypedArrayPrototypeFilterStage::AwaitSpecies;
            begin_typed_array_filter_species_get(
                runtime,
                state,
                &value,
                return_to,
                execution_budget,
            )
        }
        TypedArrayPrototypeFilterStage::AwaitSpecies => {
            let constructor = if matches!(value, StoredValue::Undefined | StoredValue::Null) {
                runtime.realm_typed_array_constructor(state.realm, state.source_element)?
            } else if let StoredValue::Function(function) = value {
                if !function_is_constructor(runtime, function)? {
                    return typed_array_type_error(state.realm, &state.origin, "not a constructor");
                }
                function
            } else {
                return typed_array_type_error(state.realm, &state.origin, "not a constructor");
            };
            begin_typed_array_filter_construct(state, constructor, return_to)
        }
        TypedArrayPrototypeFilterStage::AwaitConstruct => {
            let StoredValue::Object(target) = value else {
                return typed_array_type_error(
                    state.realm,
                    &state.origin,
                    "TypedArray species constructor returned a non-TypedArray",
                );
            };
            let (target_state, target_length) = typed_array_require_writable_in_bounds(
                runtime,
                target,
                state.realm,
                &state.origin,
            )?;
            if target_length < state.kept.len() {
                return typed_array_type_error(
                    state.realm,
                    &state.origin,
                    "TypedArray species constructor returned a too-short TypedArray",
                );
            }
            if target_state.element().is_bigint() != state.source_element.is_bigint() {
                return typed_array_type_error(
                    state.realm,
                    &state.origin,
                    "TypedArray species constructor returned a different content type",
                );
            }
            state.target = Some(target);
            state.stage = TypedArrayPrototypeFilterStage::NextKeptValue;
            advance_typed_array_prototype_filter(
                runtime,
                state,
                StoredValue::Undefined,
                return_to,
                execution_budget,
            )
        }
        TypedArrayPrototypeFilterStage::NextKeptValue => {
            if state.write_index >= state.kept.len() {
                let target = state.target.ok_or(EngineFault::RuntimeInvariant {
                    message: "TypedArray.prototype.filter lost its constructed target",
                })?;
                return Ok(NativeDispatch::Immediate(StoredValue::Object(target)));
            }
            let target = state.target.ok_or(EngineFault::RuntimeInvariant {
                message: "TypedArray.prototype.filter lost its constructed target",
            })?;
            let value = state.kept[state.write_index].duplicate();
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_typed_array_element_set(
                runtime,
                target,
                TypedArrayPropertyKey::Index(state.write_index),
                value,
                TypedArraySetCompletion::Filter(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

pub(super) fn resume_typed_array_prototype_filter_after_store(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeFilterState,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.write_index = state
        .write_index
        .checked_add(1)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "TypedArray.prototype.filter write index overflowed within its collected values",
        })?;
    state.stage = TypedArrayPrototypeFilterStage::NextKeptValue;
    advance_typed_array_prototype_filter(
        runtime,
        state,
        StoredValue::Undefined,
        return_to,
        execution_budget,
    )
}

fn begin_typed_array_filter_constructor_get(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeFilterState,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = TypedArrayPrototypeFilterStage::AwaitConstructor;
    let source = StoredValue::Object(state.source);
    charge_heap_property_lookup(runtime, &source, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        &source,
        runtime.predefined_property_key(PredefinedAtom::Constructor),
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        typed_array_prototype_filter_continuation,
        |state, value| {
            advance_typed_array_prototype_filter(runtime, state, value, return_to, execution_budget)
        },
        "TypedArray.prototype.filter constructor Get produced a structured result",
    )
}

fn begin_typed_array_filter_species_get(
    runtime: &mut Runtime,
    state: TypedArrayPrototypeFilterState,
    constructor: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_heap_property_lookup(runtime, constructor, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        constructor,
        runtime.predefined_symbol_property_key(PredefinedAtom::SymbolSpecies),
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        typed_array_prototype_filter_continuation,
        |state, value| {
            advance_typed_array_prototype_filter(runtime, state, value, return_to, execution_budget)
        },
        "TypedArray.prototype.filter species Get produced a structured result",
    )
}

fn begin_typed_array_filter_construct(
    mut state: TypedArrayPrototypeFilterState,
    constructor: FunctionId,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = TypedArrayPrototypeFilterStage::AwaitConstruct;
    let origin = state.origin.clone();
    let count = state.kept.len();
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(typed_array_prototype_filter_continuation(state));
    Ok(NativeDispatch::Call(NativeCall {
        function: constructor,
        receiver: StoredValue::Undefined,
        arguments: CallArguments::from_values(vec![typed_array_usize_number(count)]),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: Some(constructor),
        native_caller: None,
    }))
}

fn typed_array_prototype_filter_continuation(
    state: TypedArrayPrototypeFilterState,
) -> NativeContinuation {
    NativeContinuation::TypedArrayPrototypeFilter(Box::new(state))
}

fn suspend_typed_array_filter(
    state: TypedArrayPrototypeFilterState,
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
    continuations.push(typed_array_prototype_filter_continuation(state));
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

fn typed_array_relative_bound(value: f64, length: usize) -> usize {
    if value < 0.0 {
        if value == f64::NEG_INFINITY {
            return 0;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the finite negative relative index is bounded against the usize source length"
        )]
        let magnitude = (-value) as u128;
        return usize::try_from(magnitude).map_or(0, |magnitude| length.saturating_sub(magnitude));
    }
    if value == f64::INFINITY {
        return length;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the finite positive relative index is bounded against the usize source length"
    )]
    let value = value as u128;
    usize::try_from(value).map_or(length, |value| value.min(length))
}

fn typed_array_at_index(relative: f64, length: usize) -> Option<usize> {
    let length_number = typed_array_usize_f64(length);
    if relative >= 0.0 {
        if relative >= length_number {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the finite non-negative index is strictly below the implementation-sized length"
        )]
        return Some(relative as usize);
    }
    if relative < -length_number {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the finite negative index has been bounded by the implementation-sized length"
    )]
    let magnitude = (-relative) as usize;
    Some(length.saturating_sub(magnitude))
}

fn typed_array_usize_number(value: usize) -> StoredValue {
    StoredValue::Number(JsNumber::from_f64(typed_array_usize_f64(value)))
}

fn typed_array_usize_f64(value: usize) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "TypedArray element lengths and byte offsets are bounded by ToIndex"
    )]
    let value = value as f64;
    value
}

#[expect(
    clippy::too_many_arguments,
    reason = "the native entry point retains both set arguments across offset conversion"
)]
fn begin_typed_array_prototype_set(
    runtime: &mut Runtime,
    target: ObjectId,
    source: StoredValue,
    offset: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_operator_primitive_conversion(
        runtime,
        offset,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TypedArrayPrototypeSetOffset(Box::new(
            TypedArrayPrototypeSetState {
                target,
                source,
                target_offset: 0,
                target_length: 0,
                source_length: 0,
                source_index: 0,
                realm,
                stage: TypedArrayPrototypeSetStage::AwaitSourceLength,
                origin: origin.clone(),
            },
        )),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_typed_array_prototype_set_offset(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeSetState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let offset =
        number_to_integer_or_infinity(operator_to_number(value, state.realm, &state.origin)?);
    if !offset.is_finite() || offset < 0.0 {
        return typed_array_range_error(state.realm, &state.origin, "invalid TypedArray offset");
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the finite, non-negative ToIntegerOrInfinity result is checked against usize below"
    )]
    let offset = offset as u128;
    let Ok(offset) = usize::try_from(offset) else {
        return typed_array_range_error(state.realm, &state.origin, "invalid TypedArray offset");
    };
    state.target_offset = offset;

    if let StoredValue::Object(source) = state.source
        && runtime.typed_array_state(source)?.is_some()
    {
        return typed_array_set_from_typed_array(runtime, &state, source);
    }
    let (_, target_length) =
        typed_array_require_in_bounds(runtime, state.target, state.realm, &state.origin)?;
    state.target_length = target_length;
    if matches!(state.source, StoredValue::Undefined | StoredValue::Null) {
        return typed_array_type_error(
            state.realm,
            &state.origin,
            "TypedArray set source is not an object",
        );
    }
    state.stage = TypedArrayPrototypeSetStage::AwaitSourceLength;
    typed_array_prototype_set_read(
        runtime,
        state,
        runtime.predefined_property_key(PredefinedAtom::Length),
        return_to,
        execution_budget,
    )
}

pub(super) fn advance_typed_array_prototype_set(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeSetState,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let value = completion.ok_or(EngineFault::RuntimeInvariant {
        message: "TypedArray.prototype.set resumed without a completion",
    })?;
    match state.stage {
        TypedArrayPrototypeSetStage::AwaitSourceLength => {
            state.stage = TypedArrayPrototypeSetStage::AwaitSourceLengthConversion;
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::TypedArrayPrototypeSetSourceLength(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TypedArrayPrototypeSetStage::AwaitSourceElement => {
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::TypedArrayPrototypeSetElement(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TypedArrayPrototypeSetStage::AwaitSourceLengthConversion => {
            Err(EngineFault::RuntimeInvariant {
                message: "TypedArray.prototype.set resumed at an invalid stage",
            }
            .into())
        }
    }
}

pub(super) fn finish_typed_array_prototype_set_source_length(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeSetState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let length = number_to_length(operator_to_number(value, state.realm, &state.origin)?);
    let Ok(length) = usize::try_from(length) else {
        return typed_array_range_error(
            state.realm,
            &state.origin,
            "TypedArray source length exceeds implementation range",
        );
    };
    let Some(end) = state.target_offset.checked_add(length) else {
        return typed_array_range_error(
            state.realm,
            &state.origin,
            "TypedArray source does not fit",
        );
    };
    if end > state.target_length {
        return typed_array_range_error(
            state.realm,
            &state.origin,
            "TypedArray source does not fit",
        );
    }
    state.source_length = length;
    state.source_index = 0;
    state.stage = TypedArrayPrototypeSetStage::AwaitSourceElement;
    typed_array_prototype_set_next_element(runtime, state, return_to, execution_budget)
}

pub(super) fn finish_typed_array_prototype_set_element(
    runtime: &mut Runtime,
    mut state: TypedArrayPrototypeSetState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let element = runtime
        .typed_array_state(state.target)?
        .copied()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "TypedArray.prototype.set target lost its internal slots",
        })?
        .element();
    let index = state.target_offset.checked_add(state.source_index).ok_or(
        EngineFault::RuntimeInvariant {
            message: "TypedArray.prototype.set target index overflowed after range validation",
        },
    )?;
    let stored = if element.is_bigint() {
        let value = to_bigint_from_primitive(&value, state.realm, &state.origin)?;
        runtime.typed_array_store_index(
            state.target,
            index,
            TypedArrayElementValue::BigInt(value.as_ref()),
        )?
    } else {
        let value = operator_to_number(value, state.realm, &state.origin)?;
        runtime.typed_array_store_index(
            state.target,
            index,
            TypedArrayElementValue::Number(value),
        )?
    };
    match stored {
        TypedArrayStoreOutcome::Stored | TypedArrayStoreOutcome::Missing => {}
        TypedArrayStoreOutcome::Immutable => {
            return typed_array_type_error(
                state.realm,
                &state.origin,
                "TypedArray backing buffer is immutable",
            );
        }
        TypedArrayStoreOutcome::ContentTypeMismatch => {
            return Err(EngineFault::RuntimeInvariant {
                message: "TypedArray.prototype.set target content type changed during conversion",
            }
            .into());
        }
    }
    state.source_index = state.source_index.saturating_add(1);
    typed_array_prototype_set_next_element(runtime, state, return_to, execution_budget)
}

#[allow(
    clippy::too_many_lines,
    reason = "the typed-source branch keeps content checks, overlap copy, and cross-element snapshotting in one non-observable operation"
)]
fn typed_array_set_from_typed_array(
    runtime: &mut Runtime,
    state: &TypedArrayPrototypeSetState,
    source: ObjectId,
) -> Result<NativeDispatch, NativeFailure> {
    let (target_state, target_length) =
        typed_array_require_in_bounds(runtime, state.target, state.realm, &state.origin)?;
    let (source_state, source_length) =
        typed_array_require_in_bounds(runtime, source, state.realm, &state.origin)?;
    if target_state.element().is_bigint() != source_state.element().is_bigint() {
        return typed_array_type_error(
            state.realm,
            &state.origin,
            "TypedArray source and target content types differ",
        );
    }
    let Some(end) = state.target_offset.checked_add(source_length) else {
        return typed_array_range_error(
            state.realm,
            &state.origin,
            "TypedArray source does not fit",
        );
    };
    if end > target_length {
        return typed_array_range_error(
            state.realm,
            &state.origin,
            "TypedArray source does not fit",
        );
    }
    if target_state.element() == source_state.element() {
        let TypedArrayView::InBounds {
            buffer: source_buffer,
            byte_offset: source_offset,
            ..
        } = runtime.typed_array_view(source)?
        else {
            return typed_array_type_error(
                state.realm,
                &state.origin,
                "TypedArray source is out of bounds",
            );
        };
        let TypedArrayView::InBounds {
            buffer: target_buffer,
            byte_offset: target_offset,
            ..
        } = runtime.typed_array_view(state.target)?
        else {
            return typed_array_type_error(
                state.realm,
                &state.origin,
                "TypedArray target is out of bounds",
            );
        };
        let element_width = source_state.element().byte_width();
        let byte_length =
            source_length
                .checked_mul(element_width)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "TypedArray source byte length overflowed after view validation",
                })?;
        let target_offset = target_offset
            .checked_add(state.target_offset.checked_mul(element_width).ok_or(
                EngineFault::RuntimeInvariant {
                    message: "TypedArray target byte offset overflowed after range validation",
                },
            )?)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "TypedArray target byte offset overflowed after range validation",
            })?;
        runtime.copy_array_buffer_bytes_to(
            source_buffer,
            source_offset,
            target_buffer,
            target_offset,
            byte_length,
        )?;
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    }
    let mut values = Vec::new();
    values
        .try_reserve_exact(source_length)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: source_length,
        })?;
    for index in 0..source_length {
        let value = runtime.typed_array_read_index(source, index)?.ok_or(
            EngineFault::RuntimeInvariant {
                message: "TypedArray source lost an in-bounds element while snapshotting",
            },
        )?;
        values.push(value);
    }
    for (index, value) in values.into_iter().enumerate() {
        let target_index =
            state
                .target_offset
                .checked_add(index)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "TypedArray target index overflowed after range validation",
                })?;
        let outcome = match value {
            StoredValue::Number(value) => runtime.typed_array_store_index(
                state.target,
                target_index,
                TypedArrayElementValue::Number(value),
            )?,
            StoredValue::BigInt(value) => runtime.typed_array_store_index(
                state.target,
                target_index,
                TypedArrayElementValue::BigInt(value.as_ref()),
            )?,
            _ => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "typed-array storage read returned a non-numeric value",
                }
                .into());
            }
        };
        if outcome != TypedArrayStoreOutcome::Stored {
            return typed_array_type_error(
                state.realm,
                &state.origin,
                "TypedArray target is out of bounds",
            );
        }
    }
    Ok(NativeDispatch::Immediate(StoredValue::Undefined))
}

fn typed_array_prototype_set_next_element(
    runtime: &mut Runtime,
    state: TypedArrayPrototypeSetState,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.source_index >= state.source_length {
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    }
    let index = u64::try_from(state.source_index).map_err(|_| EngineFault::RuntimeInvariant {
        message: "TypedArray.prototype.set source index does not fit u64",
    })?;
    let key = array_static_index_key(runtime, index)?;
    typed_array_prototype_set_read(runtime, state, key, return_to, execution_budget)
}

fn typed_array_prototype_set_read(
    runtime: &mut Runtime,
    state: TypedArrayPrototypeSetState,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_typed_array_set_property_lookup(runtime, state.realm, &state.source, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        &state.source,
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
        typed_array_prototype_set_continuation,
        |state, value| {
            advance_typed_array_prototype_set(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "TypedArray.prototype.set source Get produced a structured result",
    )
}

fn charge_typed_array_set_property_lookup(
    runtime: &Runtime,
    realm: RealmId,
    base: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    let prototype = match base {
        StoredValue::Boolean(_) => Some(runtime.realm_boolean_prototype(realm)?),
        StoredValue::Number(_) => Some(runtime.realm_number_prototype(realm)?),
        StoredValue::BigInt(_) => Some(runtime.realm_bigint_prototype(realm)?),
        StoredValue::String(_) => Some(runtime.realm_string_prototype(realm)?),
        StoredValue::Symbol(_) => Some(runtime.realm_symbol_prototype(realm)?),
        StoredValue::Function(_) | StoredValue::Object(_) => None,
        StoredValue::Undefined | StoredValue::Null => {
            return Err(EngineFault::RuntimeInvariant {
                message: "TypedArray.prototype.set property lookup received a nullish source",
            }
            .into());
        }
    };
    if let Some(prototype) = prototype {
        charge_heap_property_lookup(runtime, &StoredValue::Object(prototype), execution_budget)
    } else {
        charge_heap_property_lookup(runtime, base, execution_budget)
    }
}

fn typed_array_prototype_set_continuation(
    state: TypedArrayPrototypeSetState,
) -> NativeContinuation {
    NativeContinuation::TypedArrayPrototypeSet(Box::new(state))
}

fn typed_array_buffer_is_immutable(
    runtime: &Runtime,
    state: TypedArrayState,
) -> Result<bool, NativeFailure> {
    runtime
        .array_buffer_state(state.buffer())?
        .map(crate::object::ArrayBufferState::is_immutable)
        .ok_or_else(|| {
            EngineFault::RuntimeInvariant {
                message: "TypedArray backing buffer lost its internal slots",
            }
            .into()
        })
}

fn typed_array_require_writable_in_bounds(
    runtime: &Runtime,
    object: ObjectId,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<(TypedArrayState, usize), NativeFailure> {
    let (state, length) = typed_array_require_in_bounds(runtime, object, realm, origin)?;
    if typed_array_buffer_is_immutable(runtime, state)? {
        return typed_array_type_error(realm, origin, "TypedArray backing buffer is immutable");
    }
    Ok((state, length))
}

pub(super) fn typed_array_require_in_bounds(
    runtime: &Runtime,
    object: ObjectId,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<(TypedArrayState, usize), NativeFailure> {
    let Some(state) = runtime.typed_array_state(object)?.copied() else {
        return typed_array_type_error(realm, origin, "not a TypedArray");
    };
    let TypedArrayView::InBounds { length, .. } = runtime.typed_array_view(object)? else {
        return typed_array_type_error(realm, origin, "TypedArray is out of bounds");
    };
    Ok((state, length))
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

pub(super) fn typed_array_type_error<T>(
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

#[allow(
    clippy::needless_pass_by_value,
    reason = "the conversion target intentionally transfers an owned continuation state"
)]
pub(super) fn finish_typed_array_element_set(
    runtime: &mut Runtime,
    state: TypedArrayElementSetState,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
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
    match stored {
        TypedArrayStoreOutcome::Stored | TypedArrayStoreOutcome::Missing => {}
        TypedArrayStoreOutcome::Immutable => {
            return typed_array_type_error(
                state.realm,
                &state.origin,
                "TypedArray backing buffer is immutable",
            );
        }
        TypedArrayStoreOutcome::ContentTypeMismatch => {
            return Err(EngineFault::RuntimeInvariant {
                message: "typed-array element content type changed during conversion",
            }
            .into());
        }
    }
    let completion = match state.completion {
        TypedArraySetCompletion::LanguageWrite => StoredValue::Undefined,
        TypedArraySetCompletion::ReflectSet => StoredValue::Boolean(true),
        TypedArraySetCompletion::Define(DefinePropertyResult::Target) => {
            StoredValue::Object(state.object)
        }
        TypedArraySetCompletion::Define(DefinePropertyResult::Boolean) => {
            StoredValue::Boolean(true)
        }
        TypedArraySetCompletion::Map(state) => {
            return resume_typed_array_prototype_map_after_store(
                runtime,
                *state,
                return_to,
                execution_budget,
            );
        }
        TypedArraySetCompletion::Filter(state) => {
            return resume_typed_array_prototype_filter_after_store(
                runtime,
                *state,
                return_to,
                execution_budget,
            );
        }
    };
    Ok(NativeDispatch::Immediate(completion))
}
