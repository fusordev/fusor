/*
 * JavaScript iterator abstract operations derived from QuickJS.
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

//! Resumable synchronous iterator operations and intrinsic iterator methods.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

fn iterator_exception(
    realm: RealmId,
    origin: JsStackFrame,
    kind: ExceptionKind,
    message: &str,
) -> Result<NativeFailure, NativeFailure> {
    Ok(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}

#[derive(Clone, Copy)]
enum IteratorFromStage {
    IteratorMethod,
    Iterator,
    NextMethod,
    PrototypeWalk,
}

pub(super) struct IteratorFromContinuation {
    input: StoredValue,
    iterator: Option<StoredValue>,
    next_method: Option<StoredValue>,
    current: Option<HeapReference>,
    realm: RealmId,
    stage: IteratorFromStage,
    origin: JsStackFrame,
}

impl IteratorFromContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64
            .saturating_add(u64::from(self.iterator.is_some()))
            .saturating_add(u64::from(self.next_method.is_some()))
            .saturating_add(u64::from(self.current.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.input, mark);
        if let Some(iterator) = &self.iterator {
            trace_stored_value_root(iterator, mark);
        }
        if let Some(next_method) = &self.next_method {
            trace_stored_value_root(next_method, mark);
        }
        if let Some(current) = self.current {
            mark(CollectionRoot::Heap(current));
        }
    }
}

pub(super) struct IteratorConcatCreationContinuation {
    items: Vec<StoredValue>,
    iterables: Vec<crate::object::IteratorConcatIterable>,
    index: usize,
    realm: RealmId,
    origin: JsStackFrame,
}

impl IteratorConcatCreationContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        usize_to_u64(self.items.len())
            .saturating_add(usize_to_u64(self.iterables.len()).saturating_mul(2))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        for item in &self.items {
            trace_stored_value_root(item, mark);
        }
        for iterable in &self.iterables {
            trace_stored_value_root(iterable.iterable(), mark);
            mark(CollectionRoot::Heap(HeapReference::Function(
                iterable.open_method(),
            )));
        }
    }
}

#[derive(Clone, Copy)]
enum IteratorZipCreationStage {
    OptionsMode,
    OptionsPadding,
    OuterMethod,
    OuterIterator,
    OuterNextMethod,
    OuterNextResult,
    OuterDone,
    OuterValue,
    InnerMethod,
    InnerIterator,
    InnerNextMethod,
    PaddingMethod,
    PaddingIterator,
    PaddingNextMethod,
    PaddingNextResult,
    PaddingDone,
    PaddingValue,
    KeyedOwnKeys,
    KeyedDescriptor,
    KeyedValue,
    KeyedPaddingValue,
}

pub(super) struct IteratorZipCreationContinuation {
    iterables: StoredValue,
    options: StoredValue,
    mode: crate::object::IteratorZipMode,
    padding_input: Option<StoredValue>,
    outer_iterator: Option<StoredValue>,
    outer_next: Option<FunctionId>,
    outer_result: Option<StoredValue>,
    inner_input: Option<StoredValue>,
    inner_iterator: Option<StoredValue>,
    records: Vec<crate::object::IteratorZipRecord>,
    padding_iterator: Option<StoredValue>,
    padding_next: Option<FunctionId>,
    padding_result: Option<StoredValue>,
    padding: Vec<StoredValue>,
    keyed: bool,
    keyed_target: Option<HeapReference>,
    keyed_keys: Vec<PropertyKey>,
    keyed_index: usize,
    current_key: Option<PropertyKey>,
    selected_keys: Vec<PropertyKey>,
    realm: RealmId,
    stage: IteratorZipCreationStage,
    origin: JsStackFrame,
}

impl IteratorZipCreationContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.padding_input.is_some()))
            .saturating_add(u64::from(self.outer_iterator.is_some()))
            .saturating_add(u64::from(self.outer_next.is_some()))
            .saturating_add(u64::from(self.outer_result.is_some()))
            .saturating_add(u64::from(self.inner_input.is_some()))
            .saturating_add(u64::from(self.inner_iterator.is_some()))
            .saturating_add(usize_to_u64(self.records.len()).saturating_mul(2))
            .saturating_add(u64::from(self.padding_iterator.is_some()))
            .saturating_add(u64::from(self.padding_next.is_some()))
            .saturating_add(u64::from(self.padding_result.is_some()))
            .saturating_add(usize_to_u64(self.padding.len()))
            .saturating_add(usize_to_u64(self.keyed_keys.len()))
            .saturating_add(u64::from(self.current_key.is_some()))
            .saturating_add(usize_to_u64(self.selected_keys.len()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.iterables, mark);
        trace_stored_value_root(&self.options, mark);
        for value in [
            self.padding_input.as_ref(),
            self.outer_iterator.as_ref(),
            self.outer_result.as_ref(),
            self.inner_input.as_ref(),
            self.inner_iterator.as_ref(),
            self.padding_iterator.as_ref(),
            self.padding_result.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            trace_stored_value_root(value, mark);
        }
        for function in [self.outer_next, self.padding_next].into_iter().flatten() {
            mark(CollectionRoot::Heap(HeapReference::Function(function)));
        }
        for record in &self.records {
            trace_stored_value_root(record.iterator(), mark);
            trace_stored_value_root(record.next_method(), mark);
        }
        for value in &self.padding {
            trace_stored_value_root(value, mark);
        }
        if let Some(target) = self.keyed_target {
            mark(CollectionRoot::Heap(target));
        }
    }
}

#[derive(Clone, Copy)]
enum IteratorZipNextStage {
    NextResult,
    Done,
    Value,
    StrictNextResult,
    StrictDone,
}

pub(super) struct IteratorZipNextContinuation {
    helper: ObjectId,
    mode: crate::object::IteratorZipMode,
    record_count: usize,
    index: usize,
    values: Vec<StoredValue>,
    keys: Option<Vec<PropertyKey>>,
    result: Option<StoredValue>,
    realm: RealmId,
    stage: IteratorZipNextStage,
    origin: JsStackFrame,
}

impl IteratorZipNextContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        usize_to_u64(self.values.len()).saturating_add(u64::from(self.result.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.helper)));
        for value in &self.values {
            trace_stored_value_root(value, mark);
        }
        if let Some(result) = &self.result {
            trace_stored_value_root(result, mark);
        }
    }
}

pub(super) enum IteratorZipCloseTerminal {
    Create {
        records: Vec<crate::object::IteratorZipRecord>,
        padding: Vec<StoredValue>,
        mode: crate::object::IteratorZipMode,
        keys: Option<Vec<PropertyKey>>,
    },
    ReturnDone,
    Throw,
}

#[derive(Clone, Copy)]
enum IteratorZipCloseStage {
    ReturnProperty,
    ReturnCall,
}

pub(super) struct IteratorZipCloseContinuation {
    helper: Option<ObjectId>,
    iterators: Vec<StoredValue>,
    current: Option<StoredValue>,
    pending: Option<PendingException>,
    terminal: IteratorZipCloseTerminal,
    realm: RealmId,
    stage: IteratorZipCloseStage,
    origin: JsStackFrame,
}

impl IteratorZipCloseContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        let terminal = match &self.terminal {
            IteratorZipCloseTerminal::Create {
                records, padding, ..
            } => usize_to_u64(records.len())
                .saturating_mul(2)
                .saturating_add(usize_to_u64(padding.len())),
            IteratorZipCloseTerminal::ReturnDone | IteratorZipCloseTerminal::Throw => 0,
        };
        usize_to_u64(self.iterators.len())
            .saturating_add(u64::from(self.current.is_some()))
            .saturating_add(u64::from(self.pending.is_some()))
            .saturating_add(terminal)
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        if let Some(helper) = self.helper {
            mark(CollectionRoot::Heap(HeapReference::Object(helper)));
        }
        for value in &self.iterators {
            trace_stored_value_root(value, mark);
        }
        if let Some(current) = &self.current {
            trace_stored_value_root(current, mark);
        }
        if let Some(PendingException {
            payload: PendingExceptionPayload::ThrownValue(value),
            ..
        }) = &self.pending
        {
            trace_stored_value_root(value, mark);
        }
        if let IteratorZipCloseTerminal::Create {
            records, padding, ..
        } = &self.terminal
        {
            for record in records {
                trace_stored_value_root(record.iterator(), mark);
                trace_stored_value_root(record.next_method(), mark);
            }
            for value in padding {
                trace_stored_value_root(value, mark);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum IteratorToArrayStage {
    NextMethod,
    NextResult,
    Done,
    Value,
}

pub(super) struct IteratorToArrayContinuation {
    iterator: StoredValue,
    next_method: Option<FunctionId>,
    result: Option<StoredValue>,
    items: Vec<StoredValue>,
    realm: RealmId,
    stage: IteratorToArrayStage,
    origin: JsStackFrame,
}

impl IteratorToArrayContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64
            .saturating_add(u64::from(self.next_method.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
            .saturating_add(usize_to_u64(self.items.len()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.iterator, mark);
        if let Some(next_method) = self.next_method {
            mark(CollectionRoot::Heap(HeapReference::Function(next_method)));
        }
        if let Some(result) = &self.result {
            trace_stored_value_root(result, mark);
        }
        for item in &self.items {
            trace_stored_value_root(item, mark);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IteratorIncludesStage {
    NextMethod,
    NextResult,
    Done,
    Value,
    CloseReturnProperty,
    CloseReturnCall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IteratorIncludesSkip {
    Finite(u64),
    Infinite,
}

impl IteratorIncludesSkip {
    fn consume_one(&mut self) -> bool {
        match self {
            Self::Finite(0) => false,
            Self::Finite(remaining) => {
                *remaining = remaining.saturating_sub(1);
                true
            }
            Self::Infinite => true,
        }
    }
}

pub(super) struct IteratorIncludesContinuation {
    iterator: StoredValue,
    next_method: Option<FunctionId>,
    result: Option<StoredValue>,
    search_element: StoredValue,
    to_skip: IteratorIncludesSkip,
    realm: RealmId,
    stage: IteratorIncludesStage,
    origin: JsStackFrame,
}

impl IteratorIncludesContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.next_method.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.iterator, mark);
        trace_stored_value_root(&self.search_element, mark);
        if let Some(next_method) = self.next_method {
            mark(CollectionRoot::Heap(HeapReference::Function(next_method)));
        }
        if let Some(result) = &self.result {
            trace_stored_value_root(result, mark);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IteratorConsumerStage {
    NextMethod,
    NextResult,
    Done,
    Value,
    Callback,
    CloseReturnProperty,
    CloseReturnCall,
}

pub(super) struct IteratorConsumerContinuation {
    iterator: StoredValue,
    next_method: Option<FunctionId>,
    callback: FunctionId,
    kind: crate::runtime::IteratorConsumer,
    counter: u64,
    result: Option<StoredValue>,
    candidate: Option<StoredValue>,
    accumulator: Option<StoredValue>,
    outcome: Option<StoredValue>,
    realm: RealmId,
    stage: IteratorConsumerStage,
    origin: JsStackFrame,
}

impl IteratorConsumerContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.next_method.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
            .saturating_add(u64::from(self.candidate.is_some()))
            .saturating_add(u64::from(self.accumulator.is_some()))
            .saturating_add(u64::from(self.outcome.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.iterator, mark);
        mark(CollectionRoot::Heap(HeapReference::Function(self.callback)));
        if let Some(next_method) = self.next_method {
            mark(CollectionRoot::Heap(HeapReference::Function(next_method)));
        }
        if let Some(result) = &self.result {
            trace_stored_value_root(result, mark);
        }
        if let Some(candidate) = &self.candidate {
            trace_stored_value_root(candidate, mark);
        }
        if let Some(accumulator) = &self.accumulator {
            trace_stored_value_root(accumulator, mark);
        }
        if let Some(outcome) = &self.outcome {
            trace_stored_value_root(outcome, mark);
        }
    }

    pub(super) const fn handles_abrupt(&self) -> bool {
        matches!(self.stage, IteratorConsumerStage::Callback)
    }
}

#[derive(Clone, Copy)]
enum IteratorDisposeStage {
    ReturnProperty,
    ReturnCall,
}

pub(super) struct IteratorDisposeContinuation {
    iterator: StoredValue,
    realm: RealmId,
    stage: IteratorDisposeStage,
    origin: JsStackFrame,
}

impl IteratorDisposeContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.iterator, mark);
    }
}

pub(super) struct IteratorHelperCreationContinuation {
    iterator: StoredValue,
    kind: crate::object::IteratorHelperKind,
    callback: Option<FunctionId>,
    remaining: f64,
    realm: RealmId,
    origin: JsStackFrame,
}

impl IteratorHelperCreationContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(u64::from(self.callback.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.iterator, mark);
        if let Some(callback) = self.callback {
            mark(CollectionRoot::Heap(HeapReference::Function(callback)));
        }
    }
}

pub(super) struct IteratorLimitContinuation {
    iterator: StoredValue,
    kind: crate::object::IteratorHelperKind,
    realm: RealmId,
    origin: JsStackFrame,
}

impl IteratorLimitContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.iterator, mark);
    }
}

#[derive(Clone, Copy)]
enum IteratorHelperNextStage {
    ConcatIteratorCall,
    ConcatNextMethod,
    ConcatNextResult,
    ConcatDone,
    ConcatValue,
    NextResult,
    Done,
    Value,
    Callback,
    InnerIteratorMethod,
    InnerIteratorCall,
    InnerNextMethod,
    InnerNextResult,
    InnerDone,
    InnerValue,
}

pub(super) struct IteratorHelperNextContinuation {
    helper: ObjectId,
    iterator: StoredValue,
    next_method: StoredValue,
    kind: crate::object::IteratorHelperKind,
    callback: Option<FunctionId>,
    counter: u64,
    remaining: f64,
    dropping: bool,
    result: Option<StoredValue>,
    candidate: Option<StoredValue>,
    inner_iterator: Option<StoredValue>,
    inner_next_method: Option<StoredValue>,
    concat_iterable: Option<crate::object::IteratorConcatIterable>,
    realm: RealmId,
    stage: IteratorHelperNextStage,
    origin: JsStackFrame,
}

impl IteratorHelperNextContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        3_u64
            .saturating_add(u64::from(self.callback.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
            .saturating_add(u64::from(self.candidate.is_some()))
            .saturating_add(u64::from(self.inner_iterator.is_some()))
            .saturating_add(u64::from(self.inner_next_method.is_some()))
            .saturating_add(u64::from(self.concat_iterable.is_some()).saturating_mul(2))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.helper)));
        trace_stored_value_root(&self.iterator, mark);
        trace_stored_value_root(&self.next_method, mark);
        if let Some(callback) = self.callback {
            mark(CollectionRoot::Heap(HeapReference::Function(callback)));
        }
        if let Some(result) = &self.result {
            trace_stored_value_root(result, mark);
        }
        if let Some(candidate) = &self.candidate {
            trace_stored_value_root(candidate, mark);
        }
        if let Some(inner_iterator) = &self.inner_iterator {
            trace_stored_value_root(inner_iterator, mark);
        }
        if let Some(inner_next_method) = &self.inner_next_method {
            trace_stored_value_root(inner_next_method, mark);
        }
        if let Some(iterable) = &self.concat_iterable {
            trace_stored_value_root(iterable.iterable(), mark);
            mark(CollectionRoot::Heap(HeapReference::Function(
                iterable.open_method(),
            )));
        }
    }
}

#[derive(Clone, Copy)]
enum IteratorHelperReturnStage {
    ReturnProperty,
    ReturnCall,
}

pub(super) struct IteratorHelperReturnContinuation {
    helper: ObjectId,
    iterator: StoredValue,
    outer_iterator: Option<StoredValue>,
    realm: RealmId,
    stage: IteratorHelperReturnStage,
    origin: JsStackFrame,
}

impl IteratorHelperReturnContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64.saturating_add(u64::from(self.outer_iterator.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.helper)));
        trace_stored_value_root(&self.iterator, mark);
        if let Some(outer_iterator) = &self.outer_iterator {
            trace_stored_value_root(outer_iterator, mark);
        }
    }
}

pub(super) struct IteratorWrapperReturnContinuation {
    iterator: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

impl IteratorWrapperReturnContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.iterator, mark);
    }
}

#[derive(Clone, Copy)]
enum IteratorPrototypeSetterStage {
    OwnDescriptor,
    Complete,
}

pub(super) struct IteratorPrototypeSetterContinuation {
    receiver: StoredValue,
    value: StoredValue,
    key: PropertyKey,
    name: JsString,
    reference: HeapReference,
    realm: RealmId,
    stage: IteratorPrototypeSetterStage,
    origin: JsStackFrame,
}

impl IteratorPrototypeSetterContinuation {
    pub(super) const fn retained_values() -> u64 {
        3
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.receiver, mark);
        trace_stored_value_root(&self.value, mark);
        mark(CollectionRoot::Heap(self.reference));
    }
}

pub(super) fn begin_iterator_constructor(
    runtime: &mut Runtime,
    function: FunctionId,
    new_target: Option<FunctionId>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = new_target else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator must be subclassed",
        )?);
    };
    if new_target == function {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator cannot be constructed directly",
        )?);
    }
    let key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    begin_intrinsic_get(
        runtime,
        realm,
        HeapReference::Function(new_target),
        StoredValue::Function(new_target),
        &key,
        IntrinsicGetContinuation::IteratorConstructor { new_target },
        return_to,
        Some(origin),
        execution_budget,
    )
}

pub(super) fn finish_iterator_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    prototype: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match prototype {
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
            HeapReference::Object(runtime.realm_iterator_prototype(realm)?)
        }
    };
    let object = runtime.allocate_ordinary_object_with_prototype(prototype)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(super) fn begin_iterator_from(
    runtime: &mut Runtime,
    input: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if !matches!(
        input,
        StoredValue::String(_) | StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator.from requires an object or string",
        )?);
    }
    let state = IteratorFromContinuation {
        input,
        iterator: None,
        next_method: None,
        current: None,
        realm,
        stage: IteratorFromStage::IteratorMethod,
        origin,
    };
    read_iterator_from_property(
        runtime,
        state,
        runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        "Symbol.iterator",
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "GetIteratorFlattenable and OrdinaryHasInstance share one resumable, proxy-aware state machine"
)]
pub(super) fn advance_iterator_from(
    runtime: &mut Runtime,
    mut state: IteratorFromContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IteratorFromStage::IteratorMethod => {
            if matches!(completion, StoredValue::Undefined | StoredValue::Null) {
                if !matches!(
                    state.input,
                    StoredValue::Function(_) | StoredValue::Object(_)
                ) {
                    return Err(iterator_exception(
                        state.realm,
                        state.origin,
                        ExceptionKind::TypeError,
                        "value is not iterator-like",
                    )?);
                }
                state.iterator = Some(state.input.duplicate());
                state.stage = IteratorFromStage::NextMethod;
                return read_iterator_from_property(
                    runtime,
                    state,
                    runtime.predefined_property_key(PredefinedAtom::Next),
                    "next",
                    return_to,
                    execution_budget,
                );
            }
            let StoredValue::Function(method) = completion else {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "Symbol.iterator is not callable",
                )?);
            };
            let receiver = state.input.duplicate();
            state.stage = IteratorFromStage::Iterator;
            let origin = state.origin.clone();
            iterator_method_call(
                method,
                receiver,
                NativeContinuation::IteratorFrom(state),
                return_to,
                origin,
            )
        }
        IteratorFromStage::Iterator => {
            if !matches!(
                completion,
                StoredValue::Function(_) | StoredValue::Object(_)
            ) {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "iterator method did not return an object",
                )?);
            }
            state.iterator = Some(completion);
            state.stage = IteratorFromStage::NextMethod;
            read_iterator_from_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Next),
                "next",
                return_to,
                execution_budget,
            )
        }
        IteratorFromStage::NextMethod => {
            let iterator = state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Iterator.from next lookup completed without an iterator",
                })?;
            let reference = iterator
                .heap_reference()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Iterator.from retained a non-object iterator",
                })?;
            state.next_method = Some(completion);
            state.current = Some(reference);
            state.stage = IteratorFromStage::PrototypeWalk;
            execution_budget.charge_instructions(1)?;
            let dispatch = begin_internal_get_prototype_of(
                runtime,
                reference,
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            )?;
            continue_get_after(
                dispatch,
                state,
                NativeContinuation::IteratorFrom,
                |state, value| {
                    advance_iterator_from(runtime, state, value, return_to, execution_budget)
                },
                "Iterator.from [[GetPrototypeOf]] produced a structured result",
            )
        }
        IteratorFromStage::PrototypeWalk => {
            let iterator_prototype = runtime.realm_iterator_prototype(state.realm)?;
            if completion.heap_reference() == Some(HeapReference::Object(iterator_prototype)) {
                return Ok(NativeDispatch::Immediate(state.iterator.ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "Iterator.from prototype walk lost its iterator",
                    },
                )?));
            }
            let Some(reference) = completion.heap_reference() else {
                if !matches!(completion, StoredValue::Null) {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "Iterator.from [[GetPrototypeOf]] returned neither object nor null",
                    }
                    .into());
                }
                let iterator = state.iterator.ok_or(EngineFault::RuntimeInvariant {
                    message: "Iterator.from wrapper allocation lost its iterator",
                })?;
                let next_method = state.next_method.ok_or(EngineFault::RuntimeInvariant {
                    message: "Iterator.from wrapper allocation lost its next method",
                })?;
                let wrapper =
                    runtime.allocate_iterator_wrapper(state.realm, iterator, next_method)?;
                return Ok(NativeDispatch::Immediate(StoredValue::Object(wrapper)));
            };
            state.current = Some(reference);
            execution_budget.charge_instructions(1)?;
            let dispatch = begin_internal_get_prototype_of(
                runtime,
                reference,
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            )?;
            continue_get_after(
                dispatch,
                state,
                NativeContinuation::IteratorFrom,
                |state, value| {
                    advance_iterator_from(runtime, state, value, return_to, execution_budget)
                },
                "Iterator.from [[GetPrototypeOf]] produced a structured result",
            )
        }
    }
}

pub(super) fn begin_iterator_concat(
    runtime: &mut Runtime,
    arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let items = arguments.into_remaining_values();
    let mut iterables = Vec::new();
    iterables
        .try_reserve_exact(items.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: items.len(),
        })?;
    continue_iterator_concat_creation(
        runtime,
        IteratorConcatCreationContinuation {
            items,
            iterables,
            index: 0,
            realm,
            origin,
        },
        return_to,
        execution_budget,
    )
}

pub(super) fn advance_iterator_concat_creation(
    runtime: &mut Runtime,
    mut state: IteratorConcatCreationContinuation,
    completion: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Function(open_method) = completion else {
        return Err(iterator_exception(
            state.realm,
            state.origin,
            ExceptionKind::TypeError,
            "Iterator.concat argument has no callable Symbol.iterator method",
        )?);
    };
    let item = state
        .items
        .get_mut(state.index)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Iterator.concat method lookup completed without an argument",
        })?;
    state
        .iterables
        .push(crate::object::IteratorConcatIterable::new(
            std::mem::replace(item, StoredValue::Undefined),
            *open_method,
        ));
    state.index = state.index.saturating_add(1);
    continue_iterator_concat_creation(runtime, state, return_to, execution_budget)
}

fn continue_iterator_concat_creation(
    runtime: &mut Runtime,
    state: IteratorConcatCreationContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(item) = state.items.get(state.index) else {
        let helper = runtime.allocate_iterator_concat_helper(state.realm, state.iterables)?;
        return Ok(NativeDispatch::Immediate(StoredValue::Object(helper)));
    };
    if item.heap_reference().is_none() {
        return Err(iterator_exception(
            state.realm,
            state.origin,
            ExceptionKind::TypeError,
            "Iterator.concat arguments must be objects",
        )?);
    }
    charge_iterator_property_lookup(runtime, item, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        item,
        runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::IteratorConcatCreation,
        |state, value| {
            advance_iterator_concat_creation(runtime, state, &value, return_to, execution_budget)
        },
        "Iterator.concat Symbol.iterator Get produced a structured result",
    )
}

fn iterator_zip_type_error(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin,
    })
}

pub(super) fn begin_iterator_zip(
    runtime: &mut Runtime,
    arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_iterator_zip_common(
        runtime,
        arguments,
        false,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn begin_iterator_zip_keyed(
    runtime: &mut Runtime,
    arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_iterator_zip_common(
        runtime,
        arguments,
        true,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_iterator_zip_common(
    runtime: &mut Runtime,
    mut arguments: CallArguments,
    keyed: bool,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let iterables = arguments.take_first_or_undefined();
    let options = arguments.take_first_or_undefined();
    let keyed_target = iterables.heap_reference();
    if iterables.heap_reference().is_none() {
        return Err(NativeFailure::Abrupt(iterator_zip_type_error(
            realm,
            origin,
            "Iterator.zip iterables must be an object",
        )?));
    }
    if !matches!(options, StoredValue::Undefined) && options.heap_reference().is_none() {
        return Err(NativeFailure::Abrupt(iterator_zip_type_error(
            realm,
            origin,
            "Iterator.zip options must be an object or undefined",
        )?));
    }
    let creation_stage = if matches!(options, StoredValue::Undefined) {
        IteratorZipCreationStage::OuterMethod
    } else {
        IteratorZipCreationStage::OptionsMode
    };
    let state = IteratorZipCreationContinuation {
        iterables,
        options,
        mode: crate::object::IteratorZipMode::Shortest,
        padding_input: None,
        outer_iterator: None,
        outer_next: None,
        outer_result: None,
        inner_input: None,
        inner_iterator: None,
        records: Vec::new(),
        padding_iterator: None,
        padding_next: None,
        padding_result: None,
        padding: Vec::new(),
        keyed,
        keyed_target,
        keyed_keys: Vec::new(),
        keyed_index: 0,
        current_key: None,
        selected_keys: Vec::new(),
        realm,
        stage: creation_stage,
        origin,
    };
    if matches!(creation_stage, IteratorZipCreationStage::OptionsMode) {
        let key = runtime
            .property_key_from_string(&JsString::from_utf8("mode")?)
            .map_err(ExecutionError::from)?;
        read_iterator_zip_creation_property(
            runtime,
            state.options.duplicate(),
            state,
            key,
            return_to,
            execution_budget,
        )
    } else {
        begin_iterator_zip_inputs(runtime, state, return_to, execution_budget)
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "Iterator.zip creation keeps every eager GetIterator and IteratorStepValue boundary explicit"
)]
pub(super) fn advance_iterator_zip_creation(
    runtime: &mut Runtime,
    mut state: IteratorZipCreationContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IteratorZipCreationStage::OptionsMode => {
            state.mode = match completion {
                StoredValue::Undefined => crate::object::IteratorZipMode::Shortest,
                StoredValue::String(value) => match value.to_utf8_lossy()?.as_str() {
                    "shortest" => crate::object::IteratorZipMode::Shortest,
                    "longest" => crate::object::IteratorZipMode::Longest,
                    "strict" => crate::object::IteratorZipMode::Strict,
                    _ => {
                        return Err(NativeFailure::Abrupt(iterator_zip_type_error(
                            state.realm,
                            state.origin,
                            "Iterator.zip mode must be 'shortest', 'longest', or 'strict'",
                        )?));
                    }
                },
                _ => {
                    return Err(NativeFailure::Abrupt(iterator_zip_type_error(
                        state.realm,
                        state.origin,
                        "Iterator.zip mode must be a string",
                    )?));
                }
            };
            if matches!(state.mode, crate::object::IteratorZipMode::Longest) {
                state.stage = IteratorZipCreationStage::OptionsPadding;
                let key = runtime
                    .property_key_from_string(&JsString::from_utf8("padding")?)
                    .map_err(ExecutionError::from)?;
                let base = state.options.duplicate();
                read_iterator_zip_creation_property(
                    runtime,
                    base,
                    state,
                    key,
                    return_to,
                    execution_budget,
                )
            } else {
                begin_iterator_zip_inputs(runtime, state, return_to, execution_budget)
            }
        }
        IteratorZipCreationStage::OptionsPadding => {
            if !matches!(completion, StoredValue::Undefined)
                && completion.heap_reference().is_none()
            {
                return Err(NativeFailure::Abrupt(iterator_zip_type_error(
                    state.realm,
                    state.origin,
                    "Iterator.zip padding must be an object or undefined",
                )?));
            }
            state.padding_input =
                (!matches!(completion, StoredValue::Undefined)).then_some(completion);
            begin_iterator_zip_inputs(runtime, state, return_to, execution_budget)
        }
        IteratorZipCreationStage::OuterMethod => {
            let StoredValue::Function(method) = completion else {
                return Err(NativeFailure::Abrupt(iterator_zip_type_error(
                    state.realm,
                    state.origin,
                    "Iterator.zip input is not iterable",
                )?));
            };
            execution_budget.charge_instructions(1)?;
            state.stage = IteratorZipCreationStage::OuterIterator;
            let receiver = state.iterables.duplicate();
            let origin = state.origin.clone();
            iterator_method_call(
                method,
                receiver,
                NativeContinuation::IteratorZipCreation(Box::new(state)),
                return_to,
                origin,
            )
        }
        IteratorZipCreationStage::OuterIterator => {
            if completion.heap_reference().is_none() {
                return Err(NativeFailure::Abrupt(iterator_zip_type_error(
                    state.realm,
                    state.origin,
                    "Iterator.zip input iterator must be an object",
                )?));
            }
            state.outer_iterator = Some(completion);
            state.stage = IteratorZipCreationStage::OuterNextMethod;
            let base = state
                .outer_iterator
                .as_ref()
                .expect("set above")
                .duplicate();
            read_iterator_zip_creation_property(
                runtime,
                base,
                state,
                runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        IteratorZipCreationStage::OuterNextMethod => {
            let StoredValue::Function(next) = completion else {
                return Err(NativeFailure::Abrupt(iterator_zip_type_error(
                    state.realm,
                    state.origin,
                    "Iterator.zip input next method is not callable",
                )?));
            };
            state.outer_next = Some(next);
            call_iterator_zip_outer_next(state, return_to, execution_budget)
        }
        IteratorZipCreationStage::OuterNextResult => {
            if completion.heap_reference().is_none() {
                let pending = iterator_zip_type_error(
                    state.realm,
                    state.origin.clone(),
                    "Iterator.zip input iterator did not return an object",
                )?;
                return fail_iterator_zip_creation(
                    runtime,
                    state,
                    pending,
                    false,
                    return_to,
                    execution_budget,
                );
            }
            state.outer_result = Some(completion);
            state.stage = IteratorZipCreationStage::OuterDone;
            let base = state.outer_result.as_ref().expect("set above").duplicate();
            read_iterator_zip_creation_property(
                runtime,
                base,
                state,
                runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        IteratorZipCreationStage::OuterDone => {
            if runtime.to_boolean(&completion)? {
                state.outer_iterator = None;
                state.outer_next = None;
                state.outer_result = None;
                return begin_iterator_zip_padding(runtime, state, return_to, execution_budget);
            }
            state.stage = IteratorZipCreationStage::OuterValue;
            let base = state
                .outer_result
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Iterator.zip outer done lookup lost its iterator result",
                })?
                .duplicate();
            read_iterator_zip_creation_property(
                runtime,
                base,
                state,
                runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        IteratorZipCreationStage::OuterValue => {
            state.outer_result = None;
            if completion.heap_reference().is_none() {
                let pending = iterator_zip_type_error(
                    state.realm,
                    state.origin.clone(),
                    "Iterator.zip inner iterables must be objects",
                )?;
                return fail_iterator_zip_creation(
                    runtime,
                    state,
                    pending,
                    true,
                    return_to,
                    execution_budget,
                );
            }
            state.inner_input = Some(completion);
            state.stage = IteratorZipCreationStage::InnerMethod;
            let base = state.inner_input.as_ref().expect("set above").duplicate();
            read_iterator_zip_creation_property(
                runtime,
                base,
                state,
                runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
                return_to,
                execution_budget,
            )
        }
        IteratorZipCreationStage::InnerMethod => match completion {
            StoredValue::Undefined | StoredValue::Null => {
                state.inner_iterator = state.inner_input.take();
                state.stage = IteratorZipCreationStage::InnerNextMethod;
                let base = state
                    .inner_iterator
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Iterator.zip direct iterator disappeared",
                    })?
                    .duplicate();
                read_iterator_zip_creation_property(
                    runtime,
                    base,
                    state,
                    runtime.predefined_property_key(PredefinedAtom::Next),
                    return_to,
                    execution_budget,
                )
            }
            StoredValue::Function(method) => {
                execution_budget.charge_instructions(1)?;
                let receiver = state
                    .inner_input
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Iterator.zip inner iterable disappeared",
                    })?
                    .duplicate();
                state.stage = IteratorZipCreationStage::InnerIterator;
                let origin = state.origin.clone();
                iterator_method_call(
                    method,
                    receiver,
                    NativeContinuation::IteratorZipCreation(Box::new(state)),
                    return_to,
                    origin,
                )
            }
            _ => {
                let pending = iterator_zip_type_error(
                    state.realm,
                    state.origin.clone(),
                    "Iterator.zip inner Symbol.iterator method is not callable",
                )?;
                let include_outer = !state.keyed;
                fail_iterator_zip_creation(
                    runtime,
                    state,
                    pending,
                    include_outer,
                    return_to,
                    execution_budget,
                )
            }
        },
        IteratorZipCreationStage::InnerIterator => {
            if completion.heap_reference().is_none() {
                let pending = iterator_zip_type_error(
                    state.realm,
                    state.origin.clone(),
                    "Iterator.zip inner iterator must be an object",
                )?;
                let include_outer = !state.keyed;
                return fail_iterator_zip_creation(
                    runtime,
                    state,
                    pending,
                    include_outer,
                    return_to,
                    execution_budget,
                );
            }
            state.inner_input = None;
            state.inner_iterator = Some(completion);
            state.stage = IteratorZipCreationStage::InnerNextMethod;
            let base = state
                .inner_iterator
                .as_ref()
                .expect("set above")
                .duplicate();
            read_iterator_zip_creation_property(
                runtime,
                base,
                state,
                runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        IteratorZipCreationStage::InnerNextMethod => {
            let iterator = state
                .inner_iterator
                .take()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Iterator.zip inner next lookup lost its iterator",
                })?;
            state
                .records
                .try_reserve(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: 1,
                })?;
            state
                .records
                .push(crate::object::IteratorZipRecord::new(iterator, completion));
            state.inner_input = None;
            if state.keyed {
                let key = state
                    .current_key
                    .take()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Iterator.zipKeyed iterator acquisition lost its key",
                    })?;
                state.selected_keys.try_reserve(1).map_err(|_| {
                    ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: 1,
                    }
                })?;
                state.selected_keys.push(key);
                continue_iterator_zip_keyed_inputs(runtime, state, return_to, execution_budget)
            } else {
                call_iterator_zip_outer_next(state, return_to, execution_budget)
            }
        }
        IteratorZipCreationStage::KeyedOwnKeys => {
            state.keyed_keys = generated_key_list(runtime, completion)?;
            continue_iterator_zip_keyed_inputs(runtime, state, return_to, execution_budget)
        }
        IteratorZipCreationStage::KeyedDescriptor => {
            let enumerable = internal_complete_own_property(runtime, &completion)?
                .is_some_and(|own| own.layout().is_enumerable());
            if !enumerable {
                state.current_key = None;
                return continue_iterator_zip_keyed_inputs(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                );
            }
            state.stage = IteratorZipCreationStage::KeyedValue;
            let key = state
                .current_key
                .clone()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Iterator.zipKeyed enumerable descriptor lost its key",
                })?;
            read_iterator_zip_creation_property(
                runtime,
                state.iterables.duplicate(),
                state,
                key,
                return_to,
                execution_budget,
            )
        }
        IteratorZipCreationStage::KeyedValue => {
            if matches!(completion, StoredValue::Undefined) {
                state.current_key = None;
                return continue_iterator_zip_keyed_inputs(
                    runtime,
                    state,
                    return_to,
                    execution_budget,
                );
            }
            if completion.heap_reference().is_none() {
                let pending = iterator_zip_type_error(
                    state.realm,
                    state.origin.clone(),
                    "Iterator.zipKeyed values must be objects or undefined",
                )?;
                return fail_iterator_zip_creation(
                    runtime,
                    state,
                    pending,
                    false,
                    return_to,
                    execution_budget,
                );
            }
            state.inner_input = Some(completion);
            state.stage = IteratorZipCreationStage::InnerMethod;
            let base = state.inner_input.as_ref().expect("set above").duplicate();
            read_iterator_zip_creation_property(
                runtime,
                base,
                state,
                runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
                return_to,
                execution_budget,
            )
        }
        IteratorZipCreationStage::KeyedPaddingValue => {
            state
                .padding
                .try_reserve(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: 1,
                })?;
            state.padding.push(completion);
            continue_iterator_zip_keyed_padding(runtime, state, return_to, execution_budget)
        }
        IteratorZipCreationStage::PaddingMethod => {
            let StoredValue::Function(method) = completion else {
                let pending = iterator_zip_type_error(
                    state.realm,
                    state.origin.clone(),
                    "Iterator.zip padding is not iterable",
                )?;
                return fail_iterator_zip_creation(
                    runtime,
                    state,
                    pending,
                    false,
                    return_to,
                    execution_budget,
                );
            };
            execution_budget.charge_instructions(1)?;
            let receiver = state
                .padding_input
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Iterator.zip padding iterable disappeared",
                })?
                .duplicate();
            state.stage = IteratorZipCreationStage::PaddingIterator;
            let origin = state.origin.clone();
            iterator_method_call(
                method,
                receiver,
                NativeContinuation::IteratorZipCreation(Box::new(state)),
                return_to,
                origin,
            )
        }
        IteratorZipCreationStage::PaddingIterator => {
            if completion.heap_reference().is_none() {
                let pending = iterator_zip_type_error(
                    state.realm,
                    state.origin.clone(),
                    "Iterator.zip padding iterator must be an object",
                )?;
                return fail_iterator_zip_creation(
                    runtime,
                    state,
                    pending,
                    false,
                    return_to,
                    execution_budget,
                );
            }
            state.padding_input = None;
            state.padding_iterator = Some(completion);
            state.stage = IteratorZipCreationStage::PaddingNextMethod;
            let base = state
                .padding_iterator
                .as_ref()
                .expect("set above")
                .duplicate();
            read_iterator_zip_creation_property(
                runtime,
                base,
                state,
                runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        IteratorZipCreationStage::PaddingNextMethod => {
            let StoredValue::Function(next) = completion else {
                let pending = iterator_zip_type_error(
                    state.realm,
                    state.origin.clone(),
                    "Iterator.zip padding next method is not callable",
                )?;
                return fail_iterator_zip_creation(
                    runtime,
                    state,
                    pending,
                    false,
                    return_to,
                    execution_budget,
                );
            };
            state.padding_next = Some(next);
            continue_iterator_zip_padding(runtime, state, return_to, execution_budget)
        }
        IteratorZipCreationStage::PaddingNextResult => {
            if completion.heap_reference().is_none() {
                let pending = iterator_zip_type_error(
                    state.realm,
                    state.origin.clone(),
                    "Iterator.zip padding iterator did not return an object",
                )?;
                return fail_iterator_zip_creation(
                    runtime,
                    state,
                    pending,
                    false,
                    return_to,
                    execution_budget,
                );
            }
            state.padding_result = Some(completion);
            state.stage = IteratorZipCreationStage::PaddingDone;
            let base = state
                .padding_result
                .as_ref()
                .expect("set above")
                .duplicate();
            read_iterator_zip_creation_property(
                runtime,
                base,
                state,
                runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        IteratorZipCreationStage::PaddingDone => {
            if runtime.to_boolean(&completion)? {
                state.padding_iterator = None;
                state.padding_next = None;
                state.padding_result = None;
                fill_iterator_zip_padding(&mut state)?;
                return finish_iterator_zip_creation(runtime, state);
            }
            state.stage = IteratorZipCreationStage::PaddingValue;
            let base = state
                .padding_result
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Iterator.zip padding done lookup lost its iterator result",
                })?
                .duplicate();
            read_iterator_zip_creation_property(
                runtime,
                base,
                state,
                runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        IteratorZipCreationStage::PaddingValue => {
            state.padding_result = None;
            state
                .padding
                .try_reserve(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: 1,
                })?;
            state.padding.push(completion);
            continue_iterator_zip_padding(runtime, state, return_to, execution_budget)
        }
    }
}

fn begin_iterator_zip_inputs(
    runtime: &mut Runtime,
    state: IteratorZipCreationContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if !state.keyed {
        return begin_iterator_zip_outer(runtime, state, return_to, execution_budget);
    }
    let target = state.keyed_target.ok_or(EngineFault::RuntimeInvariant {
        message: "Iterator.zipKeyed target is not an object",
    })?;
    let mut state = state;
    state.stage = IteratorZipCreationStage::KeyedOwnKeys;
    let dispatch = begin_internal_own_keys(
        runtime,
        target,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_iterator_zip_creation_after_dispatch(
        runtime,
        dispatch,
        state,
        return_to,
        execution_budget,
    )
}

fn begin_iterator_zip_outer(
    runtime: &mut Runtime,
    mut state: IteratorZipCreationContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = IteratorZipCreationStage::OuterMethod;
    let base = state.iterables.duplicate();
    read_iterator_zip_creation_property(
        runtime,
        base,
        state,
        runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        return_to,
        execution_budget,
    )
}

fn continue_iterator_zip_keyed_inputs(
    runtime: &mut Runtime,
    mut state: IteratorZipCreationContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(key) = state.keyed_keys.get(state.keyed_index).cloned() else {
        return begin_iterator_zip_padding(runtime, state, return_to, execution_budget);
    };
    state.keyed_index = state.keyed_index.saturating_add(1);
    state.current_key = Some(key.clone());
    state.stage = IteratorZipCreationStage::KeyedDescriptor;
    let target = state.keyed_target.ok_or(EngineFault::RuntimeInvariant {
        message: "Iterator.zipKeyed target disappeared",
    })?;
    let dispatch = match begin_internal_get_own_property(
        runtime,
        target,
        key,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending)) => {
            return resume_iterator_zip_creation_abrupt(
                runtime,
                state,
                pending,
                return_to,
                execution_budget,
            );
        }
        Err(error) => return Err(error),
    };
    continue_iterator_zip_creation_after_dispatch(
        runtime,
        dispatch,
        state,
        return_to,
        execution_budget,
    )
}

fn continue_iterator_zip_creation_after_dispatch(
    runtime: &mut Runtime,
    dispatch: NativeDispatch,
    state: IteratorZipCreationContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_iterator_zip_creation(runtime, state, value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::IteratorZipCreation(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::IteratorZipCreation(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Iterator.zipKeyed internal method produced a structured result",
        }
        .into()),
    }
}

fn call_iterator_zip_outer_next(
    mut state: IteratorZipCreationContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget.charge_instructions(1)?;
    let next = state.outer_next.ok_or(EngineFault::RuntimeInvariant {
        message: "Iterator.zip outer iterator has no cached next method",
    })?;
    let receiver = state
        .outer_iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Iterator.zip outer iterator disappeared",
        })?
        .duplicate();
    state.stage = IteratorZipCreationStage::OuterNextResult;
    let origin = state.origin.clone();
    iterator_method_call(
        next,
        receiver,
        NativeContinuation::IteratorZipCreation(Box::new(state)),
        return_to,
        origin,
    )
}

fn begin_iterator_zip_padding(
    runtime: &mut Runtime,
    mut state: IteratorZipCreationContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if !matches!(state.mode, crate::object::IteratorZipMode::Longest) {
        return finish_iterator_zip_creation(runtime, state);
    }
    if state.keyed {
        if state.padding_input.is_none() {
            fill_iterator_zip_padding(&mut state)?;
            return finish_iterator_zip_creation(runtime, state);
        }
        return continue_iterator_zip_keyed_padding(runtime, state, return_to, execution_budget);
    }
    let Some(base) = state.padding_input.as_ref().map(StoredValue::duplicate) else {
        fill_iterator_zip_padding(&mut state)?;
        return finish_iterator_zip_creation(runtime, state);
    };
    state.stage = IteratorZipCreationStage::PaddingMethod;
    read_iterator_zip_creation_property(
        runtime,
        base,
        state,
        runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        return_to,
        execution_budget,
    )
}

fn continue_iterator_zip_keyed_padding(
    runtime: &mut Runtime,
    mut state: IteratorZipCreationContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(key) = state.selected_keys.get(state.padding.len()).cloned() else {
        return finish_iterator_zip_creation(runtime, state);
    };
    state.stage = IteratorZipCreationStage::KeyedPaddingValue;
    let base = state
        .padding_input
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Iterator.zipKeyed padding object disappeared",
        })?
        .duplicate();
    read_iterator_zip_creation_property(runtime, base, state, key, return_to, execution_budget)
}

fn continue_iterator_zip_padding(
    runtime: &mut Runtime,
    mut state: IteratorZipCreationContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.padding.len() >= state.records.len() {
        let iterator = state
            .padding_iterator
            .take()
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Iterator.zip completed padding without an iterator",
            })?;
        return begin_iterator_zip_close(
            runtime,
            IteratorZipCloseContinuation {
                helper: None,
                iterators: vec![iterator],
                current: None,
                pending: None,
                terminal: IteratorZipCloseTerminal::Create {
                    records: state.records,
                    padding: state.padding,
                    mode: state.mode,
                    keys: None,
                },
                realm: state.realm,
                stage: IteratorZipCloseStage::ReturnProperty,
                origin: state.origin,
            },
            return_to,
            execution_budget,
        );
    }
    execution_budget.charge_instructions(1)?;
    let next = state.padding_next.ok_or(EngineFault::RuntimeInvariant {
        message: "Iterator.zip padding iterator has no cached next method",
    })?;
    let receiver = state
        .padding_iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Iterator.zip padding iterator disappeared",
        })?
        .duplicate();
    state.stage = IteratorZipCreationStage::PaddingNextResult;
    let origin = state.origin.clone();
    iterator_method_call(
        next,
        receiver,
        NativeContinuation::IteratorZipCreation(Box::new(state)),
        return_to,
        origin,
    )
}

fn fill_iterator_zip_padding(
    state: &mut IteratorZipCreationContinuation,
) -> Result<(), NativeFailure> {
    let additional = state.records.len().saturating_sub(state.padding.len());
    state
        .padding
        .try_reserve_exact(additional)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional,
        })?;
    state
        .padding
        .extend((0..additional).map(|_| StoredValue::Undefined));
    Ok(())
}

fn finish_iterator_zip_creation(
    runtime: &mut Runtime,
    state: IteratorZipCreationContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    let helper = runtime.allocate_iterator_zip_helper(
        state.realm,
        state.records,
        state.padding,
        state.mode,
        state.keyed.then_some(state.selected_keys),
    )?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(helper)))
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the owned base keeps the observable Get root alive beside the moved continuation"
)]
fn read_iterator_zip_creation_property(
    runtime: &mut Runtime,
    base: StoredValue,
    state: IteratorZipCreationContinuation,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_iterator_property_lookup(runtime, &base, execution_budget)?;
    let dispatch = match begin_value_get(
        runtime,
        &base,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending)) => {
            return resume_iterator_zip_creation_abrupt(
                runtime,
                state,
                pending,
                return_to,
                execution_budget,
            );
        }
        Err(error) => return Err(error),
    };
    continue_get_after(
        dispatch,
        Box::new(state),
        NativeContinuation::IteratorZipCreation,
        |state, value| {
            advance_iterator_zip_creation(runtime, *state, value, return_to, execution_budget)
        },
        "Iterator.zip Get produced a structured result",
    )
}

fn fail_iterator_zip_creation(
    runtime: &mut Runtime,
    mut state: IteratorZipCreationContinuation,
    pending: PendingException,
    include_outer: bool,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let additional = state
        .records
        .len()
        .saturating_add(usize::from(include_outer));
    let mut iterators = Vec::new();
    iterators
        .try_reserve_exact(additional)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional,
        })?;
    if include_outer && let Some(outer) = state.outer_iterator.take() {
        iterators.push(outer);
    }
    iterators.extend(
        state
            .records
            .iter()
            .map(|record| record.iterator().duplicate()),
    );
    begin_iterator_zip_close(
        runtime,
        IteratorZipCloseContinuation {
            helper: None,
            iterators,
            current: None,
            pending: Some(pending),
            terminal: IteratorZipCloseTerminal::Throw,
            realm: state.realm,
            stage: IteratorZipCloseStage::ReturnProperty,
            origin: state.origin,
        },
        return_to,
        execution_budget,
    )
}

pub(super) fn resume_iterator_zip_creation_abrupt(
    runtime: &mut Runtime,
    state: IteratorZipCreationContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let include_outer = !state.keyed
        && matches!(
            state.stage,
            IteratorZipCreationStage::InnerMethod
                | IteratorZipCreationStage::InnerIterator
                | IteratorZipCreationStage::InnerNextMethod
        );
    let closes_records = matches!(
        state.stage,
        IteratorZipCreationStage::OuterNextResult
            | IteratorZipCreationStage::OuterDone
            | IteratorZipCreationStage::OuterValue
            | IteratorZipCreationStage::InnerMethod
            | IteratorZipCreationStage::InnerIterator
            | IteratorZipCreationStage::InnerNextMethod
            | IteratorZipCreationStage::PaddingMethod
            | IteratorZipCreationStage::PaddingIterator
            | IteratorZipCreationStage::PaddingNextMethod
            | IteratorZipCreationStage::PaddingNextResult
            | IteratorZipCreationStage::PaddingDone
            | IteratorZipCreationStage::PaddingValue
            | IteratorZipCreationStage::KeyedDescriptor
            | IteratorZipCreationStage::KeyedValue
            | IteratorZipCreationStage::KeyedPaddingValue
    );
    if closes_records {
        fail_iterator_zip_creation(
            runtime,
            state,
            pending,
            include_outer,
            return_to,
            execution_budget,
        )
    } else {
        Err(NativeFailure::Abrupt(pending))
    }
}

fn begin_iterator_zip_close(
    runtime: &mut Runtime,
    mut state: IteratorZipCloseContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(iterator) = state.iterators.pop() else {
        return finish_iterator_zip_close(runtime, state);
    };
    state.current = Some(iterator);
    state.stage = IteratorZipCloseStage::ReturnProperty;
    let base = state.current.as_ref().expect("set above").duplicate();
    charge_iterator_property_lookup(runtime, &base, execution_budget)?;
    let dispatch = match begin_value_get(
        runtime,
        &base,
        runtime.predefined_property_key(PredefinedAtom::Return),
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending)) => {
            return resume_iterator_zip_close_abrupt(
                runtime,
                state,
                pending,
                return_to,
                execution_budget,
            );
        }
        Err(error) => return Err(error),
    };
    continue_get_after(
        dispatch,
        Box::new(state),
        NativeContinuation::IteratorZipClose,
        |state, value| {
            advance_iterator_zip_close(runtime, *state, value, return_to, execution_budget)
        },
        "Iterator.zip IteratorCloseAll return Get produced a structured result",
    )
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the close completion is consumed at the IteratorCloseAll boundary"
)]
pub(super) fn advance_iterator_zip_close(
    runtime: &mut Runtime,
    mut state: IteratorZipCloseContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IteratorZipCloseStage::ReturnProperty => match completion {
            StoredValue::Undefined | StoredValue::Null => {
                state.current = None;
                begin_iterator_zip_close(runtime, state, return_to, execution_budget)
            }
            StoredValue::Function(function) => {
                execution_budget.charge_instructions(1)?;
                let receiver = state
                    .current
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Iterator.zip close lost its current iterator",
                    })?
                    .duplicate();
                state.stage = IteratorZipCloseStage::ReturnCall;
                let origin = state.origin.clone();
                iterator_method_call(
                    function,
                    receiver,
                    NativeContinuation::IteratorZipClose(Box::new(state)),
                    return_to,
                    origin,
                )
            }
            _ => {
                record_iterator_zip_close_error(
                    &mut state,
                    "iterator return method is not callable",
                )?;
                activate_iterator_zip_creation_cleanup(&mut state)?;
                state.current = None;
                begin_iterator_zip_close(runtime, state, return_to, execution_budget)
            }
        },
        IteratorZipCloseStage::ReturnCall => {
            if completion.heap_reference().is_none() {
                record_iterator_zip_close_error(
                    &mut state,
                    "iterator return method did not return an object",
                )?;
                activate_iterator_zip_creation_cleanup(&mut state)?;
            }
            state.current = None;
            begin_iterator_zip_close(runtime, state, return_to, execution_budget)
        }
    }
}

fn record_iterator_zip_close_error(
    state: &mut IteratorZipCloseContinuation,
    message: &str,
) -> Result<(), NativeFailure> {
    if state.pending.is_none() {
        state.pending = Some(iterator_zip_type_error(
            state.realm,
            state.origin.clone(),
            message,
        )?);
    }
    Ok(())
}

pub(super) fn resume_iterator_zip_close_abrupt(
    runtime: &mut Runtime,
    mut state: IteratorZipCloseContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.pending.is_none() {
        state.pending = Some(pending);
        activate_iterator_zip_creation_cleanup(&mut state)?;
    }
    state.current = None;
    begin_iterator_zip_close(runtime, state, return_to, execution_budget)
}

fn activate_iterator_zip_creation_cleanup(
    state: &mut IteratorZipCloseContinuation,
) -> Result<(), NativeFailure> {
    if state.pending.is_none() || !matches!(state.terminal, IteratorZipCloseTerminal::Create { .. })
    {
        return Ok(());
    }
    let terminal = std::mem::replace(&mut state.terminal, IteratorZipCloseTerminal::Throw);
    let IteratorZipCloseTerminal::Create { records, .. } = terminal else {
        unreachable!("checked above")
    };
    state
        .iterators
        .try_reserve_exact(records.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: records.len(),
        })?;
    state
        .iterators
        .extend(records.iter().map(|record| record.iterator().duplicate()));
    Ok(())
}

fn finish_iterator_zip_close(
    runtime: &mut Runtime,
    state: IteratorZipCloseContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(helper) = state.helper {
        runtime.set_iterator_helper_lifecycle(
            helper,
            crate::object::IteratorHelperLifecycle::Completed,
        )?;
    }
    if let Some(pending) = state.pending {
        return Err(NativeFailure::Abrupt(pending));
    }
    match state.terminal {
        IteratorZipCloseTerminal::Create {
            records,
            padding,
            mode,
            keys,
        } => Ok(NativeDispatch::Immediate(StoredValue::Object(
            runtime.allocate_iterator_zip_helper(state.realm, records, padding, mode, keys)?,
        ))),
        IteratorZipCloseTerminal::ReturnDone => {
            iterator_result(runtime, state.realm, StoredValue::Undefined, true)
        }
        IteratorZipCloseTerminal::Throw => Err(EngineFault::RuntimeInvariant {
            message: "Iterator.zip throw close completed without a pending exception",
        }
        .into()),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "zip resume keeps the branded helper, immutable mode/key shape, realm, provenance, and execution authority explicit"
)]
pub(super) fn begin_iterator_zip_next(
    runtime: &mut Runtime,
    helper: ObjectId,
    mode: crate::object::IteratorZipMode,
    record_count: usize,
    keys: Option<Vec<PropertyKey>>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    runtime
        .set_iterator_helper_lifecycle(helper, crate::object::IteratorHelperLifecycle::Executing)?;
    if record_count == 0 {
        runtime.set_iterator_helper_lifecycle(
            helper,
            crate::object::IteratorHelperLifecycle::Completed,
        )?;
        return iterator_result(runtime, realm, StoredValue::Undefined, true);
    }
    continue_iterator_zip_next(
        runtime,
        IteratorZipNextContinuation {
            helper,
            mode,
            record_count,
            index: 0,
            values: Vec::new(),
            keys,
            result: None,
            realm,
            stage: IteratorZipNextStage::NextResult,
            origin,
        },
        return_to,
        execution_budget,
    )
}

fn continue_iterator_zip_next(
    runtime: &mut Runtime,
    mut state: IteratorZipNextContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.index >= state.record_count {
        return finish_iterator_zip_next_values(runtime, state);
    }
    let record = runtime
        .iterator_zip_record(state.helper, state.index)?
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Iterator.zip record disappeared",
        })?;
    if record.is_done() {
        if !matches!(state.mode, crate::object::IteratorZipMode::Longest) {
            return Err(EngineFault::RuntimeInvariant {
                message: "completed Iterator.zip record survived outside longest mode",
            }
            .into());
        }
        let padding = runtime
            .iterator_zip_padding(state.helper, state.index)?
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Iterator.zip padding record disappeared",
            })?;
        state
            .values
            .try_reserve(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::FrameValues,
                additional: 1,
            })?;
        state.values.push(padding);
        state.index = state.index.saturating_add(1);
        return continue_iterator_zip_next(runtime, state, return_to, execution_budget);
    }
    let StoredValue::Function(next) = record.next_method() else {
        let pending = iterator_zip_type_error(
            state.realm,
            state.origin.clone(),
            "Iterator.zip iterator next method is not callable",
        )?;
        return fail_iterator_zip_next(runtime, state, pending, return_to, execution_budget);
    };
    execution_budget.charge_instructions(1)?;
    let receiver = record.iterator().duplicate();
    state.stage = IteratorZipNextStage::NextResult;
    let origin = state.origin.clone();
    iterator_method_call(
        *next,
        receiver,
        NativeContinuation::IteratorZipNext(state),
        return_to,
        origin,
    )
}

fn continue_iterator_zip_strict_check(
    runtime: &mut Runtime,
    mut state: IteratorZipNextContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.index >= state.record_count {
        runtime.set_iterator_helper_lifecycle(
            state.helper,
            crate::object::IteratorHelperLifecycle::Completed,
        )?;
        return iterator_result(runtime, state.realm, StoredValue::Undefined, true);
    }
    let record = runtime
        .iterator_zip_record(state.helper, state.index)?
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Iterator.zip strict record disappeared",
        })?;
    let StoredValue::Function(next) = record.next_method() else {
        let pending = iterator_zip_type_error(
            state.realm,
            state.origin.clone(),
            "Iterator.zip iterator next method is not callable",
        )?;
        return fail_iterator_zip_next(runtime, state, pending, return_to, execution_budget);
    };
    execution_budget.charge_instructions(1)?;
    state.stage = IteratorZipNextStage::StrictNextResult;
    let receiver = record.iterator().duplicate();
    let origin = state.origin.clone();
    iterator_method_call(
        *next,
        receiver,
        NativeContinuation::IteratorZipNext(state),
        return_to,
        origin,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "the shared zip driver keeps shortest, longest, strict, and strict-completion steps in one auditable state machine"
)]
pub(super) fn advance_iterator_zip_next(
    runtime: &mut Runtime,
    mut state: IteratorZipNextContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IteratorZipNextStage::NextResult | IteratorZipNextStage::StrictNextResult => {
            if completion.heap_reference().is_none() {
                let pending = iterator_zip_type_error(
                    state.realm,
                    state.origin.clone(),
                    "Iterator.zip iterator did not return an object",
                )?;
                return fail_iterator_zip_next(
                    runtime,
                    state,
                    pending,
                    return_to,
                    execution_budget,
                );
            }
            let strict = matches!(state.stage, IteratorZipNextStage::StrictNextResult);
            state.result = Some(completion);
            state.stage = if strict {
                IteratorZipNextStage::StrictDone
            } else {
                IteratorZipNextStage::Done
            };
            let base = state.result.as_ref().expect("set above").duplicate();
            read_iterator_zip_next_property(
                runtime,
                base,
                state,
                runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        IteratorZipNextStage::Done => {
            if runtime.to_boolean(&completion)? {
                runtime.finish_iterator_zip_record(state.helper, state.index)?;
                state.result = None;
                match state.mode {
                    crate::object::IteratorZipMode::Shortest => close_iterator_zip_for_completion(
                        runtime,
                        state,
                        None,
                        return_to,
                        execution_budget,
                    ),
                    crate::object::IteratorZipMode::Longest => {
                        let padding = runtime
                            .iterator_zip_padding(state.helper, state.index)?
                            .ok_or(EngineFault::RuntimeInvariant {
                                message: "Iterator.zip padding record disappeared",
                            })?;
                        state.values.try_reserve(1).map_err(|_| {
                            ExecutionError::AllocationFailed {
                                resource: RuntimeResource::FrameValues,
                                additional: 1,
                            }
                        })?;
                        state.values.push(padding);
                        state.index = state.index.saturating_add(1);
                        continue_iterator_zip_next(runtime, state, return_to, execution_budget)
                    }
                    crate::object::IteratorZipMode::Strict if state.index == 0 => {
                        state.values.clear();
                        state.index = 1;
                        continue_iterator_zip_strict_check(
                            runtime,
                            state,
                            return_to,
                            execution_budget,
                        )
                    }
                    crate::object::IteratorZipMode::Strict => {
                        let pending = iterator_zip_type_error(
                            state.realm,
                            state.origin.clone(),
                            "Iterator.zip strict iterators have different lengths",
                        )?;
                        close_iterator_zip_for_completion(
                            runtime,
                            state,
                            Some(pending),
                            return_to,
                            execution_budget,
                        )
                    }
                }
            } else {
                state.stage = IteratorZipNextStage::Value;
                let base = state
                    .result
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Iterator.zip done lookup lost its iterator result",
                    })?
                    .duplicate();
                read_iterator_zip_next_property(
                    runtime,
                    base,
                    state,
                    runtime.predefined_property_key(PredefinedAtom::Value),
                    return_to,
                    execution_budget,
                )
            }
        }
        IteratorZipNextStage::Value => {
            state.result = None;
            state
                .values
                .try_reserve(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: 1,
                })?;
            state.values.push(completion);
            state.index = state.index.saturating_add(1);
            continue_iterator_zip_next(runtime, state, return_to, execution_budget)
        }
        IteratorZipNextStage::StrictDone => {
            state.result = None;
            if runtime.to_boolean(&completion)? {
                runtime.finish_iterator_zip_record(state.helper, state.index)?;
                state.index = state.index.saturating_add(1);
                continue_iterator_zip_strict_check(runtime, state, return_to, execution_budget)
            } else {
                let pending = iterator_zip_type_error(
                    state.realm,
                    state.origin.clone(),
                    "Iterator.zip strict iterators have different lengths",
                )?;
                close_iterator_zip_for_completion(
                    runtime,
                    state,
                    Some(pending),
                    return_to,
                    execution_budget,
                )
            }
        }
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the owned iterator-result base stays rooted beside the moved zip continuation"
)]
fn read_iterator_zip_next_property(
    runtime: &mut Runtime,
    base: StoredValue,
    state: IteratorZipNextContinuation,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_iterator_property_lookup(runtime, &base, execution_budget)?;
    let dispatch = match begin_value_get(
        runtime,
        &base,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending)) => {
            return resume_iterator_zip_next_abrupt(
                runtime,
                state,
                pending,
                return_to,
                execution_budget,
            );
        }
        Err(error) => return Err(error),
    };
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::IteratorZipNext,
        |state, value| {
            advance_iterator_zip_next(runtime, state, value, return_to, execution_budget)
        },
        "Iterator.zip iterator-result Get produced a structured result",
    )
}

fn finish_iterator_zip_next_values(
    runtime: &mut Runtime,
    state: IteratorZipNextContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.mode, crate::object::IteratorZipMode::Longest)
        && runtime
            .iterator_zip_open_iterators(state.helper)?
            .is_empty()
    {
        runtime.set_iterator_helper_lifecycle(
            state.helper,
            crate::object::IteratorHelperLifecycle::Completed,
        )?;
        return iterator_result(runtime, state.realm, StoredValue::Undefined, true);
    }
    let result = if let Some(keys) = state.keys {
        if keys.len() != state.values.len() {
            return Err(EngineFault::RuntimeInvariant {
                message: "Iterator.zipKeyed key/value lengths differ",
            }
            .into());
        }
        let object = runtime.allocate_ordinary_object_with_optional_prototype(None)?;
        for (key, value) in keys.into_iter().zip(state.values) {
            runtime.append_data_property(
                HeapReference::Object(object),
                key,
                PropertyLayout::data(true, true, true),
                value,
            )?;
        }
        StoredValue::Object(object)
    } else {
        StoredValue::Object(runtime.allocate_array(state.realm, state.values)?)
    };
    runtime.finish_iterator_zip_yield(state.helper)?;
    iterator_result(runtime, state.realm, result, false)
}

fn close_iterator_zip_for_completion(
    runtime: &mut Runtime,
    state: IteratorZipNextContinuation,
    pending: Option<PendingException>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let iterators = runtime.iterator_zip_open_iterators(state.helper)?;
    let terminal = if pending.is_some() {
        IteratorZipCloseTerminal::Throw
    } else {
        IteratorZipCloseTerminal::ReturnDone
    };
    begin_iterator_zip_close(
        runtime,
        IteratorZipCloseContinuation {
            helper: Some(state.helper),
            iterators,
            current: None,
            pending,
            terminal,
            realm: state.realm,
            stage: IteratorZipCloseStage::ReturnProperty,
            origin: state.origin,
        },
        return_to,
        execution_budget,
    )
}

fn fail_iterator_zip_next(
    runtime: &mut Runtime,
    state: IteratorZipNextContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    runtime.finish_iterator_zip_record(state.helper, state.index)?;
    close_iterator_zip_for_completion(runtime, state, Some(pending), return_to, execution_budget)
}

pub(super) fn resume_iterator_zip_next_abrupt(
    runtime: &mut Runtime,
    state: IteratorZipNextContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    fail_iterator_zip_next(runtime, state, pending, return_to, execution_budget)
}

pub(super) fn begin_iterator_to_array(
    runtime: &mut Runtime,
    receiver: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if receiver.heap_reference().is_none() {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator.prototype.toArray receiver must be an object",
        )?);
    }
    let state = IteratorToArrayContinuation {
        iterator: receiver,
        next_method: None,
        result: None,
        items: Vec::new(),
        realm,
        stage: IteratorToArrayStage::NextMethod,
        origin,
    };
    read_iterator_to_array_property(
        runtime,
        state,
        runtime.predefined_property_key(PredefinedAtom::Next),
        return_to,
        execution_budget,
    )
}

#[expect(
    clippy::float_cmp,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the proposal requires exact Number integrality and the validated value is a nonnegative safe integer"
)]
fn iterator_includes_skip(
    skipped_elements: Option<&StoredValue>,
) -> Result<IteratorIncludesSkip, ExceptionKind> {
    let Some(skipped_elements) = skipped_elements else {
        return Ok(IteratorIncludesSkip::Finite(0));
    };
    let StoredValue::Number(number) = skipped_elements else {
        return if matches!(skipped_elements, StoredValue::Undefined) {
            Ok(IteratorIncludesSkip::Finite(0))
        } else {
            Err(ExceptionKind::TypeError)
        };
    };
    let value = number.as_f64();
    if value.is_nan() || (value.is_finite() && value.trunc() != value) {
        return Err(ExceptionKind::TypeError);
    }
    if value < 0.0 {
        return Err(ExceptionKind::RangeError);
    }
    if value.is_infinite() {
        return Ok(IteratorIncludesSkip::Infinite);
    }
    if value > f64::from_bits(0x433f_ffff_ffff_ffff) {
        return Err(ExceptionKind::RangeError);
    }
    Ok(IteratorIncludesSkip::Finite(value as u64))
}

/// `Iterator.prototype.includes`, including validation-before-`next` and
/// normal close on a successful match.
#[allow(
    clippy::too_many_arguments,
    reason = "the native boundary keeps both arguments, receiver, realm, source origin, return target, and budget explicit"
)]
pub(super) fn begin_iterator_includes(
    runtime: &mut Runtime,
    receiver: StoredValue,
    search_element: StoredValue,
    skipped_elements: Option<&StoredValue>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if receiver.heap_reference().is_none() {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator.prototype.includes receiver must be an object",
        )?);
    }
    let to_skip = match iterator_includes_skip(skipped_elements) {
        Ok(to_skip) => to_skip,
        Err(kind) => {
            let message = match kind {
                ExceptionKind::TypeError => {
                    "Iterator.prototype.includes skippedElements must be an integral Number"
                }
                ExceptionKind::RangeError => {
                    "Iterator.prototype.includes skippedElements is outside the allowed range"
                }
                _ => {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "Iterator.prototype.includes validation produced an invalid exception kind",
                    }
                    .into());
                }
            };
            let pending = PendingException {
                realm,
                payload: PendingExceptionPayload::EngineError {
                    kind,
                    message: JsString::from_utf8(message)?,
                },
                origin,
            };
            return begin_exceptional_iterator_close(
                runtime,
                receiver,
                pending,
                return_to,
                execution_budget,
            );
        }
    };
    let state = IteratorIncludesContinuation {
        iterator: receiver,
        next_method: None,
        result: None,
        search_element,
        to_skip,
        realm,
        stage: IteratorIncludesStage::NextMethod,
        origin,
    };
    read_iterator_includes_property(
        runtime,
        state,
        runtime.predefined_property_key(PredefinedAtom::Next),
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "one typed state machine exposes every IteratorStepValue and normal-close suspension boundary"
)]
pub(super) fn advance_iterator_includes(
    runtime: &mut Runtime,
    mut state: IteratorIncludesContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IteratorIncludesStage::NextMethod => {
            let StoredValue::Function(next_method) = completion else {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "iterator next method is not callable",
                )?);
            };
            state.next_method = Some(next_method);
            call_iterator_includes_next(state, return_to, execution_budget)
        }
        IteratorIncludesStage::NextResult => {
            if completion.heap_reference().is_none() {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "iterator next method did not return an object",
                )?);
            }
            state.result = Some(completion);
            state.stage = IteratorIncludesStage::Done;
            read_iterator_includes_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        IteratorIncludesStage::Done => {
            if runtime.to_boolean(&completion)? {
                return Ok(NativeDispatch::Immediate(StoredValue::Boolean(false)));
            }
            state.stage = IteratorIncludesStage::Value;
            read_iterator_includes_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        IteratorIncludesStage::Value => {
            state.result = None;
            if state.to_skip.consume_one() {
                return call_iterator_includes_next(state, return_to, execution_budget);
            }
            if !completion.same_value_zero(&state.search_element) {
                return call_iterator_includes_next(state, return_to, execution_budget);
            }
            state.stage = IteratorIncludesStage::CloseReturnProperty;
            read_iterator_includes_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Return),
                return_to,
                execution_budget,
            )
        }
        IteratorIncludesStage::CloseReturnProperty => match completion {
            StoredValue::Undefined | StoredValue::Null => {
                Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)))
            }
            StoredValue::Function(return_method) => {
                state.stage = IteratorIncludesStage::CloseReturnCall;
                execution_budget.charge_instructions(1)?;
                let receiver = state.iterator.duplicate();
                let origin = state.origin.clone();
                iterator_method_call(
                    return_method,
                    receiver,
                    NativeContinuation::IteratorIncludes(state),
                    return_to,
                    origin,
                )
            }
            StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => Err(iterator_exception(
                state.realm,
                state.origin,
                ExceptionKind::TypeError,
                "iterator return method is not callable",
            )?),
        },
        IteratorIncludesStage::CloseReturnCall => {
            if completion.heap_reference().is_none() {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "iterator return method did not return an object",
                )?);
            }
            Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)))
        }
    }
}

fn call_iterator_includes_next(
    mut state: IteratorIncludesContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let next_method = state.next_method.ok_or(EngineFault::RuntimeInvariant {
        message: "Iterator.prototype.includes has no callable next method",
    })?;
    state.result = None;
    state.stage = IteratorIncludesStage::NextResult;
    execution_budget.charge_instructions(1)?;
    let receiver = state.iterator.duplicate();
    let origin = state.origin.clone();
    iterator_method_call(
        next_method,
        receiver,
        NativeContinuation::IteratorIncludes(state),
        return_to,
        origin,
    )
}

fn read_iterator_includes_property(
    runtime: &mut Runtime,
    state: IteratorIncludesContinuation,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = match state.stage {
        IteratorIncludesStage::NextMethod | IteratorIncludesStage::CloseReturnProperty => {
            &state.iterator
        }
        IteratorIncludesStage::Done | IteratorIncludesStage::Value => {
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "Iterator.prototype.includes result lookup has no result object",
            })?
        }
        IteratorIncludesStage::NextResult | IteratorIncludesStage::CloseReturnCall => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Iterator.prototype.includes call stage attempted a property lookup",
            }
            .into());
        }
    };
    charge_iterator_property_lookup(runtime, base, execution_budget)?;
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
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::IteratorIncludes,
        |state, value| {
            advance_iterator_includes(runtime, state, value, return_to, execution_budget)
        },
        "Iterator.prototype.includes property Get produced a structured result",
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch keeps the consumer kind, receiver, callback, optional accumulator, realm, source origin, and shared budget explicit"
)]
pub(super) fn begin_iterator_consumer(
    runtime: &mut Runtime,
    kind: crate::runtime::IteratorConsumer,
    receiver: StoredValue,
    callback: &StoredValue,
    initial_value: Option<StoredValue>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if receiver.heap_reference().is_none() {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator consumer receiver must be an object",
        )?);
    }
    let StoredValue::Function(callback) = callback else {
        let NativeFailure::Abrupt(pending) = iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator consumer callback must be callable",
        )?
        else {
            unreachable!("iterator_exception always returns an abrupt completion")
        };
        return begin_exceptional_iterator_close(
            runtime,
            receiver,
            pending,
            return_to,
            execution_budget,
        );
    };
    let state = IteratorConsumerContinuation {
        iterator: receiver,
        next_method: None,
        callback: *callback,
        kind,
        counter: 0,
        result: None,
        candidate: None,
        accumulator: initial_value,
        outcome: None,
        realm,
        stage: IteratorConsumerStage::NextMethod,
        origin,
    };
    read_iterator_consumer_property(
        runtime,
        state,
        runtime.predefined_property_key(PredefinedAtom::Next),
        return_to,
        execution_budget,
    )
}

pub(super) fn begin_iterator_dispose(
    runtime: &mut Runtime,
    receiver: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = IteratorDisposeContinuation {
        iterator: receiver,
        realm,
        stage: IteratorDisposeStage::ReturnProperty,
        origin,
    };
    read_iterator_dispose_return(runtime, state, return_to, execution_budget)
}

pub(super) fn begin_iterator_map(
    runtime: &mut Runtime,
    receiver: StoredValue,
    callback: &StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_iterator_callback_helper(
        runtime,
        receiver,
        callback,
        crate::object::IteratorHelperKind::Map,
        "Iterator.prototype.map receiver must be an object",
        "Iterator.prototype.map mapper must be callable",
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn begin_iterator_filter(
    runtime: &mut Runtime,
    receiver: StoredValue,
    callback: &StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_iterator_callback_helper(
        runtime,
        receiver,
        callback,
        crate::object::IteratorHelperKind::Filter,
        "Iterator.prototype.filter receiver must be an object",
        "Iterator.prototype.filter predicate must be callable",
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn begin_iterator_flat_map(
    runtime: &mut Runtime,
    receiver: StoredValue,
    callback: &StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_iterator_callback_helper(
        runtime,
        receiver,
        callback,
        crate::object::IteratorHelperKind::FlatMap,
        "Iterator.prototype.flatMap receiver must be an object",
        "Iterator.prototype.flatMap mapper must be callable",
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the resumable helper constructor keeps the observable method diagnostics and VM call state explicit"
)]
fn begin_iterator_callback_helper(
    runtime: &mut Runtime,
    receiver: StoredValue,
    callback: &StoredValue,
    kind: crate::object::IteratorHelperKind,
    receiver_message: &str,
    callback_message: &str,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if receiver.heap_reference().is_none() {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            receiver_message,
        )?);
    }
    let StoredValue::Function(callback) = callback else {
        let NativeFailure::Abrupt(pending) =
            iterator_exception(realm, origin, ExceptionKind::TypeError, callback_message)?
        else {
            unreachable!("iterator_exception always returns an abrupt completion")
        };
        return begin_exceptional_iterator_close(
            runtime,
            receiver,
            pending,
            return_to,
            execution_budget,
        );
    };
    let state = IteratorHelperCreationContinuation {
        iterator: receiver,
        kind,
        callback: Some(*callback),
        remaining: 0.0,
        realm,
        origin,
    };
    begin_iterator_helper_creation(runtime, state, return_to, execution_budget)
}

pub(super) fn begin_iterator_take(
    runtime: &mut Runtime,
    receiver: StoredValue,
    limit: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_iterator_limit(
        runtime,
        IteratorLimitContinuation {
            iterator: receiver,
            kind: crate::object::IteratorHelperKind::Take,
            realm,
            origin,
        },
        limit,
        return_to,
        execution_budget,
    )
}

pub(super) fn begin_iterator_drop(
    runtime: &mut Runtime,
    receiver: StoredValue,
    limit: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_iterator_limit(
        runtime,
        IteratorLimitContinuation {
            iterator: receiver,
            kind: crate::object::IteratorHelperKind::Drop,
            realm,
            origin,
        },
        limit,
        return_to,
        execution_budget,
    )
}

fn begin_iterator_limit(
    runtime: &mut Runtime,
    state: IteratorLimitContinuation,
    limit: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.iterator.heap_reference().is_none() {
        return Err(iterator_exception(
            state.realm,
            state.origin,
            ExceptionKind::TypeError,
            "Iterator Helper receiver must be an object",
        )?);
    }
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        limit,
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::IteratorLimit(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn advance_iterator_limit(
    runtime: &mut Runtime,
    state: IteratorLimitContinuation,
    limit: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let number = match operator_to_number(limit, state.realm, &state.origin) {
        Ok(number) => number,
        Err(NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending)) => {
            return resume_iterator_limit_abrupt(
                runtime,
                state,
                pending,
                return_to,
                execution_budget,
            );
        }
        Err(error) => return Err(error),
    };
    let raw_limit = number.as_f64();
    if raw_limit.is_nan() {
        return close_iterator_limit_range_error(runtime, state, return_to, execution_budget);
    }
    let remaining = number_to_integer_or_infinity(number);
    if remaining.is_sign_negative() && remaining != 0.0 {
        return close_iterator_limit_range_error(runtime, state, return_to, execution_budget);
    }
    begin_iterator_helper_creation(
        runtime,
        IteratorHelperCreationContinuation {
            iterator: state.iterator,
            kind: state.kind,
            callback: None,
            remaining,
            realm: state.realm,
            origin: state.origin,
        },
        return_to,
        execution_budget,
    )
}

pub(super) fn resume_iterator_limit_abrupt(
    runtime: &mut Runtime,
    state: IteratorLimitContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_exceptional_iterator_close(
        runtime,
        state.iterator,
        pending,
        return_to,
        execution_budget,
    )
}

fn close_iterator_limit_range_error(
    runtime: &mut Runtime,
    state: IteratorLimitContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let NativeFailure::Abrupt(pending) = iterator_exception(
        state.realm,
        state.origin,
        ExceptionKind::RangeError,
        "Iterator Helper limit must be a non-negative number",
    )?
    else {
        unreachable!("iterator_exception always returns an abrupt completion")
    };
    begin_exceptional_iterator_close(
        runtime,
        state.iterator,
        pending,
        return_to,
        execution_budget,
    )
}

fn begin_iterator_helper_creation(
    runtime: &mut Runtime,
    state: IteratorHelperCreationContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_iterator_property_lookup(runtime, &state.iterator, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        &state.iterator,
        runtime.predefined_property_key(PredefinedAtom::Next),
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::IteratorHelperCreation,
        |state, value| advance_iterator_helper_creation(runtime, state, value),
        "Iterator helper next Get produced a structured result",
    )
}

pub(super) fn advance_iterator_helper_creation(
    runtime: &mut Runtime,
    state: IteratorHelperCreationContinuation,
    next_method: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let helper = match state.kind {
        crate::object::IteratorHelperKind::Map
        | crate::object::IteratorHelperKind::Filter
        | crate::object::IteratorHelperKind::FlatMap => {
            let callback = state.callback.ok_or(EngineFault::RuntimeInvariant {
                message: "callback Iterator Helper creation has no callback",
            })?;
            runtime.allocate_iterator_callback_helper(
                state.realm,
                state.iterator,
                next_method,
                state.kind,
                callback,
            )?
        }
        crate::object::IteratorHelperKind::Take | crate::object::IteratorHelperKind::Drop => {
            runtime.allocate_iterator_limit_helper(
                state.realm,
                state.iterator,
                next_method,
                state.kind,
                state.remaining,
            )?
        }
        crate::object::IteratorHelperKind::Concat | crate::object::IteratorHelperKind::Zip => {
            return Err(EngineFault::RuntimeInvariant {
                message: "static Iterator helper used the ordinary helper constructor",
            }
            .into());
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::Object(helper)))
}

#[allow(
    clippy::too_many_lines,
    reason = "helper resume validates the branded lifecycle and retained inner record before selecting one auditable call boundary"
)]
pub(super) fn begin_iterator_helper_next(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(helper) = receiver else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator Helper object expected",
        )?);
    };
    let helper = *helper;
    let Some(snapshot) = runtime.iterator_helper_snapshot(helper)? else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator Helper object expected",
        )?);
    };
    match snapshot.lifecycle {
        crate::object::IteratorHelperLifecycle::Completed => {
            return iterator_result(runtime, realm, StoredValue::Undefined, true);
        }
        crate::object::IteratorHelperLifecycle::Executing => {
            return Err(iterator_exception(
                realm,
                origin,
                ExceptionKind::TypeError,
                "Iterator Helper is already running",
            )?);
        }
        crate::object::IteratorHelperLifecycle::SuspendedStart
        | crate::object::IteratorHelperLifecycle::SuspendedYield => {}
    }
    if matches!(snapshot.kind, crate::object::IteratorHelperKind::Zip) {
        return begin_iterator_zip_next(
            runtime,
            helper,
            snapshot.zip_mode,
            snapshot.zip_record_count,
            snapshot.zip_keys,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if matches!(snapshot.kind, crate::object::IteratorHelperKind::Concat) {
        runtime.set_iterator_helper_lifecycle(
            helper,
            crate::object::IteratorHelperLifecycle::Executing,
        )?;
        let has_inner = match (&snapshot.inner_iterator, &snapshot.inner_next_method) {
            (Some(_), Some(_)) => true,
            (None, None) => false,
            (Some(_), None) | (None, Some(_)) => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Iterator.concat retained an incomplete active iterator record",
                }
                .into());
            }
        };
        let state = IteratorHelperNextContinuation {
            helper,
            iterator: snapshot.iterator,
            next_method: snapshot.next_method,
            kind: snapshot.kind,
            callback: None,
            counter: 0,
            remaining: 0.0,
            dropping: false,
            result: None,
            candidate: None,
            inner_iterator: snapshot.inner_iterator,
            inner_next_method: snapshot.inner_next_method,
            concat_iterable: snapshot.concat_iterable,
            realm,
            stage: IteratorHelperNextStage::ConcatNextResult,
            origin,
        };
        if has_inner {
            return call_iterator_concat_inner_next(runtime, state, return_to, execution_budget);
        }
        return open_iterator_concat_iterable(runtime, state, return_to, execution_budget);
    }
    if matches!(snapshot.kind, crate::object::IteratorHelperKind::Take) && snapshot.remaining == 0.0
    {
        return begin_iterator_helper_return(
            runtime,
            receiver,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    runtime
        .set_iterator_helper_lifecycle(helper, crate::object::IteratorHelperLifecycle::Executing)?;
    let mut remaining = snapshot.remaining;
    let mut dropping = false;
    if matches!(snapshot.kind, crate::object::IteratorHelperKind::Take) {
        remaining = runtime.consume_iterator_helper_remaining(helper)?;
    } else if matches!(snapshot.kind, crate::object::IteratorHelperKind::Drop) && remaining > 0.0 {
        remaining = runtime.consume_iterator_helper_remaining(helper)?;
        dropping = true;
    }
    let StoredValue::Function(next_method) = snapshot.next_method else {
        runtime.set_iterator_helper_lifecycle(
            helper,
            crate::object::IteratorHelperLifecycle::Completed,
        )?;
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "iterator next method is not callable",
        )?);
    };
    execution_budget.charge_instructions(1)?;
    let has_inner = match (&snapshot.inner_iterator, &snapshot.inner_next_method) {
        (Some(_), Some(_)) => true,
        (None, None) => false,
        (Some(_), None) | (None, Some(_)) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "flatMap helper retained an incomplete inner iterator record",
            }
            .into());
        }
    };
    let state = IteratorHelperNextContinuation {
        helper,
        iterator: snapshot.iterator,
        next_method: StoredValue::Function(next_method),
        kind: snapshot.kind,
        callback: snapshot.callback,
        counter: snapshot.counter,
        remaining,
        dropping,
        result: None,
        candidate: None,
        inner_iterator: snapshot.inner_iterator,
        inner_next_method: snapshot.inner_next_method,
        concat_iterable: None,
        realm,
        stage: if matches!(snapshot.kind, crate::object::IteratorHelperKind::FlatMap) && has_inner {
            IteratorHelperNextStage::InnerNextResult
        } else {
            IteratorHelperNextStage::NextResult
        },
        origin: origin.clone(),
    };
    if has_inner {
        call_iterator_helper_inner_next(runtime, state, return_to, execution_budget)
    } else {
        iterator_method_call(
            next_method,
            state.iterator.duplicate(),
            NativeContinuation::IteratorHelperNext(state),
            return_to,
            origin,
        )
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the Iterator Helper generator stages stay explicit so each observable suspension and close boundary is auditable"
)]
pub(super) fn advance_iterator_helper_next(
    runtime: &mut Runtime,
    mut state: IteratorHelperNextContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IteratorHelperNextStage::ConcatIteratorCall => {
            if completion.heap_reference().is_none() {
                return fail_iterator_concat_with_type_error(
                    runtime,
                    state,
                    "Iterator.concat iterable method did not return an object",
                );
            }
            state.concat_iterable = None;
            state.inner_iterator = Some(completion);
            state.stage = IteratorHelperNextStage::ConcatNextMethod;
            read_iterator_concat_inner_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        IteratorHelperNextStage::ConcatNextMethod => {
            state.inner_next_method = Some(completion);
            let iterator = state
                .inner_iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Iterator.concat next lookup lost its iterator",
                })?
                .duplicate();
            let next_method = state
                .inner_next_method
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Iterator.concat next lookup lost its result",
                })?
                .duplicate();
            runtime.install_iterator_helper_inner(state.helper, iterator, next_method)?;
            state.stage = IteratorHelperNextStage::ConcatNextResult;
            call_iterator_concat_inner_next(runtime, state, return_to, execution_budget)
        }
        IteratorHelperNextStage::ConcatNextResult => {
            if completion.heap_reference().is_none() {
                return fail_iterator_concat_with_type_error(
                    runtime,
                    state,
                    "Iterator.concat inner next method did not return an object",
                );
            }
            state.result = Some(completion);
            state.stage = IteratorHelperNextStage::ConcatDone;
            read_iterator_helper_next_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        IteratorHelperNextStage::ConcatDone => {
            if runtime.to_boolean(&completion)? {
                runtime.finish_iterator_concat_inner(state.helper)?;
                state.inner_iterator = None;
                state.inner_next_method = None;
                state.result = None;
                state.concat_iterable = runtime.current_iterator_concat_iterable(state.helper)?;
                return open_iterator_concat_iterable(runtime, state, return_to, execution_budget);
            }
            state.stage = IteratorHelperNextStage::ConcatValue;
            read_iterator_helper_next_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        IteratorHelperNextStage::ConcatValue => {
            runtime.set_iterator_helper_lifecycle(
                state.helper,
                crate::object::IteratorHelperLifecycle::SuspendedYield,
            )?;
            iterator_result(runtime, state.realm, completion, false)
        }
        IteratorHelperNextStage::NextResult => {
            if completion.heap_reference().is_none() {
                complete_iterator_helper(runtime, state.helper)?;
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "iterator next method did not return an object",
                )?);
            }
            state.result = Some(completion);
            state.stage = IteratorHelperNextStage::Done;
            read_iterator_helper_next_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        IteratorHelperNextStage::Done => {
            if runtime.to_boolean(&completion)? {
                complete_iterator_helper(runtime, state.helper)?;
                return iterator_result(runtime, state.realm, StoredValue::Undefined, true);
            }
            if matches!(state.kind, crate::object::IteratorHelperKind::Drop) && state.dropping {
                return continue_iterator_drop(runtime, state, return_to, execution_budget);
            }
            state.stage = IteratorHelperNextStage::Value;
            read_iterator_helper_next_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        IteratorHelperNextStage::Value => {
            if matches!(
                state.kind,
                crate::object::IteratorHelperKind::Take | crate::object::IteratorHelperKind::Drop
            ) {
                runtime.finish_iterator_limit_yield(state.helper)?;
                return iterator_result(runtime, state.realm, completion, false);
            }
            let mut arguments = Vec::new();
            arguments
                .try_reserve_exact(2)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: 2,
                })?;
            if matches!(state.kind, crate::object::IteratorHelperKind::Filter) {
                state.candidate = Some(completion.duplicate());
            }
            arguments.push(completion);
            arguments.push(StoredValue::Number(iterator_counter_number(state.counter)));
            state.result = None;
            state.stage = IteratorHelperNextStage::Callback;
            execution_budget.charge_instructions(1)?;
            let origin = state.origin.clone();
            let callback = state.callback.ok_or(EngineFault::RuntimeInvariant {
                message: "callback Iterator Helper has no callback",
            })?;
            iterator_call_with_arguments(
                callback,
                StoredValue::Undefined,
                arguments,
                NativeContinuation::IteratorHelperNext(state),
                return_to,
                origin,
            )
        }
        IteratorHelperNextStage::Callback => advance_iterator_helper_callback(
            runtime,
            state,
            completion,
            return_to,
            execution_budget,
        ),
        IteratorHelperNextStage::InnerIteratorMethod => advance_iterator_flat_map_method(
            runtime,
            state,
            &completion,
            return_to,
            execution_budget,
        ),
        IteratorHelperNextStage::InnerIteratorCall => {
            if completion.heap_reference().is_none() {
                return close_iterator_helper_with_type_error(
                    runtime,
                    state,
                    "flatMap iterator method did not return an object",
                    return_to,
                    execution_budget,
                );
            }
            state.inner_iterator = Some(completion);
            state.stage = IteratorHelperNextStage::InnerNextMethod;
            read_iterator_flat_map_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        IteratorHelperNextStage::InnerNextMethod => {
            state.inner_next_method = Some(completion);
            let inner_iterator = state
                .inner_iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "flatMap next lookup completed without an inner iterator",
                })?
                .duplicate();
            let inner_next_method = state
                .inner_next_method
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "flatMap next lookup lost its result",
                })?
                .duplicate();
            runtime.install_iterator_helper_inner(
                state.helper,
                inner_iterator,
                inner_next_method,
            )?;
            state.stage = IteratorHelperNextStage::InnerNextResult;
            call_iterator_helper_inner_next(runtime, state, return_to, execution_budget)
        }
        IteratorHelperNextStage::InnerNextResult => {
            if completion.heap_reference().is_none() {
                return close_iterator_helper_with_type_error(
                    runtime,
                    state,
                    "flatMap inner next method did not return an object",
                    return_to,
                    execution_budget,
                );
            }
            state.result = Some(completion);
            state.stage = IteratorHelperNextStage::InnerDone;
            read_iterator_helper_next_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        IteratorHelperNextStage::InnerDone => {
            if runtime.to_boolean(&completion)? {
                runtime.finish_iterator_flat_map_inner(state.helper)?;
                state.counter = state.counter.saturating_add(1);
                state.inner_iterator = None;
                state.inner_next_method = None;
                state.result = None;
                state.stage = IteratorHelperNextStage::NextResult;
                return call_iterator_helper_outer_next(state, return_to, execution_budget);
            }
            state.stage = IteratorHelperNextStage::InnerValue;
            read_iterator_helper_next_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        IteratorHelperNextStage::InnerValue => {
            runtime.finish_iterator_flat_map_yield(state.helper)?;
            iterator_result(runtime, state.realm, completion, false)
        }
    }
}

fn open_iterator_concat_iterable(
    runtime: &mut Runtime,
    mut state: IteratorHelperNextContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let iterable = if let Some(iterable) = state.concat_iterable.take() {
        iterable
    } else {
        let Some(iterable) = runtime.current_iterator_concat_iterable(state.helper)? else {
            complete_iterator_helper(runtime, state.helper)?;
            return iterator_result(runtime, state.realm, StoredValue::Undefined, true);
        };
        iterable
    };
    let open_method = iterable.open_method();
    let receiver = iterable.iterable().duplicate();
    state.concat_iterable = Some(iterable);
    state.stage = IteratorHelperNextStage::ConcatIteratorCall;
    execution_budget.charge_instructions(1)?;
    let origin = state.origin.clone();
    iterator_method_call(
        open_method,
        receiver,
        NativeContinuation::IteratorHelperNext(state),
        return_to,
        origin,
    )
}

fn read_iterator_concat_inner_property(
    runtime: &mut Runtime,
    state: IteratorHelperNextContinuation,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let iterator = state
        .inner_iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Iterator.concat inner property lookup has no iterator",
        })?;
    charge_iterator_property_lookup(runtime, iterator, execution_budget)?;
    let dispatch = match begin_value_get(
        runtime,
        iterator,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending)) => {
            complete_iterator_helper(runtime, state.helper)?;
            return Err(NativeFailure::Abrupt(pending));
        }
        Err(error) => return Err(error),
    };
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::IteratorHelperNext,
        |state, value| {
            advance_iterator_helper_next(runtime, state, value, return_to, execution_budget)
        },
        "Iterator.concat inner property Get produced a structured result",
    )
}

fn call_iterator_concat_inner_next(
    runtime: &mut Runtime,
    state: IteratorHelperNextContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(iterator) = state.inner_iterator.as_ref() else {
        return Err(EngineFault::RuntimeInvariant {
            message: "Iterator.concat next call has no iterator",
        }
        .into());
    };
    let Some(StoredValue::Function(next_method)) = state.inner_next_method.as_ref() else {
        return fail_iterator_concat_with_type_error(
            runtime,
            state,
            "Iterator.concat inner next method is not callable",
        );
    };
    let next_method = *next_method;
    let receiver = iterator.duplicate();
    execution_budget.charge_instructions(1)?;
    let origin = state.origin.clone();
    iterator_method_call(
        next_method,
        receiver,
        NativeContinuation::IteratorHelperNext(state),
        return_to,
        origin,
    )
}

fn fail_iterator_concat_with_type_error(
    runtime: &mut Runtime,
    state: IteratorHelperNextContinuation,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    complete_iterator_helper(runtime, state.helper)?;
    Err(iterator_exception(
        state.realm,
        state.origin,
        ExceptionKind::TypeError,
        message,
    )?)
}

fn continue_iterator_drop(
    runtime: &mut Runtime,
    mut state: IteratorHelperNextContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.result = None;
    state.stage = IteratorHelperNextStage::NextResult;
    if state.remaining > 0.0 {
        state.remaining = runtime.consume_iterator_helper_remaining(state.helper)?;
    } else {
        state.dropping = false;
    }
    execution_budget.charge_instructions(1)?;
    let StoredValue::Function(next_method) = state.next_method else {
        return Err(EngineFault::RuntimeInvariant {
            message: "running Iterator Helper lost its callable next method",
        }
        .into());
    };
    let receiver = state.iterator.duplicate();
    let origin = state.origin.clone();
    iterator_method_call(
        next_method,
        receiver,
        NativeContinuation::IteratorHelperNext(state),
        return_to,
        origin,
    )
}

fn advance_iterator_helper_callback(
    runtime: &mut Runtime,
    mut state: IteratorHelperNextContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.kind {
        crate::object::IteratorHelperKind::Map => {
            runtime.finish_iterator_helper_callback(state.helper, true)?;
            iterator_result(runtime, state.realm, completion, false)
        }
        crate::object::IteratorHelperKind::Filter => {
            let selected = runtime.to_boolean(&completion)?;
            runtime.finish_iterator_helper_callback(state.helper, selected)?;
            let candidate = state
                .candidate
                .take()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Iterator filter callback has no candidate value",
                })?;
            if selected {
                return iterator_result(runtime, state.realm, candidate, false);
            }
            state.counter = state.counter.saturating_add(1);
            state.stage = IteratorHelperNextStage::NextResult;
            execution_budget.charge_instructions(1)?;
            let StoredValue::Function(next_method) = state.next_method else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "running Iterator Helper lost its callable next method",
                }
                .into());
            };
            let receiver = state.iterator.duplicate();
            let origin = state.origin.clone();
            iterator_method_call(
                next_method,
                receiver,
                NativeContinuation::IteratorHelperNext(state),
                return_to,
                origin,
            )
        }
        crate::object::IteratorHelperKind::FlatMap => {
            if completion.heap_reference().is_none() {
                return close_iterator_helper_with_type_error(
                    runtime,
                    state,
                    "flatMap mapper result must be an object",
                    return_to,
                    execution_budget,
                );
            }
            state.candidate = Some(completion);
            state.stage = IteratorHelperNextStage::InnerIteratorMethod;
            let mapped = state
                .candidate
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "flatMap lost its mapped value",
                })?
                .duplicate();
            read_iterator_flat_map_property_from(
                runtime,
                state,
                &mapped,
                runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
                return_to,
                execution_budget,
            )
        }
        crate::object::IteratorHelperKind::Take | crate::object::IteratorHelperKind::Drop => {
            Err(EngineFault::RuntimeInvariant {
                message: "limit Iterator Helper resumed from a callback",
            }
            .into())
        }
        crate::object::IteratorHelperKind::Concat | crate::object::IteratorHelperKind::Zip => {
            Err(EngineFault::RuntimeInvariant {
                message: "static Iterator helper resumed from a callback",
            }
            .into())
        }
    }
}

fn advance_iterator_flat_map_method(
    runtime: &mut Runtime,
    mut state: IteratorHelperNextContinuation,
    completion: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mapped = state
        .candidate
        .take()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "flatMap iterator-method lookup lost its mapped value",
        })?;
    match completion {
        StoredValue::Undefined | StoredValue::Null => {
            state.inner_iterator = Some(mapped);
            state.stage = IteratorHelperNextStage::InnerNextMethod;
            read_iterator_flat_map_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        StoredValue::Function(method) => {
            state.stage = IteratorHelperNextStage::InnerIteratorCall;
            execution_budget.charge_instructions(1)?;
            let origin = state.origin.clone();
            iterator_method_call(
                *method,
                mapped,
                NativeContinuation::IteratorHelperNext(state),
                return_to,
                origin,
            )
        }
        StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Object(_) => close_iterator_helper_with_type_error(
            runtime,
            state,
            "flatMap Symbol.iterator method is not callable",
            return_to,
            execution_budget,
        ),
    }
}

fn call_iterator_helper_outer_next(
    state: IteratorHelperNextContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget.charge_instructions(1)?;
    let StoredValue::Function(next_method) = state.next_method else {
        return Err(EngineFault::RuntimeInvariant {
            message: "running Iterator Helper lost its callable next method",
        }
        .into());
    };
    let receiver = state.iterator.duplicate();
    let origin = state.origin.clone();
    iterator_method_call(
        next_method,
        receiver,
        NativeContinuation::IteratorHelperNext(state),
        return_to,
        origin,
    )
}

fn call_iterator_helper_inner_next(
    runtime: &mut Runtime,
    state: IteratorHelperNextContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(inner_iterator) = state.inner_iterator.as_ref() else {
        return Err(EngineFault::RuntimeInvariant {
            message: "flatMap inner next call has no iterator",
        }
        .into());
    };
    let Some(StoredValue::Function(next_method)) = state.inner_next_method.as_ref() else {
        return close_iterator_helper_with_type_error(
            runtime,
            state,
            "flatMap inner next method is not callable",
            return_to,
            execution_budget,
        );
    };
    let next_method = *next_method;
    let receiver = inner_iterator.duplicate();
    execution_budget.charge_instructions(1)?;
    let origin = state.origin.clone();
    iterator_method_call(
        next_method,
        receiver,
        NativeContinuation::IteratorHelperNext(state),
        return_to,
        origin,
    )
}

fn read_iterator_flat_map_property(
    runtime: &mut Runtime,
    state: IteratorHelperNextContinuation,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = state
        .inner_iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "flatMap inner property lookup has no iterator",
        })?
        .duplicate();
    read_iterator_flat_map_property_from(runtime, state, &base, key, return_to, execution_budget)
}

fn read_iterator_flat_map_property_from(
    runtime: &mut Runtime,
    state: IteratorHelperNextContinuation,
    base: &StoredValue,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_iterator_property_lookup(runtime, base, execution_budget)?;
    let dispatch = match begin_value_get(
        runtime,
        base,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending)) => {
            return close_iterator_helper_abrupt(
                runtime,
                state,
                pending,
                return_to,
                execution_budget,
            );
        }
        Err(error) => return Err(error),
    };
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::IteratorHelperNext,
        |state, value| {
            advance_iterator_helper_next(runtime, state, value, return_to, execution_budget)
        },
        "flatMap property Get produced a structured result",
    )
}

fn close_iterator_helper_with_type_error(
    runtime: &mut Runtime,
    state: IteratorHelperNextContinuation,
    message: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let NativeFailure::Abrupt(pending) = iterator_exception(
        state.realm,
        state.origin.clone(),
        ExceptionKind::TypeError,
        message,
    )?
    else {
        unreachable!("iterator_exception always returns an abrupt completion")
    };
    close_iterator_helper_abrupt(runtime, state, pending, return_to, execution_budget)
}

fn close_iterator_helper_abrupt(
    runtime: &mut Runtime,
    state: IteratorHelperNextContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    complete_iterator_helper(runtime, state.helper)?;
    begin_exceptional_iterator_close(
        runtime,
        state.iterator,
        pending,
        return_to,
        execution_budget,
    )
}

fn read_iterator_helper_next_property(
    runtime: &mut Runtime,
    state: IteratorHelperNextContinuation,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let result = state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "Iterator Helper result lookup has no result object",
    })?;
    charge_iterator_property_lookup(runtime, result, execution_budget)?;
    let dispatch = match begin_value_get(
        runtime,
        result,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending)) => {
            if iterator_helper_stage_closes_outer(state.stage) {
                return close_iterator_helper_abrupt(
                    runtime,
                    state,
                    pending,
                    return_to,
                    execution_budget,
                );
            }
            complete_iterator_helper(runtime, state.helper)?;
            return Err(NativeFailure::Abrupt(pending));
        }
        Err(error) => return Err(error),
    };
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::IteratorHelperNext,
        |state, value| {
            advance_iterator_helper_next(runtime, state, value, return_to, execution_budget)
        },
        "Iterator Helper result Get produced a structured result",
    )
}

const fn iterator_helper_stage_closes_outer(stage: IteratorHelperNextStage) -> bool {
    matches!(
        stage,
        IteratorHelperNextStage::Callback
            | IteratorHelperNextStage::InnerIteratorMethod
            | IteratorHelperNextStage::InnerIteratorCall
            | IteratorHelperNextStage::InnerNextMethod
            | IteratorHelperNextStage::InnerNextResult
            | IteratorHelperNextStage::InnerDone
            | IteratorHelperNextStage::InnerValue
    )
}

pub(super) fn begin_iterator_helper_return(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(helper) = receiver else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator Helper object expected",
        )?);
    };
    let helper = *helper;
    let Some(snapshot) = runtime.iterator_helper_snapshot(helper)? else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator Helper object expected",
        )?);
    };
    match snapshot.lifecycle {
        crate::object::IteratorHelperLifecycle::Completed => {
            return iterator_result(runtime, realm, StoredValue::Undefined, true);
        }
        crate::object::IteratorHelperLifecycle::Executing => {
            return Err(iterator_exception(
                realm,
                origin,
                ExceptionKind::TypeError,
                "Iterator Helper is already running",
            )?);
        }
        crate::object::IteratorHelperLifecycle::SuspendedStart
        | crate::object::IteratorHelperLifecycle::SuspendedYield => {}
    }
    if matches!(snapshot.kind, crate::object::IteratorHelperKind::Zip) {
        let iterators = runtime.iterator_zip_open_iterators(helper)?;
        let suspended_start = matches!(
            snapshot.lifecycle,
            crate::object::IteratorHelperLifecycle::SuspendedStart
        );
        runtime.set_iterator_helper_lifecycle(
            helper,
            if suspended_start {
                crate::object::IteratorHelperLifecycle::Completed
            } else {
                crate::object::IteratorHelperLifecycle::Executing
            },
        )?;
        return begin_iterator_zip_close(
            runtime,
            IteratorZipCloseContinuation {
                helper: (!suspended_start).then_some(helper),
                iterators,
                current: None,
                pending: None,
                terminal: IteratorZipCloseTerminal::ReturnDone,
                realm,
                stage: IteratorZipCloseStage::ReturnProperty,
                origin,
            },
            return_to,
            execution_budget,
        );
    }
    let is_concat = matches!(snapshot.kind, crate::object::IteratorHelperKind::Concat);
    let (iterator, outer_iterator) = match (snapshot.inner_iterator, snapshot.inner_next_method) {
        (Some(inner_iterator), Some(_)) => (inner_iterator, Some(snapshot.iterator)),
        (None, None) if is_concat => {
            complete_iterator_helper(runtime, helper)?;
            return iterator_result(runtime, realm, StoredValue::Undefined, true);
        }
        (None, None) => (snapshot.iterator, None),
        (Some(_), None) | (None, Some(_)) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Iterator Helper retained an incomplete inner iterator record",
            }
            .into());
        }
    };
    runtime
        .set_iterator_helper_lifecycle(helper, crate::object::IteratorHelperLifecycle::Executing)?;
    let state = IteratorHelperReturnContinuation {
        helper,
        iterator,
        outer_iterator: if is_concat { None } else { outer_iterator },
        realm,
        stage: IteratorHelperReturnStage::ReturnProperty,
        origin,
    };
    read_iterator_helper_return(runtime, state, return_to, execution_budget)
}

fn read_iterator_helper_return(
    runtime: &mut Runtime,
    state: IteratorHelperReturnContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_iterator_property_lookup(runtime, &state.iterator, execution_budget)?;
    let dispatch = match begin_value_get(
        runtime,
        &state.iterator,
        runtime.predefined_property_key(PredefinedAtom::Return),
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending)) => {
            return fail_iterator_helper_return(
                runtime,
                state,
                pending,
                return_to,
                execution_budget,
            );
        }
        Err(error) => return Err(error),
    };
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::IteratorHelperReturn,
        |state, value| {
            advance_iterator_helper_return(runtime, state, &value, return_to, execution_budget)
        },
        "Iterator Helper return Get produced a structured result",
    )
}

pub(super) fn advance_iterator_helper_return(
    runtime: &mut Runtime,
    mut state: IteratorHelperReturnContinuation,
    completion: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IteratorHelperReturnStage::ReturnProperty => match completion {
            StoredValue::Undefined | StoredValue::Null => {
                continue_iterator_helper_outer_return(runtime, state, return_to, execution_budget)
            }
            StoredValue::Function(function) => {
                execution_budget.charge_instructions(1)?;
                let receiver = state.iterator.duplicate();
                state.stage = IteratorHelperReturnStage::ReturnCall;
                let origin = state.origin.clone();
                iterator_method_call(
                    *function,
                    receiver,
                    NativeContinuation::IteratorHelperReturn(state),
                    return_to,
                    origin,
                )
            }
            StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => fail_iterator_helper_return_with_type_error(
                runtime,
                state,
                "iterator return method is not callable",
                return_to,
                execution_budget,
            ),
        },
        IteratorHelperReturnStage::ReturnCall => {
            if completion.heap_reference().is_none() {
                return fail_iterator_helper_return_with_type_error(
                    runtime,
                    state,
                    "iterator return method did not return an object",
                    return_to,
                    execution_budget,
                );
            }
            continue_iterator_helper_outer_return(runtime, state, return_to, execution_budget)
        }
    }
}

fn continue_iterator_helper_outer_return(
    runtime: &mut Runtime,
    mut state: IteratorHelperReturnContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(outer_iterator) = state.outer_iterator.take() else {
        complete_iterator_helper(runtime, state.helper)?;
        return iterator_result(runtime, state.realm, StoredValue::Undefined, true);
    };
    state.iterator = outer_iterator;
    state.stage = IteratorHelperReturnStage::ReturnProperty;
    read_iterator_helper_return(runtime, state, return_to, execution_budget)
}

fn fail_iterator_helper_return_with_type_error(
    runtime: &mut Runtime,
    state: IteratorHelperReturnContinuation,
    message: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let NativeFailure::Abrupt(pending) = iterator_exception(
        state.realm,
        state.origin.clone(),
        ExceptionKind::TypeError,
        message,
    )?
    else {
        unreachable!("iterator_exception always returns an abrupt completion")
    };
    fail_iterator_helper_return(runtime, state, pending, return_to, execution_budget)
}

fn fail_iterator_helper_return(
    runtime: &mut Runtime,
    state: IteratorHelperReturnContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(outer_iterator) = state.outer_iterator else {
        complete_iterator_helper(runtime, state.helper)?;
        return Err(NativeFailure::Abrupt(pending));
    };
    complete_iterator_helper(runtime, state.helper)?;
    begin_exceptional_iterator_close(
        runtime,
        outer_iterator,
        pending,
        return_to,
        execution_budget,
    )
}

fn complete_iterator_helper(runtime: &mut Runtime, helper: ObjectId) -> Result<(), EngineFault> {
    runtime.set_iterator_helper_lifecycle(helper, crate::object::IteratorHelperLifecycle::Completed)
}

fn iterator_counter_number(counter: u64) -> JsNumber {
    let high = u32::try_from(counter >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(counter & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    JsNumber::from_f64(f64::from(high) * 4_294_967_296.0 + f64::from(low))
}

#[allow(
    clippy::too_many_lines,
    reason = "the shared consumer exposes every IteratorStepValue and normal-close suspension boundary as one typed state machine"
)]
pub(super) fn advance_iterator_consumer(
    runtime: &mut Runtime,
    mut state: IteratorConsumerContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IteratorConsumerStage::NextMethod => {
            let StoredValue::Function(next_method) = completion else {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "iterator next method is not callable",
                )?);
            };
            state.next_method = Some(next_method);
            call_iterator_consumer_next(state, return_to, execution_budget)
        }
        IteratorConsumerStage::NextResult => {
            if completion.heap_reference().is_none() {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "iterator next method did not return an object",
                )?);
            }
            state.result = Some(completion);
            state.stage = IteratorConsumerStage::Done;
            read_iterator_consumer_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        IteratorConsumerStage::Done => {
            if runtime.to_boolean(&completion)? {
                return finish_iterator_consumer_exhausted(state);
            }
            state.stage = IteratorConsumerStage::Value;
            read_iterator_consumer_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        IteratorConsumerStage::Value => {
            if matches!(state.kind, crate::runtime::IteratorConsumer::Reduce)
                && state.accumulator.is_none()
            {
                state.accumulator = Some(completion);
                state.counter = 1;
                state.result = None;
                return call_iterator_consumer_next(state, return_to, execution_budget);
            }
            let argument_count = if matches!(state.kind, crate::runtime::IteratorConsumer::Reduce) {
                3
            } else {
                2
            };
            let mut arguments = Vec::new();
            arguments.try_reserve_exact(argument_count).map_err(|_| {
                ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: argument_count,
                }
            })?;
            if matches!(state.kind, crate::runtime::IteratorConsumer::Find) {
                state.candidate = Some(completion.duplicate());
            }
            if matches!(state.kind, crate::runtime::IteratorConsumer::Reduce) {
                arguments.push(
                    state
                        .accumulator
                        .take()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "Iterator.prototype.reduce lost its accumulator",
                        })?,
                );
            }
            arguments.push(completion);
            arguments.push(StoredValue::Number(iterator_counter_number(state.counter)));
            state.result = None;
            state.stage = IteratorConsumerStage::Callback;
            execution_budget.charge_instructions(1)?;
            let origin = state.origin.clone();
            iterator_call_with_arguments(
                state.callback,
                StoredValue::Undefined,
                arguments,
                NativeContinuation::IteratorConsumer(state),
                return_to,
                origin,
            )
        }
        IteratorConsumerStage::Callback => finish_iterator_consumer_callback(
            runtime,
            state,
            completion,
            return_to,
            execution_budget,
        ),
        IteratorConsumerStage::CloseReturnProperty => match completion {
            StoredValue::Undefined | StoredValue::Null => finish_iterator_consumer_close(state),
            StoredValue::Function(return_method) => {
                state.stage = IteratorConsumerStage::CloseReturnCall;
                execution_budget.charge_instructions(1)?;
                let receiver = state.iterator.duplicate();
                let origin = state.origin.clone();
                iterator_method_call(
                    return_method,
                    receiver,
                    NativeContinuation::IteratorConsumer(state),
                    return_to,
                    origin,
                )
            }
            StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => Err(iterator_exception(
                state.realm,
                state.origin,
                ExceptionKind::TypeError,
                "iterator return method is not callable",
            )?),
        },
        IteratorConsumerStage::CloseReturnCall => {
            if completion.heap_reference().is_none() {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "iterator return method did not return an object",
                )?);
            }
            finish_iterator_consumer_close(state)
        }
    }
}

fn finish_iterator_consumer_callback(
    runtime: &mut Runtime,
    mut state: IteratorConsumerContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.kind, crate::runtime::IteratorConsumer::Reduce) {
        state.accumulator = Some(completion);
        state.counter = state.counter.saturating_add(1);
        return call_iterator_consumer_next(state, return_to, execution_budget);
    }
    let truthy = runtime.to_boolean(&completion)?;
    let exits = match state.kind {
        crate::runtime::IteratorConsumer::Every => !truthy,
        crate::runtime::IteratorConsumer::Find | crate::runtime::IteratorConsumer::Some => truthy,
        crate::runtime::IteratorConsumer::ForEach => false,
        crate::runtime::IteratorConsumer::Reduce => unreachable!(
            "Iterator.prototype.reduce returns before predicate-style consumer handling"
        ),
    };
    if exits {
        let outcome = match state.kind {
            crate::runtime::IteratorConsumer::Every => StoredValue::Boolean(false),
            crate::runtime::IteratorConsumer::Some => StoredValue::Boolean(true),
            crate::runtime::IteratorConsumer::Find => {
                state
                    .candidate
                    .take()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Iterator.prototype.find lost its candidate value",
                    })?
            }
            crate::runtime::IteratorConsumer::ForEach => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Iterator.prototype.forEach cannot exit from a callback result",
                }
                .into());
            }
            crate::runtime::IteratorConsumer::Reduce => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Iterator.prototype.reduce cannot exit through a predicate result",
                }
                .into());
            }
        };
        state.outcome = Some(outcome);
        state.candidate = None;
        state.stage = IteratorConsumerStage::CloseReturnProperty;
        return read_iterator_consumer_property(
            runtime,
            state,
            runtime.predefined_property_key(PredefinedAtom::Return),
            return_to,
            execution_budget,
        );
    }
    state.candidate = None;
    state.counter = state.counter.saturating_add(1);
    call_iterator_consumer_next(state, return_to, execution_budget)
}

fn finish_iterator_consumer_exhausted(
    mut state: IteratorConsumerContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    let value = match state.kind {
        crate::runtime::IteratorConsumer::Every => StoredValue::Boolean(true),
        crate::runtime::IteratorConsumer::Some => StoredValue::Boolean(false),
        crate::runtime::IteratorConsumer::Find | crate::runtime::IteratorConsumer::ForEach => {
            StoredValue::Undefined
        }
        crate::runtime::IteratorConsumer::Reduce => {
            let Some(accumulator) = state.accumulator.take() else {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "Iterator.prototype.reduce cannot reduce an empty iterator without an initial value",
                )?);
            };
            accumulator
        }
    };
    Ok(NativeDispatch::Immediate(value))
}

fn finish_iterator_consumer_close(
    mut state: IteratorConsumerContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    Ok(NativeDispatch::Immediate(state.outcome.take().ok_or(
        EngineFault::RuntimeInvariant {
            message: "Iterator consumer close lost its normal completion",
        },
    )?))
}

fn call_iterator_consumer_next(
    mut state: IteratorConsumerContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let next_method = state.next_method.ok_or(EngineFault::RuntimeInvariant {
        message: "Iterator consumer has no callable next method",
    })?;
    state.result = None;
    state.stage = IteratorConsumerStage::NextResult;
    execution_budget.charge_instructions(1)?;
    let receiver = state.iterator.duplicate();
    let origin = state.origin.clone();
    iterator_method_call(
        next_method,
        receiver,
        NativeContinuation::IteratorConsumer(state),
        return_to,
        origin,
    )
}

fn read_iterator_consumer_property(
    runtime: &mut Runtime,
    state: IteratorConsumerContinuation,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = match state.stage {
        IteratorConsumerStage::NextMethod | IteratorConsumerStage::CloseReturnProperty => {
            &state.iterator
        }
        IteratorConsumerStage::Done | IteratorConsumerStage::Value => {
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "Iterator consumer result lookup has no result object",
            })?
        }
        IteratorConsumerStage::NextResult
        | IteratorConsumerStage::Callback
        | IteratorConsumerStage::CloseReturnCall => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Iterator consumer call stage attempted a property lookup",
            }
            .into());
        }
    };
    charge_iterator_property_lookup(runtime, base, execution_budget)?;
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
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::IteratorConsumer,
        |state, value| {
            advance_iterator_consumer(runtime, state, value, return_to, execution_budget)
        },
        "Iterator consumer property Get produced a structured result",
    )
}

pub(super) fn resume_iterator_consumer_abrupt(
    runtime: &mut Runtime,
    state: IteratorConsumerContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if !state.handles_abrupt() {
        return Err(EngineFault::RuntimeInvariant {
            message: "Iterator consumer handled an abrupt completion outside its callback",
        }
        .into());
    }
    begin_exceptional_iterator_close(
        runtime,
        state.iterator,
        pending,
        return_to,
        execution_budget,
    )
}

pub(super) fn advance_iterator_dispose(
    state: IteratorDisposeContinuation,
    completion: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IteratorDisposeStage::ReturnProperty => match completion {
            StoredValue::Undefined | StoredValue::Null => {
                Ok(NativeDispatch::Immediate(StoredValue::Undefined))
            }
            StoredValue::Function(return_method) => {
                execution_budget.charge_instructions(1)?;
                let receiver = state.iterator.duplicate();
                let origin = state.origin.clone();
                let mut state = state;
                state.stage = IteratorDisposeStage::ReturnCall;
                iterator_method_call(
                    *return_method,
                    receiver,
                    NativeContinuation::IteratorDispose(state),
                    return_to,
                    origin,
                )
            }
            StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => Err(iterator_exception(
                state.realm,
                state.origin,
                ExceptionKind::TypeError,
                "iterator return method is not callable",
            )?),
        },
        IteratorDisposeStage::ReturnCall => Ok(NativeDispatch::Immediate(StoredValue::Undefined)),
    }
}

fn read_iterator_dispose_return(
    runtime: &mut Runtime,
    state: IteratorDisposeContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if !matches!(state.stage, IteratorDisposeStage::ReturnProperty) {
        return Err(EngineFault::RuntimeInvariant {
            message: "Iterator.prototype[Symbol.dispose] attempted a property lookup after its return call",
        }
        .into());
    }
    charge_iterator_property_lookup(runtime, &state.iterator, execution_budget)?;
    let name = JsString::from_utf8("return")?;
    let dispatch = begin_value_get(
        runtime,
        &state.iterator,
        runtime.predefined_property_key(PredefinedAtom::Return),
        Some(&name),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::IteratorDispose,
        |state, value| advance_iterator_dispose(state, &value, return_to, execution_budget),
        "Iterator.prototype[Symbol.dispose] return Get produced a structured result",
    )
}

pub(super) fn advance_iterator_to_array(
    runtime: &mut Runtime,
    mut state: IteratorToArrayContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IteratorToArrayStage::NextMethod => {
            let StoredValue::Function(next_method) = completion else {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "iterator next method is not callable",
                )?);
            };
            state.next_method = Some(next_method);
            call_iterator_to_array_next(state, return_to, execution_budget)
        }
        IteratorToArrayStage::NextResult => {
            if completion.heap_reference().is_none() {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "iterator next method did not return an object",
                )?);
            }
            state.result = Some(completion);
            state.stage = IteratorToArrayStage::Done;
            read_iterator_to_array_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        IteratorToArrayStage::Done => {
            if runtime.to_boolean(&completion)? {
                return Ok(NativeDispatch::Immediate(StoredValue::Object(
                    runtime.allocate_array(state.realm, state.items)?,
                )));
            }
            state.stage = IteratorToArrayStage::Value;
            read_iterator_to_array_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        IteratorToArrayStage::Value => {
            state
                .items
                .try_reserve(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: 1,
                })?;
            state.items.push(completion);
            state.result = None;
            call_iterator_to_array_next(state, return_to, execution_budget)
        }
    }
}

fn read_iterator_to_array_property(
    runtime: &mut Runtime,
    state: IteratorToArrayContinuation,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = match state.stage {
        IteratorToArrayStage::NextMethod => &state.iterator,
        IteratorToArrayStage::Done | IteratorToArrayStage::Value => {
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "Iterator.prototype.toArray result lookup has no result object",
            })?
        }
        IteratorToArrayStage::NextResult => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Iterator.prototype.toArray call stage attempted a property read",
            }
            .into());
        }
    };
    charge_iterator_property_lookup(runtime, base, execution_budget)?;
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
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::IteratorToArray,
        |state, value| {
            advance_iterator_to_array(runtime, state, value, return_to, execution_budget)
        },
        "Iterator.prototype.toArray Get produced a structured result",
    )
}

fn call_iterator_to_array_next(
    mut state: IteratorToArrayContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget.charge_instructions(1)?;
    let next_method = state.next_method.ok_or(EngineFault::RuntimeInvariant {
        message: "Iterator.prototype.toArray has no retained next method",
    })?;
    let receiver = state.iterator.duplicate();
    state.stage = IteratorToArrayStage::NextResult;
    let origin = state.origin.clone();
    iterator_method_call(
        next_method,
        receiver,
        NativeContinuation::IteratorToArray(state),
        return_to,
        origin,
    )
}

fn read_iterator_from_property(
    runtime: &mut Runtime,
    state: IteratorFromContinuation,
    key: PropertyKey,
    property_name: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = match state.stage {
        IteratorFromStage::IteratorMethod => &state.input,
        IteratorFromStage::NextMethod => {
            state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Iterator.from next lookup has no iterator",
                })?
        }
        IteratorFromStage::Iterator | IteratorFromStage::PrototypeWalk => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Iterator.from attempted a property read in the wrong stage",
            }
            .into());
        }
    };
    charge_iterator_property_lookup(runtime, base, execution_budget)?;
    let name = JsString::from_utf8(property_name)?;
    let dispatch = begin_value_get(
        runtime,
        base,
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
        NativeContinuation::IteratorFrom,
        |state, value| advance_iterator_from(runtime, state, value, return_to, execution_budget),
        "Iterator.from Get produced a structured result",
    )
}

pub(super) fn iterator_getter_call(
    function: FunctionId,
    receiver: StoredValue,
    continuation: NativeContinuation,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    pre_call: Option<NativePreCall>,
) -> Result<NativeDispatch, NativeFailure> {
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(continuation);
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::empty(),
        return_to,
        origin,
        continuations,
        pre_call,
        new_target: None,
        native_caller: None,
    }))
}

fn iterator_method_call(
    function: FunctionId,
    receiver: StoredValue,
    continuation: NativeContinuation,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    iterator_getter_call(function, receiver, continuation, return_to, origin, None)
}

fn iterator_call_with_arguments(
    function: FunctionId,
    receiver: StoredValue,
    arguments: Vec<StoredValue>,
    continuation: NativeContinuation,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(continuation);
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

fn iterator_terminal_call(
    function: FunctionId,
    receiver: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> NativeDispatch {
    NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::empty(),
        return_to,
        origin,
        continuations: Vec::new(),
        pre_call: None,
        new_target: None,
        native_caller: None,
    })
}

pub(super) fn iterator_result(
    runtime: &mut Runtime,
    realm: RealmId,
    value: StoredValue,
    done: bool,
) -> Result<NativeDispatch, NativeFailure> {
    Ok(NativeDispatch::Immediate(StoredValue::Object(
        runtime.allocate_iterator_result(realm, value, done)?,
    )))
}

pub(super) fn begin_array_iterator_method(
    runtime: &mut Runtime,
    receiver: StoredValue,
    kind: crate::object::ArrayIteratorKind,
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(receiver, StoredValue::Undefined | StoredValue::Null) {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "cannot convert to object",
        )?);
    }
    let primitive_wrapper = matches!(
        receiver,
        StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
    );
    let additional_objects = 1_usize.saturating_add(usize::from(primitive_wrapper));
    check_execution_limit(
        RuntimeResource::HeapObjects,
        runtime.limits.max_heap_objects,
        usize_to_u64(runtime.objects.len()).saturating_add(usize_to_u64(additional_objects)),
    )?;
    if matches!(receiver, StoredValue::String(_)) {
        check_execution_limit(
            RuntimeResource::ObjectProperties,
            runtime.limits.max_object_properties,
            runtime.object_properties.saturating_add(1),
        )?;
    }
    runtime
        .objects
        .try_reserve(additional_objects)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapObjects,
            additional: additional_objects,
        })?;
    let collection_pending = runtime.collection_pending;
    let mut temporary_wrapper = None;
    let receiver = match receiver {
        StoredValue::Undefined | StoredValue::Null => unreachable!("nullish receiver was rejected"),
        StoredValue::Boolean(value) => {
            let wrapper = runtime.allocate_boxed_boolean(realm, value)?;
            temporary_wrapper = Some(wrapper);
            StoredValue::Object(wrapper)
        }
        StoredValue::Number(value) => {
            let wrapper = runtime.allocate_boxed_number(realm, value)?;
            temporary_wrapper = Some(wrapper);
            StoredValue::Object(wrapper)
        }
        StoredValue::BigInt(value) => {
            let wrapper = runtime.allocate_boxed_bigint(realm, value)?;
            temporary_wrapper = Some(wrapper);
            StoredValue::Object(wrapper)
        }
        StoredValue::String(value) => {
            let wrapper = runtime.allocate_boxed_string(realm, value)?;
            temporary_wrapper = Some(wrapper);
            StoredValue::Object(wrapper)
        }
        StoredValue::Symbol(value) => {
            let wrapper = runtime.allocate_boxed_symbol(realm, value)?;
            temporary_wrapper = Some(wrapper);
            StoredValue::Object(wrapper)
        }
        value @ (StoredValue::Function(_) | StoredValue::Object(_)) => value,
    };
    match runtime.allocate_array_iterator(realm, receiver, kind) {
        Ok(iterator) => Ok(NativeDispatch::Immediate(StoredValue::Object(iterator))),
        Err(error) => {
            if let Some(wrapper) = temporary_wrapper
                && let Some(object) = runtime.objects.remove(wrapper)
            {
                runtime.object_properties = runtime
                    .object_properties
                    .saturating_sub(usize_to_u64(object.record.property_count()));
            }
            runtime.collection_pending = collection_pending;
            Err(error.into())
        }
    }
}

pub(super) fn begin_string_iterator_method(
    runtime: &mut Runtime,
    receiver: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match receiver {
        StoredValue::Undefined | StoredValue::Null => Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "cannot convert to object",
        )?),
        StoredValue::String(string) => Ok(NativeDispatch::Immediate(StoredValue::Object(
            runtime.allocate_string_iterator(realm, string)?,
        ))),
        value => begin_operator_primitive_conversion(
            runtime,
            value,
            OperatorPrimitiveHint::String,
            OperatorPrimitiveTarget::StringIteratorIntrinsic,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the call boundary transfers ownership of the receiver into receiver validation"
)]
pub(super) fn begin_array_iterator_next(
    runtime: &mut Runtime,
    receiver: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(iterator) = receiver else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Array Iterator object expected",
        )?);
    };
    if runtime
        .objects
        .get(iterator)
        .and_then(crate::object::HeapObject::array_iterator_state)
        .is_none()
    {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Array Iterator object expected",
        )?);
    }
    let snapshot = runtime.array_iterator_snapshot(iterator)?;
    let Some(iterated) = snapshot.iterated else {
        return iterator_result(runtime, realm, StoredValue::Undefined, true);
    };
    let state = ArrayIteratorNextContinuation {
        iterator,
        iterated,
        kind: snapshot.kind,
        index: snapshot.next,
        realm,
        stage: ArrayIteratorNextStage::AwaitLength,
        prepared_result: None,
        origin,
    };
    let key = runtime.predefined_property_key(PredefinedAtom::Length);
    charge_iterator_property_lookup(runtime, &state.iterated, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        &state.iterated,
        key,
        None,
        realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::ArrayIteratorNext,
        |state, value| {
            begin_array_iterator_length_conversion(
                runtime,
                state,
                value,
                return_to,
                execution_budget,
            )
        },
        "Array iterator length Get produced a structured result",
    )
}

pub(super) fn advance_array_iterator_next(
    runtime: &mut Runtime,
    state: ArrayIteratorNextContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ArrayIteratorNextStage::AwaitLength => begin_array_iterator_length_conversion(
            runtime,
            state,
            completion,
            return_to,
            execution_budget,
        ),
        ArrayIteratorNextStage::AwaitValue => {
            finish_array_iterator_value(runtime, state, completion)
        }
    }
}

fn begin_array_iterator_length_conversion(
    runtime: &mut Runtime,
    state: ArrayIteratorNextContinuation,
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
        OperatorPrimitiveTarget::ArrayIteratorLength(state),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "Array iterator index advancement, prepared-result admission, and Proxy-aware element Get remain one failure-atomic step"
)]
pub(super) fn finish_array_iterator_length(
    runtime: &mut Runtime,
    mut state: ArrayIteratorNextContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let number = operator_to_number(value, state.realm, &state.origin)?;
    let length = number_to_uint32(number);
    let live = runtime.array_iterator_snapshot(state.iterator)?;
    state.iterated = live.iterated.unwrap_or(StoredValue::Undefined);
    state.kind = live.kind;
    state.index = live.next;
    if state.index >= length {
        let prepared = runtime.prepare_iterator_result_allocation(state.realm, None)?;
        let result =
            runtime.commit_prepared_iterator_result(prepared, StoredValue::Undefined, true)?;
        runtime.finish_array_iterator(state.iterator)?;
        return Ok(NativeDispatch::Immediate(StoredValue::Object(result)));
    }

    let index = state.index;
    state.prepared_result = Some(
        runtime.prepare_iterator_result_allocation(
            state.realm,
            matches!(state.kind, crate::object::ArrayIteratorKind::KeyAndValue)
                .then_some(StoredValue::Number(JsNumber::from_u32(index))),
        )?,
    );
    if matches!(state.kind, crate::object::ArrayIteratorKind::Key) {
        let prepared = state
            .prepared_result
            .take()
            .expect("Array iterator result preparation was just installed");
        let result = runtime.commit_prepared_iterator_result(
            prepared,
            StoredValue::Number(JsNumber::from_u32(index)),
            false,
        )?;
        runtime.advance_array_iterator(state.iterator)?;
        return Ok(NativeDispatch::Immediate(StoredValue::Object(result)));
    }
    let Some(index) = ArrayIndex::new(index) else {
        let prepared = state
            .prepared_result
            .take()
            .expect("Array iterator result preparation was just installed");
        let result =
            runtime.commit_prepared_iterator_result(prepared, StoredValue::Undefined, true)?;
        runtime.finish_array_iterator(state.iterator)?;
        return Ok(NativeDispatch::Immediate(StoredValue::Object(result)));
    };
    let key = PropertyKey::from_index(index);
    charge_iterator_property_lookup(runtime, &state.iterated, execution_budget)?;
    state.stage = ArrayIteratorNextStage::AwaitValue;
    state
        .prepared_result
        .as_mut()
        .expect("Array iterator result preparation was just installed")
        .mark_callback_boundary();
    let iterator = state.iterator;
    let dispatch = match begin_value_get(
        runtime,
        &state.iterated,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(error @ (NativeFailure::Abrupt(_) | NativeFailure::AbruptAfterTransient(_))) => {
            runtime.advance_array_iterator(iterator)?;
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    match dispatch {
        NativeDispatch::Immediate(value) => {
            runtime.advance_array_iterator(iterator)?;
            finish_array_iterator_value(runtime, state, value)
        }
        NativeDispatch::Call(mut call) => {
            debug_assert!(call.pre_call.is_none());
            call.pre_call = Some(NativePreCall::AdvanceArrayIterator(iterator));
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::ArrayIteratorNext(state)],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            runtime.advance_array_iterator(iterator)?;
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::ArrayIteratorNext(state)],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Array iterator value Get produced a structured result",
        }
        .into()),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the completed continuation is consumed at this terminal boundary"
)]
fn finish_array_iterator_value(
    runtime: &mut Runtime,
    mut state: ArrayIteratorNextContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(prepared) = state.prepared_result.take() else {
        return Err(EngineFault::RuntimeInvariant {
            message: "Array iterator value completion has no prepared result allocation",
        }
        .into());
    };
    let result = runtime.commit_prepared_iterator_result(prepared, value, false)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the call boundary transfers ownership of the receiver into receiver validation"
)]
pub(super) fn begin_string_iterator_next(
    runtime: &mut Runtime,
    receiver: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(iterator) = receiver else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "String Iterator object expected",
        )?);
    };
    if runtime
        .objects
        .get(iterator)
        .and_then(crate::object::HeapObject::string_iterator_state)
        .is_none()
    {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "String Iterator object expected",
        )?);
    }
    let prepared = runtime.prepare_iterator_result_allocation(realm, None)?;
    let (value, done) = match runtime.string_iterator_next(iterator)? {
        Some(value) => (StoredValue::String(value), false),
        None => (StoredValue::Undefined, true),
    };
    let result = runtime.commit_prepared_iterator_result(prepared, value, done)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
}

pub(super) fn begin_iterator_wrapper_next(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(wrapper) = receiver else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator wrapper expected",
        )?);
    };
    let Some(record) = runtime.iterator_wrapper_record(*wrapper)? else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator wrapper expected",
        )?);
    };
    let StoredValue::Function(next) = record.next_method() else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "iterator next method is not callable",
        )?);
    };
    execution_budget.charge_instructions(1)?;
    Ok(iterator_terminal_call(
        *next,
        record.iterator().duplicate(),
        return_to,
        origin,
    ))
}

pub(super) fn begin_iterator_wrapper_return(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(wrapper) = receiver else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator wrapper expected",
        )?);
    };
    let Some(record) = runtime.iterator_wrapper_record(*wrapper)? else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator wrapper expected",
        )?);
    };
    let iterator = record.iterator().duplicate();
    charge_iterator_property_lookup(runtime, &iterator, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::Return);
    let name = JsString::from_utf8("return")?;
    let state = IteratorWrapperReturnContinuation {
        iterator,
        realm,
        origin,
    };
    let dispatch = begin_value_get(
        runtime,
        &state.iterator,
        key,
        Some(&name),
        realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::IteratorWrapperReturn,
        |state, value| {
            advance_iterator_wrapper_return(runtime, state, &value, return_to, execution_budget)
        },
        "Iterator wrapper return Get produced a structured result",
    )
}

pub(super) fn advance_iterator_wrapper_return(
    runtime: &mut Runtime,
    state: IteratorWrapperReturnContinuation,
    completion: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match completion {
        StoredValue::Undefined | StoredValue::Null => {
            iterator_result(runtime, state.realm, StoredValue::Undefined, true)
        }
        StoredValue::Function(function) => {
            execution_budget.charge_instructions(1)?;
            Ok(iterator_terminal_call(
                *function,
                state.iterator,
                return_to,
                state.origin,
            ))
        }
        StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Object(_) => Err(iterator_exception(
            state.realm,
            state.origin,
            ExceptionKind::TypeError,
            "iterator return method is not callable",
        )?),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the intrinsic setter retains its descriptor key, receiver, value, realm, and caller continuation explicitly"
)]
pub(super) fn begin_iterator_prototype_setter(
    runtime: &mut Runtime,
    receiver: StoredValue,
    value: StoredValue,
    key: PropertyKey,
    name: JsString,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(reference) = receiver.heap_reference() else {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Iterator prototype setter receiver must be an object",
        )?);
    };
    if reference == HeapReference::Object(runtime.realm_iterator_prototype(realm)?) {
        return Err(iterator_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "cannot replace an intrinsic Iterator prototype property",
        )?);
    }
    execution_budget.charge_instructions(1)?;
    let state = IteratorPrototypeSetterContinuation {
        receiver,
        value,
        key: key.clone(),
        name,
        reference,
        realm,
        stage: IteratorPrototypeSetterStage::OwnDescriptor,
        origin,
    };
    let dispatch = begin_internal_get_own_property(
        runtime,
        reference,
        key,
        realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::IteratorPrototypeSetter,
        |state, value| {
            advance_iterator_prototype_setter(runtime, state, &value, return_to, execution_budget)
        },
        "Iterator prototype setter [[GetOwnProperty]] produced a structured result",
    )
}

pub(super) fn advance_iterator_prototype_setter(
    runtime: &mut Runtime,
    mut state: IteratorPrototypeSetterContinuation,
    completion: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IteratorPrototypeSetterStage::Complete => {
            Ok(NativeDispatch::Immediate(StoredValue::Undefined))
        }
        IteratorPrototypeSetterStage::OwnDescriptor => {
            state.stage = IteratorPrototypeSetterStage::Complete;
            let dispatch = if matches!(completion, StoredValue::Undefined) {
                let definition = PropertyDefinition::data(
                    Requested::Present(state.value.duplicate()),
                    Requested::Present(true),
                )
                .with_enumerable(Requested::Present(true))
                .with_configurable(Requested::Present(true));
                begin_internal_define_own_property(
                    runtime,
                    state.reference,
                    state.key.clone(),
                    definition,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                    DefinePropertyResult::Target,
                )?
            } else {
                begin_internal_set(
                    runtime,
                    state.reference,
                    state.key.clone(),
                    state.name.clone(),
                    state.value.duplicate(),
                    state.receiver.duplicate(),
                    true,
                    false,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?
            };
            continue_get_after(
                dispatch,
                state,
                NativeContinuation::IteratorPrototypeSetter,
                |_state, _value| Ok(NativeDispatch::Immediate(StoredValue::Undefined)),
                "Iterator prototype setter internal write produced a structured result",
            )
        }
    }
}

pub(super) fn begin_for_of_start(
    runtime: &mut Runtime,
    iterable: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = ForOfStartContinuation {
        iterable,
        iterator: None,
        async_from_sync: false,
        realm,
        stage: ForOfStartStage::IteratorMethod,
        origin,
    };
    read_for_of_start_property(
        runtime,
        state,
        &runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        return_to,
        execution_budget,
    )
}

pub(super) fn begin_for_await_of_start(
    runtime: &mut Runtime,
    iterable: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = ForOfStartContinuation {
        iterable,
        iterator: None,
        async_from_sync: false,
        realm,
        stage: ForOfStartStage::AsyncIteratorMethod,
        origin,
    };
    read_for_of_start_property(
        runtime,
        state,
        &runtime.predefined_symbol_property_key(PredefinedAtom::SymbolAsyncIterator),
        return_to,
        execution_budget,
    )
}

pub(super) fn advance_for_of_start(
    runtime: &mut Runtime,
    mut state: ForOfStartContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ForOfStartStage::AsyncIteratorMethod
            if matches!(completion, StoredValue::Undefined | StoredValue::Null) =>
        {
            state.async_from_sync = true;
            state.stage = ForOfStartStage::IteratorMethod;
            read_for_of_start_property(
                runtime,
                state,
                &runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
                return_to,
                execution_budget,
            )
        }
        ForOfStartStage::IteratorMethod | ForOfStartStage::AsyncIteratorMethod => {
            let StoredValue::Function(method) = completion else {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "value is not iterable",
                )?);
            };
            let receiver = state.iterable.duplicate();
            state.stage = ForOfStartStage::Iterator;
            let origin = state.origin.clone();
            iterator_method_call(
                method,
                receiver,
                NativeContinuation::ForOfStart(state),
                return_to,
                origin,
            )
        }
        ForOfStartStage::Iterator => {
            if !matches!(
                completion,
                StoredValue::Function(_) | StoredValue::Object(_)
            ) {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "not an object",
                )?);
            }
            state.iterator = Some(completion);
            state.stage = ForOfStartStage::NextMethod;
            read_for_of_start_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        ForOfStartStage::NextMethod => {
            let iterator = state.iterator.ok_or(EngineFault::RuntimeInvariant {
                message: "for-of next lookup completed without an iterator",
            })?;
            if state.async_from_sync {
                let wrapper =
                    runtime.allocate_async_from_sync_iterator(state.realm, iterator, completion)?;
                return Ok(NativeDispatch::ForOfRecord {
                    iterator: StoredValue::Object(wrapper),
                    next: StoredValue::Function(
                        runtime.realm_async_from_sync_iterator_next(state.realm)?,
                    ),
                });
            }
            Ok(NativeDispatch::ForOfRecord {
                iterator,
                next: completion,
            })
        }
    }
}

fn read_for_of_start_property(
    runtime: &mut Runtime,
    state: ForOfStartContinuation,
    key: &PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (base, property_name) = match state.stage {
        ForOfStartStage::IteratorMethod => (&state.iterable, "Symbol.iterator"),
        ForOfStartStage::AsyncIteratorMethod => (&state.iterable, "Symbol.asyncIterator"),
        ForOfStartStage::NextMethod => {
            let iterator = state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "for-of next lookup has no iterator",
                })?;
            (iterator, "next")
        }
        ForOfStartStage::Iterator => {
            return Err(EngineFault::RuntimeInvariant {
                message: "for-of iterator call stage attempted a property read",
            }
            .into());
        }
    };
    charge_iterator_property_lookup(runtime, base, execution_budget)?;
    let property_name = JsString::from_utf8(property_name)?;
    let dispatch = begin_value_get(
        runtime,
        base,
        key.clone(),
        Some(&property_name),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::ForOfStart,
        |state, value| advance_for_of_start(runtime, state, value, return_to, execution_budget),
        "for-of start Get produced a structured result",
    )
}

pub(super) fn begin_for_of_next(
    iterator: StoredValue,
    next: StoredValue,
    realm: RealmId,
    offset: u8,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let function = match &next {
        StoredValue::Function(function) => *function,
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_)
        | StoredValue::Object(_) => {
            return Err(iterator_exception(
                realm,
                origin,
                ExceptionKind::TypeError,
                "not a function",
            )?);
        }
    };
    execution_budget.charge_instructions(1)?;
    let receiver = iterator.duplicate();
    let state = ForOfNextContinuation {
        iterator,
        next,
        result: None,
        realm,
        stage: ForOfNextStage::Result,
        offset,
        origin: origin.clone(),
    };
    iterator_method_call(
        function,
        receiver,
        NativeContinuation::ForOfNext(state),
        return_to,
        origin,
    )
}

pub(super) fn advance_for_of_next(
    runtime: &mut Runtime,
    mut state: ForOfNextContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ForOfNextStage::Result => {
            if !matches!(
                completion,
                StoredValue::Function(_) | StoredValue::Object(_)
            ) {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "iterator must return an object",
                )?);
            }
            state.result = Some(completion);
            state.stage = ForOfNextStage::Done;
            read_for_of_next_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        ForOfNextStage::Done => {
            if runtime.to_boolean(&completion)? {
                return Ok(NativeDispatch::ForOfStep {
                    value: StoredValue::Undefined,
                    done: true,
                    offset: state.offset,
                });
            }
            state.stage = ForOfNextStage::Value;
            read_for_of_next_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        ForOfNextStage::Value => Ok(NativeDispatch::ForOfStep {
            value: completion,
            done: false,
            offset: state.offset,
        }),
    }
}

fn read_for_of_next_property(
    runtime: &mut Runtime,
    state: ForOfNextContinuation,
    key: &PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let result = state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "for-of result property lookup has no result object",
    })?;
    charge_iterator_property_lookup(runtime, result, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        result,
        key.clone(),
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::ForOfNext,
        |state, value| advance_for_of_next(runtime, state, value, return_to, execution_budget),
        "for-of result Get produced a structured result",
    )
}

pub(super) fn begin_for_of_close(
    runtime: &mut Runtime,
    iterator: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(iterator, StoredValue::Undefined) {
        return Ok(NativeDispatch::ForOfClosed);
    }
    let state = ForOfCloseContinuation {
        iterator,
        realm,
        stage: ForOfCloseStage::AwaitReturnProperty,
        origin,
    };
    read_for_of_return(runtime, state, return_to, execution_budget)
}

fn read_for_of_return(
    runtime: &mut Runtime,
    state: ForOfCloseContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let key = runtime.predefined_property_key(PredefinedAtom::Return);
    charge_iterator_property_lookup(runtime, &state.iterator, execution_budget)?;
    let dispatch = begin_value_get(
        runtime,
        &state.iterator,
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
        NativeContinuation::ForOfClose,
        |state, value| advance_for_of_close(state, &value, return_to),
        "for-of return Get produced a structured result",
    )
}

pub(super) fn advance_for_of_close(
    mut state: ForOfCloseContinuation,
    completion: &StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ForOfCloseStage::AwaitReturnProperty => match completion {
            StoredValue::Undefined | StoredValue::Null => Ok(NativeDispatch::ForOfClosed),
            StoredValue::Function(function) => {
                let receiver = state.iterator.duplicate();
                state.stage = ForOfCloseStage::AwaitReturnCall;
                let origin = state.origin.clone();
                iterator_method_call(
                    *function,
                    receiver,
                    NativeContinuation::ForOfClose(state),
                    return_to,
                    origin,
                )
            }
            StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => Err(iterator_exception(
                state.realm,
                state.origin,
                ExceptionKind::TypeError,
                "not a function",
            )?),
        },
        ForOfCloseStage::AwaitReturnCall => {
            if matches!(
                completion,
                StoredValue::Function(_) | StoredValue::Object(_)
            ) {
                Ok(NativeDispatch::ForOfClosed)
            } else {
                Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "not an object",
                )?)
            }
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Append starts with explicit destination, cursor, realm, provenance, and execution authority"
)]
pub(super) fn begin_iterator_append(
    runtime: &mut Runtime,
    array: ObjectId,
    next_index: u32,
    iterable: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = IteratorAppendContinuation {
        array,
        next_index,
        iterable,
        iterator: None,
        next_acquired: false,
        next_method: None,
        result: None,
        realm,
        stage: IteratorAppendStage::AwaitProbe,
        origin,
    };
    read_append_property(
        runtime,
        state,
        runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "the ordered iterator protocol is one explicit resumable state machine"
)]
pub(super) fn advance_iterator_append(
    runtime: &mut Runtime,
    mut state: IteratorAppendContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IteratorAppendStage::AwaitProbe => {
            state.stage = IteratorAppendStage::AwaitMethod;
            read_append_property(
                runtime,
                state,
                runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
                return_to,
                execution_budget,
            )
        }
        IteratorAppendStage::AwaitMethod => {
            let StoredValue::Function(method) = completion else {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "value is not iterable",
                )?);
            };
            let receiver = state.iterable.duplicate();
            state.stage = IteratorAppendStage::AwaitIterator;
            let origin = state.origin.clone();
            iterator_method_call(
                method,
                receiver,
                NativeContinuation::IteratorAppend(state),
                return_to,
                origin,
            )
        }
        IteratorAppendStage::AwaitIterator => {
            if !matches!(
                completion,
                StoredValue::Function(_) | StoredValue::Object(_)
            ) {
                return Err(iterator_exception(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "not an object",
                )?);
            }
            state.iterator = Some(completion);
            state.stage = IteratorAppendStage::AwaitNextMethod;
            read_append_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        IteratorAppendStage::AwaitNextMethod => {
            state.next_acquired = true;
            let StoredValue::Function(next) = completion else {
                let pending = iterator_exception(
                    state.realm,
                    state.origin.clone(),
                    ExceptionKind::TypeError,
                    "not a function",
                )?;
                let NativeFailure::Abrupt(pending) = pending else {
                    unreachable!("iterator_exception always returns an abrupt completion")
                };
                return begin_iterator_close(runtime, state, pending, return_to, execution_budget);
            };
            state.next_method = Some(next);
            call_append_next(state, return_to, execution_budget)
        }
        IteratorAppendStage::AwaitNextResult => {
            if !matches!(
                completion,
                StoredValue::Function(_) | StoredValue::Object(_)
            ) {
                let pending = iterator_exception(
                    state.realm,
                    state.origin.clone(),
                    ExceptionKind::TypeError,
                    "iterator must return an object",
                )?;
                let NativeFailure::Abrupt(pending) = pending else {
                    unreachable!("iterator_exception always returns an abrupt completion")
                };
                return begin_iterator_close(runtime, state, pending, return_to, execution_budget);
            }
            state.result = Some(completion);
            state.stage = IteratorAppendStage::AwaitDone;
            read_append_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        IteratorAppendStage::AwaitDone => {
            if runtime.to_boolean(&completion)? {
                return Ok(NativeDispatch::Pair(
                    StoredValue::Object(state.array),
                    StoredValue::Number(JsNumber::from_u32(state.next_index)),
                ));
            }
            state.stage = IteratorAppendStage::AwaitValue;
            read_append_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        IteratorAppendStage::AwaitValue => {
            let Some(index) = ArrayIndex::new(state.next_index) else {
                let pending = iterator_exception(
                    state.realm,
                    state.origin.clone(),
                    ExceptionKind::RangeError,
                    "invalid array length",
                )?;
                let NativeFailure::Abrupt(pending) = pending else {
                    unreachable!("iterator_exception always returns an abrupt completion")
                };
                return begin_iterator_close(runtime, state, pending, return_to, execution_budget);
            };
            let key = PropertyKey::from_index(index);
            let work = runtime.preview_array_data_property_work(state.array, &key)?;
            execution_budget.charge_instructions(work)?;
            match runtime.define_array_data_property(
                state.array,
                key,
                PropertyLayout::data(true, true, true),
                completion,
            )? {
                ArrayDefineOutcome::Complete => {}
                ArrayDefineOutcome::ReadOnlyLength | ArrayDefineOutcome::NonExtensible => {
                    let pending = iterator_exception(
                        state.realm,
                        state.origin.clone(),
                        ExceptionKind::TypeError,
                        "cannot append iterator value",
                    )?;
                    let NativeFailure::Abrupt(pending) = pending else {
                        unreachable!("iterator_exception always returns an abrupt completion")
                    };
                    return begin_iterator_close(
                        runtime,
                        state,
                        pending,
                        return_to,
                        execution_budget,
                    );
                }
            }
            state.next_index = state.next_index.saturating_add(1);
            state.result = None;
            call_append_next(state, return_to, execution_budget)
        }
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "property-key ownership remains local to one resumable Get boundary"
)]
fn read_append_property(
    runtime: &mut Runtime,
    state: IteratorAppendContinuation,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = match state.stage {
        IteratorAppendStage::AwaitProbe | IteratorAppendStage::AwaitMethod => &state.iterable,
        IteratorAppendStage::AwaitNextMethod => {
            state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "iterator next lookup has no iterator",
                })?
        }
        IteratorAppendStage::AwaitDone | IteratorAppendStage::AwaitValue => {
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "iterator result lookup has no result object",
            })?
        }
        IteratorAppendStage::AwaitIterator | IteratorAppendStage::AwaitNextResult => {
            return Err(EngineFault::RuntimeInvariant {
                message: "iterator call stage attempted a property read",
            }
            .into());
        }
    };
    charge_iterator_property_lookup(runtime, base, execution_budget)?;
    let dispatch = match begin_value_get(
        runtime,
        base,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending) | NativeFailure::AbruptAfterTransient(pending))
            if state.next_acquired =>
        {
            return begin_iterator_close(runtime, state, pending, return_to, execution_budget);
        }
        Err(error) => return Err(error),
    };
    continue_get_after(
        dispatch,
        state,
        NativeContinuation::IteratorAppend,
        |state, value| advance_iterator_append(runtime, state, value, return_to, execution_budget),
        "iterator append Get produced a structured result",
    )
}

fn call_append_next(
    mut state: IteratorAppendContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget.charge_instructions(1)?;
    let next = state.next_method.ok_or(EngineFault::RuntimeInvariant {
        message: "iterator advance has no retained next method",
    })?;
    let receiver = state
        .iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "iterator advance has no retained iterator",
        })?
        .duplicate();
    state.stage = IteratorAppendStage::AwaitNextResult;
    let origin = state.origin.clone();
    iterator_method_call(
        next,
        receiver,
        NativeContinuation::IteratorAppend(state),
        return_to,
        origin,
    )
}

pub(super) fn resume_iterator_abrupt(
    runtime: &mut Runtime,
    continuation: NativeContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match continuation {
        NativeContinuation::IteratorConsumer(state) => {
            resume_iterator_consumer_abrupt(runtime, state, pending, return_to, execution_budget)
        }
        NativeContinuation::IteratorHelperNext(state) => {
            complete_iterator_helper(runtime, state.helper)?;
            if iterator_helper_stage_closes_outer(state.stage) {
                begin_exceptional_iterator_close(
                    runtime,
                    state.iterator,
                    pending,
                    return_to,
                    execution_budget,
                )
            } else {
                Err(NativeFailure::Abrupt(pending))
            }
        }
        NativeContinuation::IteratorHelperReturn(state) => {
            fail_iterator_helper_return(runtime, state, pending, return_to, execution_budget)
        }
        NativeContinuation::IteratorAppend(state) => {
            if state.next_acquired {
                begin_iterator_close(runtime, state, pending, return_to, execution_budget)
            } else {
                Err(NativeFailure::Abrupt(pending))
            }
        }
        NativeContinuation::IteratorClose(state) => Err(NativeFailure::Abrupt(state.original)),
        NativeContinuation::IteratorZipCreation(state) => resume_iterator_zip_creation_abrupt(
            runtime,
            *state,
            pending,
            return_to,
            execution_budget,
        ),
        NativeContinuation::IteratorZipNext(state) => {
            resume_iterator_zip_next_abrupt(runtime, state, pending, return_to, execution_budget)
        }
        NativeContinuation::IteratorZipClose(state) => {
            resume_iterator_zip_close_abrupt(runtime, *state, pending, return_to, execution_budget)
        }
        _ => Err(EngineFault::RuntimeInvariant {
            message: "non-abrupt native continuation reached iterator abrupt resumption",
        }
        .into()),
    }
}

fn begin_iterator_close(
    runtime: &mut Runtime,
    state: IteratorAppendContinuation,
    original: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let iterator = state.iterator.ok_or(EngineFault::RuntimeInvariant {
        message: "IteratorClose started before iterator acquisition",
    })?;
    begin_exceptional_iterator_close(runtime, iterator, original, return_to, execution_budget)
}

pub(super) fn begin_exceptional_iterator_close(
    runtime: &mut Runtime,
    iterator: StoredValue,
    original: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let close = IteratorCloseContinuation {
        iterator,
        original,
        stage: IteratorCloseStage::AwaitReturnProperty,
    };
    read_iterator_return(runtime, close, return_to, execution_budget)
}

fn read_iterator_return(
    runtime: &mut Runtime,
    close: IteratorCloseContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let key = runtime.predefined_property_key(PredefinedAtom::Return);
    charge_iterator_property_lookup(runtime, &close.iterator, execution_budget)?;
    let dispatch = match begin_value_get(
        runtime,
        &close.iterator,
        key,
        None,
        close.original.realm,
        return_to,
        close.original.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(_) | NativeFailure::AbruptAfterTransient(_)) => {
            return Err(NativeFailure::Abrupt(close.original));
        }
        Err(error) => return Err(error),
    };
    continue_get_after(
        dispatch,
        close,
        NativeContinuation::IteratorClose,
        |close, value| advance_iterator_close(close, value, return_to),
        "iterator close return Get produced a structured result",
    )
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the close completion is consumed at the pending-exception boundary"
)]
pub(super) fn advance_iterator_close(
    mut close: IteratorCloseContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    match close.stage {
        IteratorCloseStage::AwaitReturnProperty => {
            let StoredValue::Function(function) = completion else {
                return Err(NativeFailure::Abrupt(close.original));
            };
            let receiver = close.iterator.duplicate();
            close.stage = IteratorCloseStage::AwaitReturnCall;
            let origin = close.original.origin.clone();
            iterator_method_call(
                function,
                receiver,
                NativeContinuation::IteratorClose(close),
                return_to,
                origin,
            )
        }
        IteratorCloseStage::AwaitReturnCall => Err(NativeFailure::Abrupt(close.original)),
    }
}

pub(super) fn charge_iterator_property_lookup(
    runtime: &Runtime,
    base: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    if base.heap_reference().is_some() {
        charge_heap_property_lookup(runtime, base, execution_budget)?;
    }
    Ok(())
}
