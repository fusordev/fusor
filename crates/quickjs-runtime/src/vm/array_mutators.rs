/*
 * JavaScript Array.prototype mutator semantics derived from QuickJS.
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

//! `Array.prototype.push`, `pop`, `shift`, `unshift`, `reverse`, `fill`, and
//! `copyWithin`.
//!
//! These are the mutators that move elements without a user callback. They share
//! one resumable driver because they share one skeleton: read `length` once with
//! `ToLength`, then perform a bounded sequence of element reads, writes, and
//! deletes, then write `length` back. Every one of those steps can enter an
//! accessor, so each is a suspension point.
//!
//! The pinned oracle fixes the observable order. For `push` on an array-like
//! with a `length` accessor and an index setter it logs `getlen`, `set1:x`,
//! `setlen:2`: the length is read first, each element is written in argument
//! order, and the new length is written last. For `pop` it logs `getlen`,
//! `get1`, `setlen:1`, and the element is deleted before the length shrinks.
//!
//! Holes are preserved rather than materialized. `shift` on `[,2]` leaves index
//! `0` present because the element that moved into it was itself present, while
//! `reverse` on `[1,,3]` keeps the middle absent: an absent source is deleted at
//! the destination rather than written as `undefined`.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// The largest length these mutators admit.
///
/// `ToLength` already clamps to this, and exceeding it is the one arithmetic
/// failure the mutators report rather than saturating.
const MAX_ARRAY_LENGTH: u64 = (1_u64 << 53) - 1;

/// One planned step in a mutator's element plan.
///
/// Making the three kinds explicit keeps the driver honest: overloading a
/// source/destination pair to also mean "delete" or "store an argument" hid the
/// distinction between reading an element and removing one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ElementStep {
    /// Read `from` and write it to `to`, deleting `to` when `from` is absent.
    Move { from: u64, to: u64 },
    /// Read `index` for the return value, then delete it.
    Take { index: u64 },
    /// Delete `index` without reading it.
    Drop { index: u64 },
    /// Write the argument belonging to `index`.
    Store { index: u64 },
    /// Exchange two indices, reading both before writing either.
    Swap { left: u64, right: u64 },
}

/// Which stage of the mutator a continuation resumes into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayMutatorStage {
    /// Awaiting the `length` property read.
    AwaitLength,
    /// Awaiting `ToLength` of the length value.
    AwaitLengthConversion,
    /// Awaiting the `fill` arguments' numeric conversions.
    AwaitFillStart,
    AwaitFillEnd,
    /// Awaiting the `copyWithin` arguments' numeric conversions.
    AwaitCopyWithinTo,
    AwaitCopyWithinFrom,
    AwaitCopyWithinFinal,
    /// Ready to perform the next planned step.
    NextStep,
    /// Awaiting the first element read of the current step.
    AwaitFirstRead,
    /// Awaiting the second element read, which only `Swap` performs.
    AwaitSecondRead,
    /// Awaiting the first element write of the current step.
    AwaitFirstWrite,
    /// Awaiting the second element write, which only `Swap` performs.
    AwaitSecondWrite,
    /// Awaiting the final `length` write.
    AwaitLengthWrite,
    /// Finished; `result` holds the value to return.
    Done,
}

/// One in-progress `Array.prototype` mutator.
pub(crate) struct ArrayMutatorContinuation {
    mutator: ArrayMutator,
    /// The coerced receiver being mutated.
    target: StoredValue,
    /// The arguments `push`, `unshift`, and `fill` still need.
    arguments: Vec<StoredValue>,
    /// The element count from the single `ToLength` length read.
    length: u64,
    /// The planned element steps, consumed front to back.
    moves: Vec<ElementStep>,
    /// The index of the next planned move.
    next_move: usize,
    /// The first value read for the current step, if it was present.
    first: Option<StoredValue>,
    /// Whether the first source was absent, so its destination is deleted.
    first_absent: bool,
    /// The second value read, which only `Swap` uses.
    second: Option<StoredValue>,
    /// Whether the second source was absent.
    second_absent: bool,
    /// `fill`'s resolved bounds.
    fill_start: u64,
    fill_end: u64,
    /// `copyWithin`'s resolved destination, source, and end.
    copy_to: u64,
    copy_from: u64,
    copy_final: u64,
    /// The value this mutator returns.
    result: StoredValue,
    /// The length to write back once the moves finish.
    final_length: u64,
    realm: RealmId,
    stage: ArrayMutatorStage,
    origin: JsStackFrame,
}

impl ArrayMutatorContinuation {
    /// The receiver, the result, the pending element, and each argument.
    pub(crate) fn retained_values(&self) -> u64 {
        4_u64.saturating_add(usize_to_u64(self.arguments.len()))
    }

    /// Reports the traced roots this continuation retains.
    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        trace_stored_value_root(&self.result, mark);
        for value in [self.first.as_ref(), self.second.as_ref()]
            .into_iter()
            .flatten()
        {
            trace_stored_value_root(value, mark);
        }
        for argument in &self.arguments {
            trace_stored_value_root(argument, mark);
        }
    }
}

/// Starts one `Array.prototype` mutator.
#[expect(
    clippy::too_many_arguments,
    reason = "one shared entry point carries the mutator identity alongside the same receiver, arguments, and resumption context every native dispatch takes"
)]
pub(super) fn begin_array_mutator(
    runtime: &mut Runtime,
    mutator: ArrayMutator,
    realm: RealmId,
    receiver: StoredValue,
    arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    // Every mutator begins with `ToObject(this)`, so a nullish receiver throws
    // before `length` is read.
    if matches!(receiver, StoredValue::Undefined | StoredValue::Null) {
        return Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("cannot convert to object")?,
            },
            origin,
        }));
    }
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
    let state = ArrayMutatorContinuation {
        mutator,
        target: receiver,
        arguments: collected,
        length: 0,
        moves: Vec::new(),
        next_move: 0,
        first: None,
        first_absent: false,
        second: None,
        second_absent: false,
        fill_start: 0,
        fill_end: 0,
        copy_to: 0,
        copy_from: 0,
        copy_final: 0,
        result: StoredValue::Undefined,
        final_length: 0,
        realm,
        stage: ArrayMutatorStage::AwaitLength,
        origin,
    };
    advance_array_mutator(runtime, state, None, return_to, execution_budget)
}

/// Resumes a mutator after an awaited read, write, or conversion.
#[allow(
    clippy::too_many_lines,
    reason = "the length, planning, element-move, and length-write stages form one traced continuation shared by every mutator"
)]
pub(super) fn advance_array_mutator(
    runtime: &mut Runtime,
    mut state: ArrayMutatorContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            ArrayMutatorStage::AwaitLength => {
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                charge_mutator_lookup(runtime, &state.target, execution_budget)?;
                match read_static_property(runtime, state.realm, &state.target, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        completion = Some(value);
                        state.stage = ArrayMutatorStage::AwaitLengthConversion;
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.stage = ArrayMutatorStage::AwaitLengthConversion;
                        return suspend(
                            state,
                            SuspendedCall {
                                function,
                                receiver,
                                argument: None,
                            },
                            return_to,
                        );
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(NativeFailure::Abrupt(property_exception_at(
                            state.realm,
                            state.origin.clone(),
                            None,
                            failure,
                        )?));
                    }
                }
            }
            ArrayMutatorStage::AwaitLengthConversion => {
                let value = take_completion(&mut completion)?;
                let number = operator_to_number(value, state.realm, &state.origin)?;
                state.length = number_to_length(number);
                // `fill` converts its bounds after the length and before any
                // element, which the oracle reports as `len|start|end`.
                if matches!(state.mutator, ArrayMutator::Fill) {
                    state.stage = ArrayMutatorStage::AwaitFillStart;
                    continue;
                }
                // `copyWithin` converts its destination, source, and end in the
                // same position (`quickjs.c:42989-42999`).
                if matches!(state.mutator, ArrayMutator::CopyWithin) {
                    state.stage = ArrayMutatorStage::AwaitCopyWithinTo;
                    continue;
                }
                plan_moves(&mut state)?;
                state.stage = ArrayMutatorStage::NextStep;
            }
            ArrayMutatorStage::AwaitFillStart => {
                if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    state.fill_start =
                        relative_bound(number_to_integer_or_infinity(number), state.length);
                    state.stage = ArrayMutatorStage::AwaitFillEnd;
                    continue;
                }
                // The start argument is the second one; `fill`'s value is first.
                match state.arguments.get(1) {
                    Some(value) if needs_conversion(value) => {
                        let value = value.duplicate();
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::Number,
                            OperatorPrimitiveTarget::ArrayMutatorArgument(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    Some(value) => {
                        completion = Some(value.duplicate());
                    }
                    None => {
                        // An absent start fills from the beginning.
                        state.fill_start = 0;
                        state.stage = ArrayMutatorStage::AwaitFillEnd;
                    }
                }
            }
            ArrayMutatorStage::AwaitFillEnd => {
                if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    state.fill_end =
                        relative_bound(number_to_integer_or_infinity(number), state.length);
                    plan_moves(&mut state)?;
                    state.stage = ArrayMutatorStage::NextStep;
                    continue;
                }
                match state.arguments.get(2) {
                    Some(value) if needs_conversion(value) => {
                        let value = value.duplicate();
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::Number,
                            OperatorPrimitiveTarget::ArrayMutatorArgument(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    Some(value) => {
                        completion = Some(value.duplicate());
                    }
                    None => {
                        // An absent end fills to the length.
                        state.fill_end = state.length;
                        plan_moves(&mut state)?;
                        state.stage = ArrayMutatorStage::NextStep;
                    }
                }
            }
            ArrayMutatorStage::AwaitCopyWithinTo => {
                if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    state.copy_to =
                        relative_bound(number_to_integer_or_infinity(number), state.length);
                    state.stage = ArrayMutatorStage::AwaitCopyWithinFrom;
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
                            OperatorPrimitiveTarget::ArrayMutatorArgument(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    Some(value) => completion = Some(value.duplicate()),
                    // An absent destination copies to the beginning.
                    None => {
                        state.copy_to = 0;
                        state.stage = ArrayMutatorStage::AwaitCopyWithinFrom;
                    }
                }
            }
            ArrayMutatorStage::AwaitCopyWithinFrom => {
                if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    state.copy_from =
                        relative_bound(number_to_integer_or_infinity(number), state.length);
                    state.stage = ArrayMutatorStage::AwaitCopyWithinFinal;
                    continue;
                }
                match state.arguments.get(1) {
                    Some(value) if needs_conversion(value) => {
                        let value = value.duplicate();
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::Number,
                            OperatorPrimitiveTarget::ArrayMutatorArgument(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    Some(value) => completion = Some(value.duplicate()),
                    // An absent source copies from the beginning.
                    None => {
                        state.copy_from = 0;
                        state.stage = ArrayMutatorStage::AwaitCopyWithinFinal;
                    }
                }
            }
            ArrayMutatorStage::AwaitCopyWithinFinal => {
                if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    state.copy_final =
                        relative_bound(number_to_integer_or_infinity(number), state.length);
                    plan_moves(&mut state)?;
                    state.stage = ArrayMutatorStage::NextStep;
                    continue;
                }
                match state.arguments.get(2) {
                    // An explicit `undefined` end is the same as an absent one,
                    // so the copy runs to the length rather than converting to
                    // `0` (`quickjs.c:42995-42999`).
                    Some(StoredValue::Undefined) | None => {
                        state.copy_final = state.length;
                        plan_moves(&mut state)?;
                        state.stage = ArrayMutatorStage::NextStep;
                    }
                    Some(value) if needs_conversion(value) => {
                        let value = value.duplicate();
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::Number,
                            OperatorPrimitiveTarget::ArrayMutatorArgument(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    Some(value) => completion = Some(value.duplicate()),
                }
            }
            ArrayMutatorStage::NextStep => {
                let Some(step) = state.moves.get(state.next_move).copied() else {
                    state.stage = ArrayMutatorStage::AwaitLengthWrite;
                    continue;
                };
                execution_budget.charge_instructions(1)?;
                state.first = None;
                state.first_absent = false;
                state.second = None;
                state.second_absent = false;
                match step {
                    // A store needs no read at all.
                    ElementStep::Store { index } => {
                        state.first = Some(argument_for(&state, index));
                        state.stage = ArrayMutatorStage::AwaitFirstWrite;
                    }
                    // A drop deletes without reading.
                    ElementStep::Drop { .. } => {
                        state.first_absent = true;
                        state.stage = ArrayMutatorStage::AwaitFirstWrite;
                    }
                    ElementStep::Take { index } | ElementStep::Move { from: index, .. } => {
                        match read_element(runtime, &mut state, index, execution_budget)? {
                            ElementRead::Absent => {
                                state.first_absent = true;
                                state.stage = ArrayMutatorStage::AwaitFirstWrite;
                            }
                            ElementRead::Value(value) => {
                                completion = Some(value);
                                state.stage = ArrayMutatorStage::AwaitFirstRead;
                            }
                            ElementRead::Suspend(call) => {
                                state.stage = ArrayMutatorStage::AwaitFirstRead;
                                return suspend(state, call, return_to);
                            }
                        }
                    }
                    ElementStep::Swap { left, .. } => {
                        match read_element(runtime, &mut state, left, execution_budget)? {
                            ElementRead::Absent => {
                                state.first_absent = true;
                                state.stage = ArrayMutatorStage::AwaitSecondRead;
                            }
                            ElementRead::Value(value) => {
                                completion = Some(value);
                                state.stage = ArrayMutatorStage::AwaitFirstRead;
                            }
                            ElementRead::Suspend(call) => {
                                state.stage = ArrayMutatorStage::AwaitFirstRead;
                                return suspend(state, call, return_to);
                            }
                        }
                    }
                }
            }
            ArrayMutatorStage::AwaitFirstRead => {
                let value = take_completion(&mut completion)?;
                let step = current_step(&state)?;
                // `pop` and `shift` return the element they remove.
                if matches!(step, ElementStep::Take { .. }) {
                    state.result = value.duplicate();
                    state.first_absent = true;
                    state.stage = ArrayMutatorStage::AwaitFirstWrite;
                    continue;
                }
                state.first = Some(value);
                state.stage = if matches!(step, ElementStep::Swap { .. }) {
                    ArrayMutatorStage::AwaitSecondRead
                } else {
                    ArrayMutatorStage::AwaitFirstWrite
                };
            }
            ArrayMutatorStage::AwaitSecondRead => {
                if let Some(value) = completion.take() {
                    state.second = Some(value);
                    state.stage = ArrayMutatorStage::AwaitFirstWrite;
                    continue;
                }
                let ElementStep::Swap { right, .. } = current_step(&state)? else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "a non-swap step reached the second read stage",
                    }
                    .into());
                };
                // Both ends are read before either is written, so neither value
                // is lost when the two writes land.
                match read_element(runtime, &mut state, right, execution_budget)? {
                    ElementRead::Absent => {
                        state.second_absent = true;
                        state.stage = ArrayMutatorStage::AwaitFirstWrite;
                    }
                    ElementRead::Value(value) => {
                        completion = Some(value);
                    }
                    ElementRead::Suspend(call) => return suspend(state, call, return_to),
                }
            }
            ArrayMutatorStage::AwaitFirstWrite => {
                if completion.take().is_some() {
                    state.stage = finish_first_write(&mut state)?;
                    continue;
                }
                let step = current_step(&state)?;
                let (index, value, absent) = match step {
                    ElementStep::Store { index } | ElementStep::Drop { index } => {
                        (index, state.first.take(), state.first_absent)
                    }
                    ElementStep::Take { index } => (index, None, true),
                    ElementStep::Move { to, .. } => (to, state.first.take(), state.first_absent),
                    // The left value lands at the right index.
                    ElementStep::Swap { right, .. } => {
                        (right, state.first.take(), state.first_absent)
                    }
                };
                match write_element(runtime, &mut state, index, value, absent, execution_budget)? {
                    ElementWrite::Complete => state.stage = finish_first_write(&mut state)?,
                    ElementWrite::Suspend(call) => return suspend(state, call, return_to),
                }
            }
            ArrayMutatorStage::AwaitSecondWrite => {
                if completion.take().is_some() {
                    state.next_move = state.next_move.saturating_add(1);
                    state.stage = ArrayMutatorStage::NextStep;
                    continue;
                }
                let ElementStep::Swap { left, .. } = current_step(&state)? else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "a non-swap step reached the second write stage",
                    }
                    .into());
                };
                let value = state.second.take();
                let absent = state.second_absent;
                match write_element(runtime, &mut state, left, value, absent, execution_budget)? {
                    ElementWrite::Complete => {
                        state.next_move = state.next_move.saturating_add(1);
                        state.stage = ArrayMutatorStage::NextStep;
                    }
                    ElementWrite::Suspend(call) => return suspend(state, call, return_to),
                }
            }
            ArrayMutatorStage::AwaitLengthWrite => {
                if completion.take().is_some() {
                    state.stage = ArrayMutatorStage::Done;
                    continue;
                }
                // `reverse`, `fill`, and `copyWithin` never change the length,
                // so they skip the write entirely and return the receiver.
                if matches!(
                    state.mutator,
                    ArrayMutator::Reverse | ArrayMutator::Fill | ArrayMutator::CopyWithin
                ) {
                    state.result = state.target.duplicate();
                    state.stage = ArrayMutatorStage::Done;
                    continue;
                }
                // A real Array's `length` is exotic: the ordinary write path
                // refuses it because a script-visible write must first run a
                // resumable numeric conversion. The mutators already hold an
                // exact length, so they reach the array path directly.
                if let StoredValue::Object(object) = state.target
                    && runtime.is_array_object(object)?
                {
                    let requested =
                        u32::try_from(state.final_length).map_err(|_| array_too_long(&state))?;
                    match runtime.set_array_length(object, requested)? {
                        ArrayLengthWriteOutcome::Complete
                        | ArrayLengthWriteOutcome::BlockedByNonConfigurable { .. } => {}
                        ArrayLengthWriteOutcome::ReadOnly => {
                            return Err(NativeFailure::Abrupt(property_exception_at(
                                state.realm,
                                state.origin.clone(),
                                None,
                                PropertyFailure::ReadOnly,
                            )?));
                        }
                    }
                    state.stage = ArrayMutatorStage::Done;
                    continue;
                }
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                let length =
                    StoredValue::Number(JsNumber::from_f64(length_as_f64(state.final_length)));
                charge_mutator_lookup(runtime, &state.target, execution_budget)?;
                match write_static_property(
                    runtime,
                    state.realm,
                    &state.target,
                    key,
                    length,
                    true,
                    execution_budget,
                )? {
                    PropertyWriteOutcome::Complete => state.stage = ArrayMutatorStage::Done,
                    PropertyWriteOutcome::Setter {
                        function,
                        receiver,
                        value,
                    } => {
                        return suspend(
                            state,
                            SuspendedCall {
                                function,
                                receiver,
                                argument: Some(value),
                            },
                            return_to,
                        );
                    }
                    PropertyWriteOutcome::Failed(failure) => {
                        return Err(NativeFailure::Abrupt(property_exception_at(
                            state.realm,
                            state.origin.clone(),
                            None,
                            failure,
                        )?));
                    }
                }
            }
            ArrayMutatorStage::Done => {
                return Ok(NativeDispatch::Immediate(state.result));
            }
        }
    }
}

/// Plans the element moves each mutator performs.
///
/// Planning up front is what lets one driver serve all seven: the differences
/// between them are entirely in this table of source/destination pairs and in
/// the final length, so the stepping, suspension, and hole handling are shared.
/// A `from` of `u64::MAX` marks a write of a call argument rather than a move.
#[expect(
    clippy::too_many_lines,
    reason = "each mutator's step plan is one arm of a single audited table, and splitting it would scatter the ordering contract"
)]
fn plan_moves(state: &mut ArrayMutatorContinuation) -> Result<(), NativeFailure> {
    let length = state.length;
    match state.mutator {
        ArrayMutator::Push => {
            let extra = usize_to_u64(state.arguments.len());
            let total = length
                .checked_add(extra)
                .filter(|total| *total <= MAX_ARRAY_LENGTH)
                .ok_or_else(|| array_too_long(state))?;
            reserve_moves(state, state.arguments.len())?;
            for offset in 0..extra {
                state.moves.push(ElementStep::Store {
                    index: length.saturating_add(offset),
                });
            }
            state.final_length = total;
            state.result = StoredValue::Number(JsNumber::from_f64(length_as_f64(total)));
        }
        ArrayMutator::Pop => {
            let Some(last) = length.checked_sub(1) else {
                // An empty array-like still writes `length` back as `0`, which
                // is why `pop` on `{length:-3}` leaves `length` at `0`.
                state.final_length = 0;
                return Ok(());
            };
            reserve_moves(state, 1)?;
            state.moves.push(ElementStep::Take { index: last });
            state.final_length = last;
        }
        ArrayMutator::Shift => {
            let Some(remaining) = length.checked_sub(1) else {
                state.final_length = 0;
                return Ok(());
            };
            let count = usize::try_from(remaining).map_err(|_| EngineFault::RuntimeInvariant {
                message: "array shift length exceeded the addressable step plan",
            })?;
            reserve_moves(state, count.saturating_add(2))?;
            // Index zero is read for the return value, every later element
            // slides down one, and the vacated final index is removed.
            state.moves.push(ElementStep::Take { index: 0 });
            for index in 1..=remaining {
                state.moves.push(ElementStep::Move {
                    from: index,
                    to: index - 1,
                });
            }
            state.moves.push(ElementStep::Drop { index: remaining });
            state.final_length = remaining;
        }
        ArrayMutator::Unshift => {
            let extra = usize_to_u64(state.arguments.len());
            let total = length
                .checked_add(extra)
                .filter(|total| *total <= MAX_ARRAY_LENGTH)
                .ok_or_else(|| array_too_long(state))?;
            let count = usize::try_from(length).map_err(|_| EngineFault::RuntimeInvariant {
                message: "array unshift length exceeded the addressable step plan",
            })?;
            reserve_moves(state, count.saturating_add(state.arguments.len()))?;
            // Existing elements move up, highest first, so no destination is
            // overwritten before its own source is read.
            if extra > 0 {
                for index in (0..length).rev() {
                    state.moves.push(ElementStep::Move {
                        from: index,
                        to: index.saturating_add(extra),
                    });
                }
            }
            for offset in 0..extra {
                state.moves.push(ElementStep::Store { index: offset });
            }
            state.final_length = total;
            state.result = StoredValue::Number(JsNumber::from_f64(length_as_f64(total)));
        }
        ArrayMutator::Reverse => {
            // Each pair is swapped by reading both ends before either is
            // written, so the middle element of an odd length is untouched.
            let pairs = length / 2;
            let count = usize::try_from(pairs.saturating_mul(2)).map_err(|_| {
                EngineFault::RuntimeInvariant {
                    message: "array reverse length exceeded the addressable step plan",
                }
            })?;
            reserve_moves(state, count)?;
            for index in 0..pairs {
                let mirror = length - 1 - index;
                state.moves.push(ElementStep::Swap {
                    left: index,
                    right: mirror,
                });
            }
            state.final_length = length;
        }
        ArrayMutator::Fill => {
            // Crossed bounds fill nothing, which is why `[1,2,3].fill(0,2,1)` is
            // unchanged.
            let span = state.fill_end.saturating_sub(state.fill_start);
            let count = usize::try_from(span).map_err(|_| EngineFault::RuntimeInvariant {
                message: "array fill span exceeded the addressable step plan",
            })?;
            reserve_moves(state, count)?;
            for index in state.fill_start..state.fill_end {
                state.moves.push(ElementStep::Store { index });
            }
            state.final_length = length;
        }
        ArrayMutator::CopyWithin => {
            // The count is `min(final - from, len - to)`; a negative difference
            // saturates to an empty plan, which is why `[1,2,3].copyWithin(0,5)`
            // is unchanged (`quickjs.c:43001`).
            let count = state
                .copy_final
                .saturating_sub(state.copy_from)
                .min(state.length.saturating_sub(state.copy_to));
            let step_count = usize::try_from(count).map_err(|_| EngineFault::RuntimeInvariant {
                message: "array copyWithin count exceeded the addressable step plan",
            })?;
            reserve_moves(state, step_count)?;
            // Overlapping ranges with the source below the destination are
            // copied backward so no destination is written before its own
            // source is read (`quickjs.c:43003-43004`); every other case walks
            // forward.
            if state.copy_from < state.copy_to
                && state.copy_to < state.copy_from.saturating_add(count)
            {
                for offset in (0..count).rev() {
                    state.moves.push(ElementStep::Move {
                        from: state.copy_from.saturating_add(offset),
                        to: state.copy_to.saturating_add(offset),
                    });
                }
            } else {
                for offset in 0..count {
                    state.moves.push(ElementStep::Move {
                        from: state.copy_from.saturating_add(offset),
                        to: state.copy_to.saturating_add(offset),
                    });
                }
            }
            state.final_length = length;
        }
    }
    Ok(())
}

/// Reserves the move plan's storage fallibly.
fn reserve_moves(
    state: &mut ArrayMutatorContinuation,
    additional: usize,
) -> Result<(), NativeFailure> {
    state
        .moves
        .try_reserve_exact(additional)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional,
        })?;
    Ok(())
}

/// Returns the argument a planned write should store at `index`.
fn argument_for(state: &ArrayMutatorContinuation, index: u64) -> StoredValue {
    match state.mutator {
        // `fill` writes its single value at every index in range.
        ArrayMutator::Fill => state
            .arguments
            .first()
            .map_or(StoredValue::Undefined, StoredValue::duplicate),
        // `unshift` writes argument `n` at index `n`.
        ArrayMutator::Unshift => usize::try_from(index)
            .ok()
            .and_then(|index| state.arguments.get(index))
            .map_or(StoredValue::Undefined, StoredValue::duplicate),
        // `push` writes argument `n` at `length + n`.
        _ => index
            .checked_sub(state.length)
            .and_then(|offset| usize::try_from(offset).ok())
            .and_then(|offset| state.arguments.get(offset))
            .map_or(StoredValue::Undefined, StoredValue::duplicate),
    }
}

/// Reports `TypeError: Array loo long`.
///
/// The misspelling is upstream's (`quickjs.c:41933`), and the message is
/// observable, so it is reproduced rather than corrected.
fn array_too_long(state: &ArrayMutatorContinuation) -> NativeFailure {
    match JsString::from_utf8("Array loo long") {
        Ok(message) => NativeFailure::Abrupt(PendingException {
            realm: state.realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message,
            },
            origin: state.origin.clone(),
        }),
        Err(error) => error.into(),
    }
}

/// One suspended accessor call.
struct SuspendedCall {
    function: FunctionId,
    receiver: StoredValue,
    argument: Option<StoredValue>,
}

/// Suspends into an accessor call that resumes this continuation.
fn suspend(
    state: ArrayMutatorContinuation,
    call: SuspendedCall,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    let arguments = match call.argument {
        None => CallArguments::empty(),
        Some(value) => {
            let mut values = Vec::new();
            values
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 1,
                })?;
            values.push(value);
            CallArguments::from_values(values)
        }
    };
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::ArrayMutator(Box::new(state)));
    Ok(NativeDispatch::Call(NativeCall {
        function: call.function,
        receiver: call.receiver,
        arguments,
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

/// Charges one property lookup, tolerating a primitive receiver.
fn charge_mutator_lookup(
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
///
/// `ToLength` bounds every value by `2^53 - 1`, so the conversion is exact.
#[expect(
    clippy::cast_precision_loss,
    reason = "ToLength bounds every length by 2^53 - 1, which binary64 represents exactly"
)]
fn length_as_f64(length: u64) -> f64 {
    length as f64
}

/// Returns the property key for one element index.
fn element_key(index: u64) -> Result<PropertyKey, NativeFailure> {
    let index = u32::try_from(index).map_err(|_| EngineFault::RuntimeInvariant {
        message: "array mutator index exceeded the array-index domain",
    })?;
    let index = ArrayIndex::new(index).ok_or(EngineFault::RuntimeInvariant {
        message: "array mutator index reached the non-index sentinel",
    })?;
    Ok(PropertyKey::from_index(index))
}

/// Extracts the awaited completion value.
fn take_completion(completion: &mut Option<StoredValue>) -> Result<StoredValue, NativeFailure> {
    completion.take().ok_or_else(|| {
        NativeFailure::Execution(
            EngineFault::RuntimeInvariant {
                message: "an array mutator resumed without its awaited completion",
            }
            .into(),
        )
    })
}

/// The outcome of one element read.
enum ElementRead {
    /// The index has no property, so a hole must be preserved.
    Absent,
    Value(StoredValue),
    Suspend(SuspendedCall),
}

/// The outcome of one element write.
enum ElementWrite {
    Complete,
    Suspend(SuspendedCall),
}

/// Returns the step currently being performed.
fn current_step(state: &ArrayMutatorContinuation) -> Result<ElementStep, NativeFailure> {
    state.moves.get(state.next_move).copied().ok_or_else(|| {
        EngineFault::RuntimeInvariant {
            message: "an array mutator stage ran with no planned step",
        }
        .into()
    })
}

/// Advances past the first write, entering the next stage.
///
/// A `Swap` still owes its second write; every other step is finished, so the
/// cursor moves on here. Advancing in one place is what keeps the driver from
/// re-running a completed step.
fn finish_first_write(
    state: &mut ArrayMutatorContinuation,
) -> Result<ArrayMutatorStage, NativeFailure> {
    if matches!(current_step(state)?, ElementStep::Swap { .. }) {
        return Ok(ArrayMutatorStage::AwaitSecondWrite);
    }
    state.next_move = state.next_move.saturating_add(1);
    Ok(ArrayMutatorStage::NextStep)
}

/// Reads one element, distinguishing an absent index from an `undefined` value.
fn read_element(
    runtime: &mut Runtime,
    state: &mut ArrayMutatorContinuation,
    index: u64,
    execution_budget: &mut ExecutionBudget,
) -> Result<ElementRead, NativeFailure> {
    let key = element_key(index)?;
    charge_mutator_lookup(runtime, &state.target, execution_budget)?;
    // A missing index is a hole, not `undefined`, so the destination is deleted
    // rather than written. This is what keeps `[1,,3].reverse()` sparse.
    if !has_property(runtime, state.realm, &state.target, &key)? {
        return Ok(ElementRead::Absent);
    }
    match read_static_property(runtime, state.realm, &state.target, &key)? {
        PropertyReadOutcome::Value(value) => Ok(ElementRead::Value(value)),
        PropertyReadOutcome::Getter { function, receiver } => {
            Ok(ElementRead::Suspend(SuspendedCall {
                function,
                receiver,
                argument: None,
            }))
        }
        PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            state.realm,
            state.origin.clone(),
            None,
            failure,
        )?)),
    }
}

/// Writes or deletes one element.
fn write_element(
    runtime: &mut Runtime,
    state: &mut ArrayMutatorContinuation,
    index: u64,
    value: Option<StoredValue>,
    absent: bool,
    execution_budget: &mut ExecutionBudget,
) -> Result<ElementWrite, NativeFailure> {
    let key = element_key(index)?;
    charge_mutator_lookup(runtime, &state.target, execution_budget)?;
    if absent {
        match delete_static_property(runtime, &state.target, &key)? {
            // A refused delete is not an error here: the element simply stays,
            // which is the same outcome a non-configurable property produces.
            PropertyDeleteOutcome::Deleted | PropertyDeleteOutcome::Refused => {
                return Ok(ElementWrite::Complete);
            }
            PropertyDeleteOutcome::Failed(failure) => {
                return Err(NativeFailure::Abrupt(property_exception_at(
                    state.realm,
                    state.origin.clone(),
                    None,
                    failure,
                )?));
            }
        }
    }
    let value = value.ok_or(EngineFault::RuntimeInvariant {
        message: "an array mutator write stage ran without a value",
    })?;
    match write_static_property(
        runtime,
        state.realm,
        &state.target,
        key,
        value,
        true,
        execution_budget,
    )? {
        PropertyWriteOutcome::Complete => Ok(ElementWrite::Complete),
        PropertyWriteOutcome::Setter {
            function,
            receiver,
            value,
        } => Ok(ElementWrite::Suspend(SuspendedCall {
            function,
            receiver,
            argument: Some(value),
        })),
        PropertyWriteOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            state.realm,
            state.origin.clone(),
            None,
            failure,
        )?)),
    }
}

/// Which stage of a splice a continuation resumes into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArraySpliceStage {
    /// Awaiting the `length` property read.
    AwaitLength,
    /// Awaiting `ToLength` of the length value.
    AwaitLengthConversion,
    /// Awaiting `ToIntegerOrInfinity` of the start argument.
    AwaitStart,
    /// Awaiting `ToIntegerOrInfinity` of the delete-count argument.
    AwaitDeleteCount,
    /// Ready to extract the next removed element.
    NextExtract,
    /// Awaiting an extracted element's read.
    AwaitExtract,
    /// Ready to perform the next planned step of the shift.
    NextStep,
    /// Awaiting a shifted element's read.
    AwaitShiftRead,
    /// Awaiting a shifted element's write.
    AwaitShiftWrite,
    /// Awaiting the final `length` write.
    AwaitLengthWrite,
    /// Finished.
    Done,
}

/// One in-progress `Array.prototype.splice`.
pub(crate) struct ArraySpliceContinuation {
    /// The coerced receiver being spliced.
    target: StoredValue,
    /// The unconverted `start` and `deleteCount` arguments plus the insertions.
    arguments: Vec<StoredValue>,
    /// The element count from the single `ToLength` length read.
    length: u64,
    /// The resolved start index.
    start: u64,
    /// The resolved number of elements to remove.
    removed: u64,
    /// The next index to extract into the result.
    next_extract: u64,
    /// The planned shift steps, consumed front to back.
    moves: Vec<ElementStep>,
    /// The index of the next planned step.
    next_move: usize,
    /// The value read for the current step.
    pending: Option<StoredValue>,
    /// Whether the current step's source was absent.
    pending_absent: bool,
    /// The array of removed elements.
    destination: ObjectId,
    /// The next index to write in the destination.
    written: u64,
    /// The length to write back once the shift finishes.
    final_length: u64,
    /// Whether the argument conversions have completed.
    planned: bool,
    realm: RealmId,
    stage: ArraySpliceStage,
    origin: JsStackFrame,
}

impl ArraySpliceContinuation {
    /// The receiver, the destination, the pending element, and each argument.
    pub(crate) fn retained_values(&self) -> u64 {
        3_u64.saturating_add(usize_to_u64(self.arguments.len()))
    }

    /// Reports the traced roots this continuation retains.
    pub(crate) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        mark(CollectionRoot::Heap(HeapReference::Object(
            self.destination,
        )));
        if let Some(pending) = &self.pending {
            trace_stored_value_root(pending, mark);
        }
        for argument in &self.arguments {
            trace_stored_value_root(argument, mark);
        }
    }

    /// Returns the number of elements being inserted.
    fn insertion_count(&self) -> u64 {
        usize_to_u64(self.arguments.len().saturating_sub(2))
    }

    /// Returns the insertion belonging to one destination index.
    fn insertion_at(&self, offset: u64) -> StoredValue {
        usize::try_from(offset)
            .ok()
            .and_then(|offset| self.arguments.get(offset.saturating_add(2)))
            .map_or(StoredValue::Undefined, StoredValue::duplicate)
    }
}

/// Starts `Array.prototype.splice`.
pub(super) fn begin_array_splice(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(receiver, StoredValue::Undefined | StoredValue::Null) {
        return Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("cannot convert to object")?,
            },
            origin,
        }));
    }
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
    let destination = runtime.allocate_array(realm, Vec::new())?;
    let state = ArraySpliceContinuation {
        target: receiver,
        arguments: collected,
        length: 0,
        start: 0,
        removed: 0,
        next_extract: 0,
        moves: Vec::new(),
        next_move: 0,
        pending: None,
        pending_absent: false,
        destination,
        written: 0,
        final_length: 0,
        planned: false,
        realm,
        stage: ArraySpliceStage::AwaitLength,
        origin,
    };
    advance_array_splice(runtime, state, None, return_to, execution_budget)
}

/// Resumes a splice after an awaited read, write, or conversion.
#[allow(
    clippy::too_many_lines,
    reason = "the length, argument, extraction, shift, and length-write phases form one traced continuation"
)]
pub(super) fn advance_array_splice(
    runtime: &mut Runtime,
    mut state: ArraySpliceContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            ArraySpliceStage::AwaitLength => {
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                charge_splice_lookup(runtime, &state.target, execution_budget)?;
                match read_static_property(runtime, state.realm, &state.target, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        completion = Some(value);
                        state.stage = ArraySpliceStage::AwaitLengthConversion;
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.stage = ArraySpliceStage::AwaitLengthConversion;
                        return suspend_splice(state, function, receiver, None, return_to);
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(splice_failure(&state, failure));
                    }
                }
            }
            ArraySpliceStage::AwaitLengthConversion => {
                let value = take_completion(&mut completion)?;
                let number = operator_to_number(value, state.realm, &state.origin)?;
                state.length = number_to_length(number);
                state.stage = ArraySpliceStage::AwaitStart;
            }
            ArraySpliceStage::AwaitStart => {
                if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    state.start = splice_bound(number_to_integer_or_infinity(number), state.length);
                    state.stage = ArraySpliceStage::AwaitDeleteCount;
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
                            OperatorPrimitiveTarget::ArraySpliceArgument(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    Some(value) => completion = Some(value.duplicate()),
                    None => {
                        // `splice()` with no arguments removes nothing.
                        state.start = 0;
                        state.removed = 0;
                        plan_splice(&mut state)?;
                        state.stage = ArraySpliceStage::NextExtract;
                    }
                }
            }
            ArraySpliceStage::AwaitDeleteCount => {
                if let Some(value) = completion.take() {
                    let number = operator_to_number(value, state.realm, &state.origin)?;
                    let requested = number_to_integer_or_infinity(number);
                    let available = state.length.saturating_sub(state.start);
                    state.removed = clamp_count(requested, available);
                    plan_splice(&mut state)?;
                    state.stage = ArraySpliceStage::NextExtract;
                    continue;
                }
                match state.arguments.get(1) {
                    Some(value) if needs_conversion(value) => {
                        let value = value.duplicate();
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::Number,
                            OperatorPrimitiveTarget::ArraySpliceArgument(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    Some(value) => completion = Some(value.duplicate()),
                    None => {
                        // An absent count removes everything from `start`, which
                        // is why `[1,2,3].splice(1)` leaves `[1]`.
                        state.removed = state.length.saturating_sub(state.start);
                        plan_splice(&mut state)?;
                        state.stage = ArraySpliceStage::NextExtract;
                    }
                }
            }
            ArraySpliceStage::NextExtract => {
                // The removed elements are collected before anything moves, so a
                // getter cannot observe a half-shifted array.
                if state.next_extract >= state.removed {
                    state.written = state.removed;
                    finish_removed(runtime, &state)?;
                    state.stage = ArraySpliceStage::NextStep;
                    continue;
                }
                execution_budget.charge_instructions(1)?;
                let offset = state.next_extract;
                state.next_extract = state.next_extract.saturating_add(1);
                let index = state.start.saturating_add(offset);
                let key = element_key(index)?;
                charge_splice_lookup(runtime, &state.target, execution_budget)?;
                // A removed hole stays a hole in the result.
                if !has_property(runtime, state.realm, &state.target, &key)? {
                    continue;
                }
                match read_static_property(runtime, state.realm, &state.target, &key)? {
                    PropertyReadOutcome::Value(value) => {
                        completion = Some(value);
                        state.stage = ArraySpliceStage::AwaitExtract;
                    }
                    PropertyReadOutcome::Getter { function, receiver } => {
                        state.stage = ArraySpliceStage::AwaitExtract;
                        return suspend_splice(state, function, receiver, None, return_to);
                    }
                    PropertyReadOutcome::Failed(failure) => {
                        return Err(splice_failure(&state, failure));
                    }
                }
            }
            ArraySpliceStage::AwaitExtract => {
                let value = take_completion(&mut completion)?;
                let index = state.next_extract.saturating_sub(1);
                let key = element_key(index)?;
                match runtime.define_array_data_property(
                    state.destination,
                    key,
                    PropertyLayout::data(true, true, true),
                    value,
                )? {
                    ArrayDefineOutcome::Complete => {}
                    ArrayDefineOutcome::ReadOnlyLength | ArrayDefineOutcome::NonExtensible => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "a freshly allocated splice result refused an element",
                        }
                        .into());
                    }
                }
                state.stage = ArraySpliceStage::NextExtract;
            }
            ArraySpliceStage::NextStep => {
                let Some(step) = state.moves.get(state.next_move).copied() else {
                    state.stage = ArraySpliceStage::AwaitLengthWrite;
                    continue;
                };
                execution_budget.charge_instructions(1)?;
                state.pending = None;
                state.pending_absent = false;
                match step {
                    ElementStep::Store { index } => {
                        let offset = index.saturating_sub(state.start);
                        state.pending = Some(state.insertion_at(offset));
                        state.stage = ArraySpliceStage::AwaitShiftWrite;
                    }
                    ElementStep::Drop { .. } => {
                        state.pending_absent = true;
                        state.stage = ArraySpliceStage::AwaitShiftWrite;
                    }
                    ElementStep::Move { from, .. } => {
                        let key = element_key(from)?;
                        charge_splice_lookup(runtime, &state.target, execution_budget)?;
                        if !has_property(runtime, state.realm, &state.target, &key)? {
                            state.pending_absent = true;
                            state.stage = ArraySpliceStage::AwaitShiftWrite;
                            continue;
                        }
                        match read_static_property(runtime, state.realm, &state.target, &key)? {
                            PropertyReadOutcome::Value(value) => {
                                completion = Some(value);
                                state.stage = ArraySpliceStage::AwaitShiftRead;
                            }
                            PropertyReadOutcome::Getter { function, receiver } => {
                                state.stage = ArraySpliceStage::AwaitShiftRead;
                                return suspend_splice(state, function, receiver, None, return_to);
                            }
                            PropertyReadOutcome::Failed(failure) => {
                                return Err(splice_failure(&state, failure));
                            }
                        }
                    }
                    ElementStep::Take { .. } | ElementStep::Swap { .. } => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "a splice plan contained a step it never emits",
                        }
                        .into());
                    }
                }
            }
            ArraySpliceStage::AwaitShiftRead => {
                state.pending = Some(take_completion(&mut completion)?);
                state.stage = ArraySpliceStage::AwaitShiftWrite;
            }
            ArraySpliceStage::AwaitShiftWrite => {
                if completion.take().is_some() {
                    state.next_move = state.next_move.saturating_add(1);
                    state.stage = ArraySpliceStage::NextStep;
                    continue;
                }
                let step = state.moves.get(state.next_move).copied().ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "a splice write stage ran with no planned step",
                    },
                )?;
                let index = match step {
                    ElementStep::Store { index } | ElementStep::Drop { index } => index,
                    ElementStep::Move { to, .. } => to,
                    ElementStep::Take { .. } | ElementStep::Swap { .. } => {
                        return Err(EngineFault::RuntimeInvariant {
                            message: "a splice plan contained a step it never emits",
                        }
                        .into());
                    }
                };
                let key = element_key(index)?;
                charge_splice_lookup(runtime, &state.target, execution_budget)?;
                if state.pending_absent {
                    match delete_static_property(runtime, &state.target, &key)? {
                        PropertyDeleteOutcome::Deleted | PropertyDeleteOutcome::Refused => {}
                        PropertyDeleteOutcome::Failed(failure) => {
                            return Err(splice_failure(&state, failure));
                        }
                    }
                    state.next_move = state.next_move.saturating_add(1);
                    state.stage = ArraySpliceStage::NextStep;
                    continue;
                }
                let value = state.pending.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "a splice write stage ran without a value",
                })?;
                match write_static_property(
                    runtime,
                    state.realm,
                    &state.target,
                    key,
                    value,
                    true,
                    execution_budget,
                )? {
                    PropertyWriteOutcome::Complete => {
                        state.next_move = state.next_move.saturating_add(1);
                        state.stage = ArraySpliceStage::NextStep;
                    }
                    PropertyWriteOutcome::Setter {
                        function,
                        receiver,
                        value,
                    } => {
                        return suspend_splice(state, function, receiver, Some(value), return_to);
                    }
                    PropertyWriteOutcome::Failed(failure) => {
                        return Err(splice_failure(&state, failure));
                    }
                }
            }
            ArraySpliceStage::AwaitLengthWrite => {
                if completion.take().is_some() {
                    state.stage = ArraySpliceStage::Done;
                    continue;
                }
                if let StoredValue::Object(object) = state.target
                    && runtime.is_array_object(object)?
                {
                    let requested = u32::try_from(state.final_length).map_err(|_| {
                        EngineFault::RuntimeInvariant {
                            message: "a splice produced a length outside the array-index domain",
                        }
                    })?;
                    match runtime.set_array_length(object, requested)? {
                        ArrayLengthWriteOutcome::Complete
                        | ArrayLengthWriteOutcome::BlockedByNonConfigurable { .. } => {}
                        ArrayLengthWriteOutcome::ReadOnly => {
                            return Err(splice_failure(&state, PropertyFailure::ReadOnly));
                        }
                    }
                    state.stage = ArraySpliceStage::Done;
                    continue;
                }
                let key = runtime.predefined_property_key(PredefinedAtom::Length);
                let length =
                    StoredValue::Number(JsNumber::from_f64(length_as_f64(state.final_length)));
                charge_splice_lookup(runtime, &state.target, execution_budget)?;
                match write_static_property(
                    runtime,
                    state.realm,
                    &state.target,
                    key,
                    length,
                    true,
                    execution_budget,
                )? {
                    PropertyWriteOutcome::Complete => state.stage = ArraySpliceStage::Done,
                    PropertyWriteOutcome::Setter {
                        function,
                        receiver,
                        value,
                    } => {
                        return suspend_splice(state, function, receiver, Some(value), return_to);
                    }
                    PropertyWriteOutcome::Failed(failure) => {
                        return Err(splice_failure(&state, failure));
                    }
                }
            }
            ArraySpliceStage::Done => {
                return Ok(NativeDispatch::Immediate(StoredValue::Object(
                    state.destination,
                )));
            }
        }
    }
}

/// Plans the shift that follows a splice's extraction.
///
/// The tail moves by `insertions - removed`. Moving it in the correct direction
/// is what keeps a source from being overwritten before it is read: growing
/// walks the tail from the end, shrinking walks it from the front.
fn plan_splice(state: &mut ArraySpliceContinuation) -> Result<(), NativeFailure> {
    if state.planned {
        return Ok(());
    }
    state.planned = true;
    let inserted = state.insertion_count();
    let tail_start = state.start.saturating_add(state.removed);
    let tail_length = state.length.saturating_sub(tail_start);
    let final_length = state
        .length
        .saturating_sub(state.removed)
        .saturating_add(inserted);
    state.final_length = final_length;

    let tail_count = usize::try_from(tail_length).map_err(|_| EngineFault::RuntimeInvariant {
        message: "a splice tail exceeded the addressable step plan",
    })?;
    let insert_count = usize::try_from(inserted).map_err(|_| EngineFault::RuntimeInvariant {
        message: "a splice insertion count exceeded the addressable step plan",
    })?;
    let shrink = state.removed.saturating_sub(inserted);
    let drop_count = usize::try_from(shrink).map_err(|_| EngineFault::RuntimeInvariant {
        message: "a splice shrink exceeded the addressable step plan",
    })?;
    state
        .moves
        .try_reserve_exact(
            tail_count
                .saturating_add(insert_count)
                .saturating_add(drop_count),
        )
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: tail_count,
        })?;

    if inserted > state.removed {
        // Growing: walk the tail from the end so no destination is written
        // before its own source is read.
        let shift = inserted - state.removed;
        for offset in (0..tail_length).rev() {
            let from = tail_start.saturating_add(offset);
            state.moves.push(ElementStep::Move {
                from,
                to: from.saturating_add(shift),
            });
        }
    } else if inserted < state.removed {
        // Shrinking: walk the tail from the front for the same reason.
        let shift = state.removed - inserted;
        for offset in 0..tail_length {
            let from = tail_start.saturating_add(offset);
            state.moves.push(ElementStep::Move {
                from,
                to: from - shift,
            });
        }
        // The now-vacant trailing slots are removed before the length shrinks.
        for index in final_length..state.length {
            state.moves.push(ElementStep::Drop { index });
        }
    }
    // The insertions land in the gap the extraction left.
    for offset in 0..inserted {
        state.moves.push(ElementStep::Store {
            index: state.start.saturating_add(offset),
        });
    }
    Ok(())
}

/// Sets the removed-elements array's length.
fn finish_removed(
    runtime: &mut Runtime,
    state: &ArraySpliceContinuation,
) -> Result<(), NativeFailure> {
    let length = u32::try_from(state.removed).map_err(|_| EngineFault::RuntimeInvariant {
        message: "a splice removed more elements than the array-index domain allows",
    })?;
    match runtime.set_array_length(state.destination, length)? {
        ArrayLengthWriteOutcome::Complete
        | ArrayLengthWriteOutcome::BlockedByNonConfigurable { .. } => Ok(()),
        ArrayLengthWriteOutcome::ReadOnly => Err(EngineFault::RuntimeInvariant {
            message: "a freshly allocated splice result refused its length",
        }
        .into()),
    }
}

/// Resolves `splice`'s relative start index.
fn splice_bound(value: f64, length: u64) -> u64 {
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
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the preceding bounds keep the value inside the u64 domain exactly"
    )]
    let bound = resolved as u64;
    bound
}

/// Clamps a delete count into `0..=available`.
fn clamp_count(requested: f64, available: u64) -> u64 {
    if requested <= 0.0 {
        return 0;
    }
    let available_as_f64 = length_as_f64(available);
    if requested >= available_as_f64 {
        return available;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the preceding bounds keep the count inside the u64 domain exactly"
    )]
    let count = requested as u64;
    count
}

/// Suspends into a call that resumes this splice.
fn suspend_splice(
    state: ArraySpliceContinuation,
    function: FunctionId,
    receiver: StoredValue,
    argument: Option<StoredValue>,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    let arguments = match argument {
        None => CallArguments::empty(),
        Some(value) => {
            let mut values = Vec::new();
            values
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 1,
                })?;
            values.push(value);
            CallArguments::from_values(values)
        }
    };
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::ArraySplice(Box::new(state)));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments,
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

/// Builds the exception a failed property operation reports.
fn splice_failure(state: &ArraySpliceContinuation, failure: PropertyFailure) -> NativeFailure {
    match property_exception_at(state.realm, state.origin.clone(), None, failure) {
        Ok(exception) => NativeFailure::Abrupt(exception),
        Err(error) => error.into(),
    }
}

/// Charges one property lookup, tolerating a primitive receiver.
fn charge_splice_lookup(
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
