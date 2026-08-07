/*
 * JavaScript Array.prototype copying semantics derived from QuickJS.
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

//! `Array.prototype.slice`, `concat`, `at`, `toReversed`, `toSpliced`, and
//! `with`.
//!
//! These read without mutating. All except `at` build a fresh Array, but every
//! method shares the same resumable element read because each read can enter a
//! getter. The two change-by-copy methods deliberately read through holes and
//! create an own `undefined` property, unlike `slice` and `concat`, which
//! preserve holes.
//!
//! `concat` spreads only a real Array. A plain array-like becomes a single
//! element, which the pinned oracle confirms: `[1].concat({length:2,0:"a"})` has
//! length `2` and its second element is the object itself. Nesting is not
//! flattened either, so `[1].concat([[2]])` keeps an Array at index `1`.
//!
//! Holes survive into the result. `[1,,3].slice(0)` keeps index `1` absent
//! because an absent source is skipped rather than written as `undefined`, which
//! is the same rule the mutators follow.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// Which stage of the copier a continuation resumes into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayCopierStage {
    /// Chooses the `ArraySpeciesCreate` result for `slice`.
    SelectSliceSpecies,
    /// Awaiting the source Array's `constructor` property for `slice`.
    AwaitSliceConstructor,
    /// Awaiting the source constructor's `@@species` property for `slice`.
    AwaitSliceSpecies,
    /// Awaiting `slice`'s custom species construction.
    AwaitSliceSpeciesConstruct,
    /// Chooses the `ArraySpeciesCreate` result for `concat`.
    SelectConcatSpecies,
    /// Awaiting the source Array's `constructor` property for `concat`.
    AwaitConcatConstructor,
    /// Awaiting the source constructor's `@@species` property for `concat`.
    AwaitConcatSpecies,
    /// Awaiting `concat`'s custom species construction.
    AwaitConcatSpeciesConstruct,
    /// Ready to evaluate `IsConcatSpreadable` for the current concat source.
    CheckSpreadability,
    /// Awaiting the current source's `@@isConcatSpreadable` value.
    AwaitSpreadability,
    /// Awaiting the current source's `length` read.
    AwaitLength,
    /// Awaiting `ToLength` of the length value.
    AwaitLengthConversion,
    /// Awaiting `ToIntegerOrInfinity` of `slice`'s, `at`'s, or `with`'s index.
    AwaitStart,
    /// Awaiting `ToIntegerOrInfinity` of `slice`'s end argument.
    AwaitEnd,
    /// Ready to read the next source element.
    NextElement,
    /// Awaiting `HasProperty` for a hole-preserving source element.
    AwaitPresence,
    /// Ready to append the next `toSpliced` insertion argument.
    NextInsertion,
    /// Awaiting an element read that may have entered a getter.
    AwaitElement,
    /// Ready to advance `concat` to its next source.
    NextSource,
    /// Finished.
    Done,
}

/// One in-progress `Array.prototype` copying method.
pub(crate) struct ArrayCopierContinuation {
    copier: ArrayCopier,
    /// The receiver, which is also `concat`'s first source.
    target: StoredValue,
    /// `concat`'s remaining sources, or `slice`/`at`'s arguments.
    arguments: Vec<StoredValue>,
    /// The source currently being read.
    source: StoredValue,
    /// The index of the next `concat` source to take from `arguments`.
    next_source: usize,
    /// Whether the current source is spread element-by-element.
    spreading: bool,
    /// The current source's length.
    length: u64,
    /// The next index to read from the current source.
    next: u64,
    /// The exclusive end of the current source's range.
    end: u64,
    /// The destination object, absent for `at` and until its method has
    /// validated the result length. `slice` and `concat` can use an arbitrary
    /// object produced by their `ArraySpeciesCreate` constructor.
    destination: Option<StoredValue>,
    /// The next index to write in the destination.
    written: u64,
    /// `at`'s answer.
    result: StoredValue,
    /// The validated replacement index for `with` or start for `toSpliced`.
    selected: Option<u64>,
    /// The validated source span skipped by `toSpliced`.
    skipped: u64,
    /// Whether `toSpliced` has appended its insertion arguments.
    inserted: bool,
    realm: RealmId,
    stage: ArrayCopierStage,
    origin: JsStackFrame,
}

impl ArrayCopierContinuation {
    /// The receiver, the current source, the result, and each argument.
    pub(crate) fn retained_values(&self) -> u64 {
        3_u64.saturating_add(usize_to_u64(self.arguments.len()))
    }

    /// Reports the traced roots this continuation retains.
    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        trace_stored_value_root(&self.source, mark);
        trace_stored_value_root(&self.result, mark);
        if let Some(destination) = &self.destination {
            trace_stored_value_root(destination, mark);
        }
        for argument in &self.arguments {
            trace_stored_value_root(argument, mark);
        }
    }
}

/// Starts one `Array.prototype` copying method.
#[expect(
    clippy::too_many_arguments,
    reason = "one shared entry point carries the method identity alongside the same receiver, arguments, and resumption context every native dispatch takes"
)]
pub(super) fn begin_array_copier(
    runtime: &mut Runtime,
    copier: ArrayCopier,
    realm: RealmId,
    receiver: StoredValue,
    arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    // Each generic copier begins with `ToObject(this)`, including `concat`'s
    // first source. The wrapper—not the primitive—is what later property
    // reads and `@@isConcatSpreadable` observe.
    let receiver = match to_object_value(runtime, realm, receiver, origin.clone())? {
        Ok(receiver) => receiver,
        Err(exception) => return Err(NativeFailure::Abrupt(exception)),
    };
    let mut collected = Vec::new();
    for value in arguments.into_remaining_iter() {
        collected
            .try_reserve(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::Frames,
                additional: 1,
            })?;
        collected.push(value);
    }
    // `slice` performs `ArraySpeciesCreate` after resolving the requested
    // range, but before any source index observation. `concat` selects its
    // `ArraySpeciesCreate` result before observing its first
    // `@@isConcatSpreadable` property.
    // `toReversed` must read `length` first, and `with` must additionally
    // convert and validate its index before ArrayCreate, so they allocate in
    // their later specification stages.
    let destination = None;
    // Every non-concat copier reads its receiver's elements. Concat first
    // performs the observable `@@isConcatSpreadable` Get and then falls back
    // to Proxy-aware IsArray.
    let spreading = !matches!(copier, ArrayCopier::Concat);
    let initial_stage = if matches!(copier, ArrayCopier::Concat) {
        ArrayCopierStage::SelectConcatSpecies
    } else {
        ArrayCopierStage::AwaitLength
    };
    let state = ArrayCopierContinuation {
        copier,
        target: receiver.duplicate(),
        arguments: collected,
        source: receiver,
        next_source: 0,
        spreading,
        length: 0,
        next: 0,
        end: 0,
        destination,
        written: 0,
        result: StoredValue::Undefined,
        selected: None,
        skipped: 0,
        inserted: false,
        realm,
        stage: initial_stage,
        origin,
    };
    advance_array_copier(runtime, state, None, return_to, execution_budget)
}

/// Resumes a copying method after an awaited read or conversion.
#[allow(
    clippy::too_many_lines,
    clippy::needless_continue,
    reason = "the length, range, insertion, element, and source-advance stages form one traced continuation shared by all six copying methods"
)]
pub(super) fn advance_array_copier(
    runtime: &mut Runtime,
    mut state: ArrayCopierContinuation,
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
            ArrayCopierStage::SelectSliceSpecies => {
                if !proxy_aware_is_array(
                    runtime,
                    state.target.duplicate(),
                    state.realm,
                    state.origin.clone(),
                )? {
                    let length = state.end.saturating_sub(state.next);
                    allocate_array_create_destination(runtime, &mut state, length)?;
                    state.stage = ArrayCopierStage::NextElement;
                    continue;
                }
                let key = runtime.predefined_property_key(PredefinedAtom::Constructor);
                charge_copier_lookup(runtime, &state.target, execution_budget)?;
                state.stage = ArrayCopierStage::AwaitSliceConstructor;
                let dispatch = begin_value_get(
                    runtime,
                    &state.target,
                    key,
                    None,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                await_get!(continue_get_state_after(
                    dispatch,
                    state,
                    array_copier_continuation,
                    "Array slice constructor Get produced a structured result",
                ));
            }
            ArrayCopierStage::AwaitSliceConstructor => {
                let constructor = take_completion(&mut completion)?;
                if let StoredValue::Function(function) = constructor
                    && function_is_constructor(runtime, function)?
                {
                    let constructor_realm = runtime.function_realm(function)?;
                    if constructor_realm != state.realm
                        && function == runtime.realm_array_constructor(constructor_realm)?
                    {
                        let length = state.end.saturating_sub(state.next);
                        allocate_array_create_destination(runtime, &mut state, length)?;
                        state.stage = ArrayCopierStage::NextElement;
                        continue;
                    }
                }
                if matches!(
                    constructor,
                    StoredValue::Function(_) | StoredValue::Object(_)
                ) {
                    let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolSpecies);
                    charge_copier_lookup(runtime, &constructor, execution_budget)?;
                    state.stage = ArrayCopierStage::AwaitSliceSpecies;
                    let dispatch = begin_value_get(
                        runtime,
                        &constructor,
                        key,
                        None,
                        state.realm,
                        return_to,
                        state.origin.clone(),
                        execution_budget,
                    )?;
                    await_get!(continue_get_state_after(
                        dispatch,
                        state,
                        array_copier_continuation,
                        "Array slice species Get produced a structured result",
                    ));
                } else if matches!(constructor, StoredValue::Undefined) {
                    let length = state.end.saturating_sub(state.next);
                    allocate_array_create_destination(runtime, &mut state, length)?;
                    state.stage = ArrayCopierStage::NextElement;
                } else {
                    return copier_type_error(&state, "not a constructor");
                }
            }
            ArrayCopierStage::AwaitSliceSpecies => {
                let species = take_completion(&mut completion)?;
                if matches!(species, StoredValue::Undefined | StoredValue::Null) {
                    let length = state.end.saturating_sub(state.next);
                    allocate_array_create_destination(runtime, &mut state, length)?;
                    state.stage = ArrayCopierStage::NextElement;
                    continue;
                }
                let StoredValue::Function(constructor) = species else {
                    return copier_type_error(&state, "not a constructor");
                };
                if !function_is_constructor(runtime, constructor)? {
                    return copier_type_error(&state, "not a constructor");
                }
                state.stage = ArrayCopierStage::AwaitSliceSpeciesConstruct;
                let length = state.end.saturating_sub(state.next);
                return suspend_construct_copier(
                    state,
                    constructor,
                    StoredValue::Number(JsNumber::from_f64(length_as_f64(length))),
                    return_to,
                );
            }
            ArrayCopierStage::AwaitSliceSpeciesConstruct => {
                let destination = take_completion(&mut completion)?;
                if destination.heap_reference().is_none() {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "ArraySpeciesCreate constructor returned a primitive",
                    }
                    .into());
                }
                state.destination = Some(destination);
                state.stage = ArrayCopierStage::NextElement;
            }
            ArrayCopierStage::SelectConcatSpecies => {
                if !proxy_aware_is_array(
                    runtime,
                    state.target.duplicate(),
                    state.realm,
                    state.origin.clone(),
                )? {
                    allocate_concat_destination(runtime, &mut state)?;
                    state.stage = ArrayCopierStage::CheckSpreadability;
                    continue;
                }
                let key = runtime.predefined_property_key(PredefinedAtom::Constructor);
                charge_copier_lookup(runtime, &state.target, execution_budget)?;
                state.stage = ArrayCopierStage::AwaitConcatConstructor;
                let dispatch = begin_value_get(
                    runtime,
                    &state.target,
                    key,
                    None,
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                await_get!(continue_get_state_after(
                    dispatch,
                    state,
                    array_copier_continuation,
                    "Array concat constructor Get produced a structured result",
                ));
            }
            ArrayCopierStage::AwaitConcatConstructor => {
                let constructor = take_completion(&mut completion)?;
                if let StoredValue::Function(function) = constructor
                    && function_is_constructor(runtime, function)?
                {
                    let constructor_realm = runtime.function_realm(function)?;
                    if constructor_realm != state.realm
                        && function == runtime.realm_array_constructor(constructor_realm)?
                    {
                        allocate_concat_destination(runtime, &mut state)?;
                        state.stage = ArrayCopierStage::CheckSpreadability;
                        continue;
                    }
                }
                if matches!(
                    constructor,
                    StoredValue::Function(_) | StoredValue::Object(_)
                ) {
                    let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolSpecies);
                    charge_copier_lookup(runtime, &constructor, execution_budget)?;
                    state.stage = ArrayCopierStage::AwaitConcatSpecies;
                    let dispatch = begin_value_get(
                        runtime,
                        &constructor,
                        key,
                        None,
                        state.realm,
                        return_to,
                        state.origin.clone(),
                        execution_budget,
                    )?;
                    await_get!(continue_get_state_after(
                        dispatch,
                        state,
                        array_copier_continuation,
                        "Array concat species Get produced a structured result",
                    ));
                } else if matches!(constructor, StoredValue::Undefined) {
                    allocate_concat_destination(runtime, &mut state)?;
                    state.stage = ArrayCopierStage::CheckSpreadability;
                } else {
                    return copier_type_error(&state, "not a constructor");
                }
            }
            ArrayCopierStage::AwaitConcatSpecies => {
                let species = take_completion(&mut completion)?;
                if matches!(species, StoredValue::Undefined | StoredValue::Null) {
                    allocate_concat_destination(runtime, &mut state)?;
                    state.stage = ArrayCopierStage::CheckSpreadability;
                    continue;
                }
                let StoredValue::Function(constructor) = species else {
                    return copier_type_error(&state, "not a constructor");
                };
                if !function_is_constructor(runtime, constructor)? {
                    return copier_type_error(&state, "not a constructor");
                }
                state.stage = ArrayCopierStage::AwaitConcatSpeciesConstruct;
                return suspend_construct_copier(
                    state,
                    constructor,
                    StoredValue::Number(JsNumber::from_i32(0)),
                    return_to,
                );
            }
            ArrayCopierStage::AwaitConcatSpeciesConstruct => {
                let destination = take_completion(&mut completion)?;
                if destination.heap_reference().is_none() {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "ArraySpeciesCreate constructor returned a primitive",
                    }
                    .into());
                }
                state.destination = Some(destination);
                state.stage = ArrayCopierStage::CheckSpreadability;
            }
            ArrayCopierStage::CheckSpreadability => {
                if state.source.heap_reference().is_none() {
                    state.spreading = false;
                    state.stage = ArrayCopierStage::AwaitLength;
                    continue;
                }
                let key = runtime
                    .predefined_symbol_property_key(PredefinedAtom::SymbolIsConcatSpreadable);
                charge_copier_lookup(runtime, &state.source, execution_budget)?;
                state.stage = ArrayCopierStage::AwaitSpreadability;
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
                await_get!(continue_get_state_after(
                    dispatch,
                    state,
                    array_copier_continuation,
                    "concat spreadability Get produced a structured result",
                ));
            }
            ArrayCopierStage::AwaitSpreadability => {
                let spreadable = take_completion(&mut completion)?;
                state.spreading = if matches!(spreadable, StoredValue::Undefined) {
                    proxy_aware_is_array(
                        runtime,
                        state.source.duplicate(),
                        state.realm,
                        state.origin.clone(),
                    )?
                } else {
                    spreadable.is_truthy()
                };
                state.stage = ArrayCopierStage::AwaitLength;
            }
            ArrayCopierStage::AwaitLength => {
                // A `concat` source that is not a real Array contributes itself
                // as one element, so it never has its length read.
                if !state.spreading {
                    let element = state.source.duplicate();
                    append_element(runtime, &mut state, element, execution_budget)?;
                    state.stage = ArrayCopierStage::NextSource;
                    continue;
                }
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                charge_copier_lookup(runtime, &state.source, execution_budget)?;
                state.stage = ArrayCopierStage::AwaitLengthConversion;
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
                await_get!(continue_get_state_after(
                    dispatch,
                    state,
                    array_copier_continuation,
                    "array copier length Get produced a structured result",
                ));
            }
            ArrayCopierStage::AwaitLengthConversion => {
                let value = take_completion(&mut completion)?;
                if needs_conversion(&value) {
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::Number,
                        OperatorPrimitiveTarget::ArrayCopierArgument(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                let number = operator_to_number(value, state.realm, &state.origin)?;
                state.length = number_to_length(number);
                match state.copier {
                    // `concat` always takes a whole source.
                    ArrayCopier::Concat => {
                        // `concat` checks the complete spread length before
                        // it probes any indexed property. Without this guard a
                        // length of `2^53 - 1` would enter an impossible loop
                        // even though the required result must throw first.
                        if state.length > MAX_SAFE_INTEGER.saturating_sub(state.written) {
                            return copier_type_error(&state, "invalid array length");
                        }
                        state.next = 0;
                        state.end = state.length;
                        state.stage = ArrayCopierStage::NextElement;
                    }
                    ArrayCopier::Slice
                    | ArrayCopier::At
                    | ArrayCopier::ToSpliced
                    | ArrayCopier::With => {
                        state.stage = ArrayCopierStage::AwaitStart;
                    }
                    ArrayCopier::ToReversed => {
                        let length = state.length;
                        allocate_array_create_destination(runtime, &mut state, length)?;
                        state.next = 0;
                        state.end = state.length;
                        state.stage = ArrayCopierStage::NextElement;
                    }
                }
            }
            ArrayCopierStage::AwaitStart => {
                if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    let integer = number_to_integer_or_infinity(number);
                    apply_start(runtime, &mut state, integer)?;
                    continue;
                }
                match state.arguments.first() {
                    Some(value) if needs_conversion(value) => {
                        let value = value.duplicate();
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::Number,
                            OperatorPrimitiveTarget::ArrayCopierArgument(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    Some(value) => completion = Some(value.duplicate()),
                    // An absent start begins at zero.
                    None => apply_start(runtime, &mut state, 0.0)?,
                }
            }
            ArrayCopierStage::AwaitEnd => {
                if matches!(state.copier, ArrayCopier::ToSpliced) {
                    if let Some(value) = completion.take() {
                        let number = operator_to_number(value, state.realm, &state.origin)?;
                        let integer = number_to_integer_or_infinity(number);
                        let start = state.selected.ok_or(EngineFault::RuntimeInvariant {
                            message: "toSpliced lost its validated start index",
                        })?;
                        let skipped = clamp_splice_skip(integer, state.length - start);
                        prepare_to_spliced(runtime, &mut state, skipped)?;
                        continue;
                    }
                    match state.arguments.get(1) {
                        None => {
                            let start = state.selected.ok_or(EngineFault::RuntimeInvariant {
                                message: "toSpliced lost its validated start index",
                            })?;
                            let skipped = if state.arguments.is_empty() {
                                0
                            } else {
                                state.length - start
                            };
                            prepare_to_spliced(runtime, &mut state, skipped)?;
                            continue;
                        }
                        Some(value) if needs_conversion(value) => {
                            let value = value.duplicate();
                            let realm = state.realm;
                            let origin = state.origin.clone();
                            return begin_operator_primitive_conversion(
                                runtime,
                                value,
                                OperatorPrimitiveHint::Number,
                                OperatorPrimitiveTarget::ArrayCopierArgument(Box::new(state)),
                                realm,
                                return_to,
                                origin,
                                execution_budget,
                            );
                        }
                        Some(value) => completion = Some(value.duplicate()),
                    }
                    continue;
                }
                if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    state.end = relative_bound(number_to_integer_or_infinity(number), state.length);
                    state.stage = if matches!(state.copier, ArrayCopier::Slice) {
                        ArrayCopierStage::SelectSliceSpecies
                    } else {
                        ArrayCopierStage::NextElement
                    };
                    continue;
                }
                match state.arguments.get(1) {
                    // An explicit `undefined` end is the same as an absent one,
                    // so it runs to the length rather than converting to `0`.
                    Some(StoredValue::Undefined) | None => {
                        state.end = state.length;
                        state.stage = if matches!(state.copier, ArrayCopier::Slice) {
                            ArrayCopierStage::SelectSliceSpecies
                        } else {
                            ArrayCopierStage::NextElement
                        };
                    }
                    Some(value) if needs_conversion(value) => {
                        let value = value.duplicate();
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::Number,
                            OperatorPrimitiveTarget::ArrayCopierArgument(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    Some(value) => completion = Some(value.duplicate()),
                }
            }
            ArrayCopierStage::NextElement => {
                if state.next >= state.end {
                    state.stage = if matches!(state.copier, ArrayCopier::ToSpliced) {
                        if state.inserted {
                            ArrayCopierStage::Done
                        } else {
                            ArrayCopierStage::NextInsertion
                        }
                    } else {
                        ArrayCopierStage::NextSource
                    };
                    continue;
                }
                execution_budget.charge_instructions(1)?;
                let index = state.next;
                state.next = state.next.saturating_add(1);
                if matches!(state.copier, ArrayCopier::With) && state.selected == Some(index) {
                    let replacement = state
                        .arguments
                        .get(1)
                        .map_or(StoredValue::Undefined, StoredValue::duplicate);
                    append_element(runtime, &mut state, replacement, execution_budget)?;
                    continue;
                }
                let source_index = if matches!(state.copier, ArrayCopier::ToReversed) {
                    state.length.saturating_sub(index).saturating_sub(1)
                } else {
                    index
                };
                // The older copying methods preserve holes. The change-by-copy
                // methods use Get directly and therefore materialize a missing
                // source index as an own `undefined` property.
                if !matches!(
                    state.copier,
                    ArrayCopier::ToReversed | ArrayCopier::ToSpliced | ArrayCopier::With
                ) {
                    let key = source_element_key(runtime, source_index)?;
                    charge_copier_lookup(runtime, &state.source, execution_budget)?;
                    state.stage = ArrayCopierStage::AwaitPresence;
                    let dispatch = begin_value_has(
                        runtime,
                        &state.source,
                        key,
                        state.realm,
                        return_to,
                        state.origin.clone(),
                        execution_budget,
                    )?;
                    await_get!(continue_get_state_after(
                        dispatch,
                        state,
                        array_copier_continuation,
                        "array copier HasProperty produced a structured result",
                    ));
                }
                await_get!(begin_array_copier_element_get(
                    runtime,
                    state,
                    source_index,
                    return_to,
                    execution_budget,
                ));
            }
            ArrayCopierStage::AwaitPresence => {
                if !take_completion(&mut completion)?.is_truthy() {
                    if matches!(state.copier, ArrayCopier::At) {
                        state.stage = ArrayCopierStage::Done;
                        continue;
                    }
                    state.written = state.written.saturating_add(1);
                    state.stage = ArrayCopierStage::NextElement;
                    continue;
                }
                let index = state.next.saturating_sub(1);
                let source_index = if matches!(state.copier, ArrayCopier::ToReversed) {
                    state.length.saturating_sub(index).saturating_sub(1)
                } else {
                    index
                };
                await_get!(begin_array_copier_element_get(
                    runtime,
                    state,
                    source_index,
                    return_to,
                    execution_budget,
                ));
            }
            ArrayCopierStage::AwaitElement => {
                let value = take_completion(&mut completion)?;
                if matches!(state.copier, ArrayCopier::At) {
                    state.result = value;
                    state.stage = ArrayCopierStage::Done;
                    continue;
                }
                append_element(runtime, &mut state, value, execution_budget)?;
                state.stage = ArrayCopierStage::NextElement;
            }
            ArrayCopierStage::NextInsertion => {
                if let Some(item) = state.arguments.get(state.next_source) {
                    execution_budget.charge_instructions(1)?;
                    let item = item.duplicate();
                    state.next_source = state.next_source.saturating_add(1);
                    append_element(runtime, &mut state, item, execution_budget)?;
                    continue;
                }
                let start = state.selected.ok_or(EngineFault::RuntimeInvariant {
                    message: "toSpliced lost its validated start index",
                })?;
                state.inserted = true;
                state.next = start.saturating_add(state.skipped);
                state.end = state.length;
                state.stage = ArrayCopierStage::NextElement;
            }
            ArrayCopierStage::NextSource => {
                // Only `concat` has more than one source.
                if !matches!(state.copier, ArrayCopier::Concat) {
                    state.stage = ArrayCopierStage::Done;
                    continue;
                }
                let Some(next) = state.arguments.get(state.next_source) else {
                    state.stage = ArrayCopierStage::Done;
                    continue;
                };
                let next = next.duplicate();
                state.next_source = state.next_source.saturating_add(1);
                state.source = next;
                state.stage = ArrayCopierStage::CheckSpreadability;
            }
            ArrayCopierStage::Done => {
                return Ok(NativeDispatch::Immediate(
                    match state.destination.as_ref() {
                        Some(destination) => {
                            // `ArraySpeciesCreate` initialized `slice`'s default
                            // Array with its final length, and a custom species
                            // result must not receive an extra length write.
                            // The other copying operations create their result
                            // empty or fill it directly, so they finalize the
                            // trailing-hole length here.
                            if !matches!(state.copier, ArrayCopier::Slice) {
                                finish_destination(runtime, &state, destination, execution_budget)?;
                            }
                            destination.duplicate()
                        }
                        None => state.result,
                    },
                ));
            }
        }
    }
}

/// Applies `slice`'s start, `at`'s index, or a change-by-copy relative index.
fn apply_start(
    runtime: &mut Runtime,
    state: &mut ArrayCopierContinuation,
    integer: f64,
) -> Result<(), NativeFailure> {
    match state.copier {
        ArrayCopier::At => {
            // `at` accepts a negative index counting from the end and answers
            // `undefined` outside the range.
            let length_as_f64 = length_as_f64(state.length);
            let resolved = if integer < 0.0 {
                length_as_f64 + integer
            } else {
                integer
            };
            if resolved < 0.0 || resolved >= length_as_f64 {
                state.stage = ArrayCopierStage::Done;
                return Ok(());
            }
            let index = relative_bound(resolved, state.length);
            state.next = index;
            state.end = index.saturating_add(1);
            state.stage = ArrayCopierStage::NextElement;
        }
        ArrayCopier::Slice | ArrayCopier::Concat => {
            state.next = relative_bound(integer, state.length);
            state.stage = ArrayCopierStage::AwaitEnd;
        }
        ArrayCopier::ToReversed => {
            return Err(EngineFault::RuntimeInvariant {
                message: "toReversed entered an argument-conversion stage",
            }
            .into());
        }
        ArrayCopier::ToSpliced => {
            state.selected = Some(relative_bound(integer, state.length));
            state.stage = ArrayCopierStage::AwaitEnd;
        }
        ArrayCopier::With => {
            let length = length_as_f64(state.length);
            let actual = if integer >= 0.0 {
                integer
            } else {
                length + integer
            };
            if actual < 0.0 || actual >= length {
                return Err(NativeFailure::Abrupt(copier_error(
                    state,
                    ExceptionKind::RangeError,
                    "invalid array index",
                )?));
            }
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the specification range check bounds the integral index by ToLength"
            )]
            let selected = actual as u64;
            state.selected = Some(selected);
            let length = state.length;
            allocate_array_create_destination(runtime, state, length)?;
            state.next = 0;
            state.end = state.length;
            state.stage = ArrayCopierStage::NextElement;
        }
    }
    Ok(())
}

/// Performs the `ArrayCreate(length)` used by copying methods with a validated
/// result length.
fn allocate_array_create_destination(
    runtime: &mut Runtime,
    state: &mut ArrayCopierContinuation,
    result_length: u64,
) -> Result<(), NativeFailure> {
    let Ok(length) = u32::try_from(result_length) else {
        return Err(NativeFailure::Abrupt(copier_error(
            state,
            ExceptionKind::RangeError,
            "invalid array length",
        )?));
    };
    let prototype = runtime.realm_array_prototype(state.realm)?;
    let destination =
        runtime.allocate_sparse_array_with_prototype(HeapReference::Object(prototype), length)?;
    state.destination = Some(StoredValue::Object(destination));
    Ok(())
}

/// Allocates the default `ArraySpeciesCreate` result for `concat`.
fn allocate_concat_destination(
    runtime: &mut Runtime,
    state: &mut ArrayCopierContinuation,
) -> Result<(), NativeFailure> {
    state.destination = Some(StoredValue::Object(
        runtime.allocate_array(state.realm, Vec::new())?,
    ));
    Ok(())
}

/// Finalizes `toSpliced`'s result length and initializes its three copy phases.
fn prepare_to_spliced(
    runtime: &mut Runtime,
    state: &mut ArrayCopierContinuation,
    skipped: u64,
) -> Result<(), NativeFailure> {
    let insertion_count = usize_to_u64(state.arguments.len().saturating_sub(2));
    let retained = state.length.saturating_sub(skipped);
    let result_length = retained.saturating_add(insertion_count);
    if result_length > MAX_SAFE_INTEGER {
        return Err(NativeFailure::Abrupt(copier_error(
            state,
            ExceptionKind::TypeError,
            "invalid array length",
        )?));
    }
    allocate_array_create_destination(runtime, state, result_length)?;
    state.skipped = skipped;
    state.next = 0;
    state.end = state.selected.ok_or(EngineFault::RuntimeInvariant {
        message: "toSpliced lost its validated start index",
    })?;
    state.next_source = 2;
    state.inserted = false;
    state.stage = ArrayCopierStage::NextElement;
    Ok(())
}

/// Clamps `skipCount` between zero and the remaining source length.
fn clamp_splice_skip(integer: f64, maximum: u64) -> u64 {
    if integer <= 0.0 {
        return 0;
    }
    if integer >= length_as_f64(maximum) {
        return maximum;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "ToIntegerOrInfinity plus the preceding bounds produce a u64"
    )]
    let skipped = integer as u64;
    skipped
}

/// Appends one element to the destination array.
fn append_element(
    runtime: &mut Runtime,
    state: &mut ArrayCopierContinuation,
    value: StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    let Some(destination) = state.destination.as_ref() else {
        return Err(EngineFault::RuntimeInvariant {
            message: "an array copier appended an element with no destination",
        }
        .into());
    };
    let key = element_key(state.written)?;
    state.written = state.written.saturating_add(1);
    match define_static_property(runtime, destination, key, value, execution_budget)? {
        PropertyWriteOutcome::Complete => Ok(()),
        PropertyWriteOutcome::Failed(failure) => Err(copier_property_failure(state, failure)),
        PropertyWriteOutcome::Setter { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Array copier CreateDataPropertyOrThrow attempted to call a setter",
        }
        .into()),
    }
}

/// Sets the destination's final length.
///
/// The length is written once at the end rather than per element, so a trailing
/// hole is still counted: `[1,,].slice(0)` has length `2`.
fn finish_destination(
    runtime: &mut Runtime,
    state: &ArrayCopierContinuation,
    destination: &StoredValue,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    let length = u32::try_from(state.written).map_err(|_| EngineFault::RuntimeInvariant {
        message: "an array copier produced a length outside the array-index domain",
    })?;
    if let StoredValue::Object(destination) = destination
        && runtime.array_length(*destination)?.is_some()
    {
        return match runtime.set_array_length(*destination, length)? {
            ArrayLengthWriteOutcome::Complete
            | ArrayLengthWriteOutcome::BlockedByNonConfigurable { .. } => Ok(()),
            ArrayLengthWriteOutcome::ReadOnly => Err(EngineFault::RuntimeInvariant {
                message: "a freshly allocated destination array refused its length",
            }
            .into()),
        };
    }
    let key = runtime.predefined_property_key(PredefinedAtom::Length);
    let value = StoredValue::Number(JsNumber::from_u32(length));
    match define_static_property(runtime, destination, key, value, execution_budget)? {
        PropertyWriteOutcome::Complete => Ok(()),
        PropertyWriteOutcome::Failed(failure) => Err(copier_property_failure(state, failure)),
        PropertyWriteOutcome::Setter { .. } => Err(EngineFault::RuntimeInvariant {
            message: "Array copier final length write attempted to call a setter",
        }
        .into()),
    }
}

fn array_copier_continuation(state: ArrayCopierContinuation) -> NativeContinuation {
    NativeContinuation::ArrayCopier(Box::new(state))
}

fn begin_array_copier_element_get(
    runtime: &mut Runtime,
    mut state: ArrayCopierContinuation,
    source_index: u64,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<GetContinuationDispatch<ArrayCopierContinuation>, NativeFailure> {
    let key = source_element_key(runtime, source_index)?;
    charge_copier_lookup(runtime, &state.source, execution_budget)?;
    state.stage = ArrayCopierStage::AwaitElement;
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
    continue_get_state_after(
        dispatch,
        state,
        array_copier_continuation,
        "array copier element Get produced a structured result",
    )
}

/// Builds one realm-owned exception for a change-by-copy precondition.
fn copier_error(
    state: &ArrayCopierContinuation,
    kind: ExceptionKind,
    message: &str,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm: state.realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin: state.origin.clone(),
    })
}

fn copier_property_failure(
    state: &ArrayCopierContinuation,
    failure: PropertyFailure,
) -> NativeFailure {
    match property_exception_at(state.realm, state.origin.clone(), None, failure) {
        Ok(exception) => NativeFailure::Abrupt(exception),
        Err(error) => error.into(),
    }
}

fn copier_type_error(
    state: &ArrayCopierContinuation,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    let message = JsString::from_utf8(message)?;
    Err(NativeFailure::Abrupt(PendingException {
        realm: state.realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message,
        },
        origin: state.origin.clone(),
    }))
}

/// Suspends into a species constructor that resumes the copier.
fn suspend_construct_copier(
    state: ArrayCopierContinuation,
    constructor: FunctionId,
    argument: StoredValue,
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
    continuations.push(NativeContinuation::ArrayCopier(Box::new(state)));
    Ok(NativeDispatch::Call(NativeCall {
        function: constructor,
        receiver: StoredValue::Undefined,
        arguments: CallArguments::from_values(vec![argument]),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: Some(constructor),
        native_caller: None,
    }))
}

/// Returns an integer-index key, including ordinary string keys above the
/// Array-index domain admitted by `LengthOfArrayLike`.
fn source_element_key(runtime: &mut Runtime, index: u64) -> Result<PropertyKey, NativeFailure> {
    if let Ok(index) = u32::try_from(index)
        && let Some(index) = ArrayIndex::new(index)
    {
        return Ok(PropertyKey::from_index(index));
    }
    let name = JsNumber::from_f64(length_as_f64(index)).to_javascript_string()?;
    Ok(runtime.property_key_from_string(&name)?)
}

/// Charges one property lookup, tolerating a primitive source.
fn charge_copier_lookup(
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

/// Returns whether a value needs a resumable `ToPrimitive`.
const fn needs_conversion(value: &StoredValue) -> bool {
    matches!(value, StoredValue::Function(_) | StoredValue::Object(_))
}

/// Resolves a relative endpoint against a length.
fn relative_bound(value: f64, length: u64) -> u64 {
    let length_as_f64 = length_as_f64(length);
    let resolved = if value < 0.0 {
        length_as_f64 + value
    } else {
        value
    };
    if resolved <= 0.0 {
        return 0;
    }
    if resolved >= length_as_f64 {
        return length;
    }
    // The bounds prove the value is a non-negative integer below `length`.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the preceding bounds keep the value inside the u64 domain exactly"
    )]
    let bound = resolved as u64;
    bound
}

/// Converts a length to binary64.
#[expect(
    clippy::cast_precision_loss,
    reason = "ToLength bounds every length by 2^53 - 1, which binary64 represents exactly"
)]
fn length_as_f64(length: u64) -> f64 {
    length as f64
}

/// Returns the array index for one element position.
fn element_index(index: u64) -> Result<ArrayIndex, NativeFailure> {
    let index = u32::try_from(index).map_err(|_| EngineFault::RuntimeInvariant {
        message: "array copier index exceeded the array-index domain",
    })?;
    ArrayIndex::new(index).ok_or_else(|| {
        EngineFault::RuntimeInvariant {
            message: "array copier index reached the non-index sentinel",
        }
        .into()
    })
}

/// Returns the property key for one element index.
fn element_key(index: u64) -> Result<PropertyKey, NativeFailure> {
    Ok(PropertyKey::from_index(element_index(index)?))
}

/// Extracts the awaited completion value.
fn take_completion(completion: &mut Option<StoredValue>) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        NativeFailure::Execution(
            EngineFault::RuntimeInvariant {
                message: "an array copier resumed without its awaited completion",
            }
            .into(),
        )
    })
}
