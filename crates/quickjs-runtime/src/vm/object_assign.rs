/*
 * JavaScript Object.assign semantics derived from QuickJS.
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

//! `Object.assign`.
//!
//! The operation is a resumable walk over several sources, because both halves
//! of each copied property can run user code: a source getter produces the
//! value and a target setter consumes it. The pinned order interleaves them per
//! key rather than reading a whole source first, so
//! `Object.assign({set a(v){}, set b(v){}}, {get a(){}, get b(){}})` observes
//! `get a`, `set a`, `get b`, `set b` (`quickjs.c:40449-40470`).
//!
//! Three details separate `assign` from the object-spread it resembles:
//!
//! * **The target converts with `ToObject`.** A primitive target is boxed and
//!   the wrapper is the result, while a nullish one throws even when no source
//!   follows, because the conversion precedes the source walk.
//! * **A nullish source is skipped.** Only the target's conversion throws; a
//!   `null` or `undefined` source contributes nothing.
//! * **Each write is a strict `Set`.** A read-only target property or a
//!   non-extensible target therefore throws rather than being silently
//!   dropped, and a target setter runs with the target as its `this`.
//!
//! Symbol keys are copied, which is what separates the source projection here
//! from `Object.keys`' string-only one; non-enumerable keys are not. The
//! enumerable attribute is re-tested against the live source at each step, so a
//! getter that hides or deletes a later key removes it from the copy.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// Which half of a copied property a continuation is awaiting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AssignStage {
    /// A source getter is running; its completion is the value to write.
    AwaitRead,
    /// A target setter is running; its completion is discarded.
    AwaitWrite,
}

/// One in-progress `Object.assign`.
pub(super) struct ObjectAssignContinuation {
    /// The object receiving every copied property, and the operation's result.
    target: StoredValue,
    /// The sources still to visit, reversed so the next one pops off the back.
    remaining: Vec<StoredValue>,
    /// The source being walked, with its own keys captured before the first
    /// read, once one has been resolved to an object.
    current: Option<AssignSource>,
    /// The key whose read or write is suspended.
    pending: Option<PropertyKey>,
    stage: AssignStage,
    realm: RealmId,
    origin: JsStackFrame,
}

/// One source under walk.
struct AssignSource {
    /// The object the reads target, which is a wrapper for a primitive source.
    value: StoredValue,
    /// The source's own keys, captured before its first read.
    keys: ForInSnapshot,
    /// The index into `keys` of the next key to consider.
    next: usize,
}

impl ObjectAssignContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        // The target, the current source, and every source still to visit.
        1_u64
            .saturating_add(u64::from(self.current.is_some()))
            .saturating_add(usize_to_u64(self.remaining.len()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.target, mark);
        if let Some(current) = &self.current {
            trace_stored_value_root(&current.value, mark);
        }
        for source in &self.remaining {
            trace_stored_value_root(source, mark);
        }
    }
}

/// What one attempted write resolved to.
///
/// Both payloads are boxed so the two arms stay the same size: a continuation
/// and a dispatch are each large, and only one is ever live.
enum AssignWrite {
    /// The property landed, so the walk continues with the returned state.
    Complete(Box<ObjectAssignContinuation>),
    /// User code must run first; the state moved into its continuation.
    Suspended(Box<NativeDispatch>),
}

/// Starts `Object.assign(target, ...sources)`.
pub(super) fn begin_object_assign(
    runtime: &mut Runtime,
    realm: RealmId,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let requested = arguments.take_first_or_undefined();
    let target = to_object(runtime, realm, requested, &origin)?;
    let mut remaining = arguments.into_remaining_values();
    // The sources pop from the back, so reversing preserves argument order.
    remaining.reverse();
    let state = ObjectAssignContinuation {
        target,
        remaining,
        current: None,
        pending: None,
        stage: AssignStage::AwaitRead,
        realm,
        origin,
    };
    advance_object_assign(runtime, state, None, return_to, execution_budget)
}

/// Resumes an assignment after a getter or setter returned, then continues.
pub(super) fn advance_object_assign(
    runtime: &mut Runtime,
    mut state: ObjectAssignContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match (completion, state.pending.take()) {
        (Some(value), Some(key)) => match state.stage {
            AssignStage::AwaitRead => {
                match write_assigned_property(
                    runtime,
                    state,
                    key,
                    value,
                    return_to,
                    execution_budget,
                )? {
                    AssignWrite::Suspended(dispatch) => return Ok(*dispatch),
                    AssignWrite::Complete(resumed) => state = *resumed,
                }
            }
            // A setter's completion is discarded; the copy already happened.
            AssignStage::AwaitWrite => state.stage = AssignStage::AwaitRead,
        },
        (None, None) => {}
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Object.assign resumed with a mismatched pending key",
            }
            .into());
        }
    }

    loop {
        // The next key is resolved without holding a borrow on the state, so a
        // write can move the whole continuation into a suspending call.
        let mut candidate = None;
        if let Some(current) = state.current.as_mut() {
            if let Some(key) = current.keys.get(current.next).cloned() {
                current.next = current.next.saturating_add(1);
                candidate = Some((current.value.duplicate(), key));
            } else {
                // This source is finished, so the next one starts fresh.
                state.current = None;
            }
        }
        let Some((source, candidate)) = candidate else {
            if state.current.is_some() {
                continue;
            }
            let Some(requested) = state.remaining.pop() else {
                return Ok(NativeDispatch::Immediate(state.target));
            };
            // A nullish source is skipped; every other primitive is boxed so
            // its exotic own keys are walked the same way an object's are.
            if matches!(requested, StoredValue::Undefined | StoredValue::Null) {
                continue;
            }
            let value = to_object(runtime, state.realm, requested, &state.origin)?;
            let reference = heap_reference_of_object(&value)?;
            let (keys, work) = runtime.try_own_key_snapshot(reference, 0, KeyPhases::ALL)?;
            execution_budget.charge_instructions(work)?;
            state.current = Some(AssignSource {
                value,
                keys,
                next: 0,
            });
            continue;
        };
        execution_budget.charge_instructions(1)?;
        // The attribute is re-tested against the live source, so a getter that
        // hid or deleted a later key removes it from the copy.
        if !own_key_is_enumerable_on(runtime, &source, candidate.key())? {
            continue;
        }
        charge_heap_property_lookup(runtime, &source, execution_budget)?;
        match read_static_property(runtime, state.realm, &source, candidate.key())? {
            PropertyReadOutcome::Value(value) => {
                let key = candidate.key().clone();
                match write_assigned_property(
                    runtime,
                    state,
                    key,
                    value,
                    return_to,
                    execution_budget,
                )? {
                    AssignWrite::Suspended(dispatch) => return Ok(*dispatch),
                    AssignWrite::Complete(resumed) => state = *resumed,
                }
            }
            PropertyReadOutcome::Getter { function, receiver } => {
                state.pending = Some(candidate.key().clone());
                state.stage = AssignStage::AwaitRead;
                return assign_suspend(
                    state,
                    function,
                    receiver,
                    CallArguments::empty(),
                    return_to,
                );
            }
            PropertyReadOutcome::Failed(_) => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Object.assign source failed an own-property read",
                }
                .into());
            }
        }
    }
}

/// Writes one copied property with an ordinary strict `Set`.
///
/// A read-only or non-extensible target throws rather than silently dropping
/// the property, and a target setter suspends the walk with the target as its
/// `this` (`JS_SetProperty` with `JS_PROP_THROW`, `quickjs.c:40462`).
///
/// The state moves in and comes back out on the `Complete` path, so a suspended
/// write can hand the whole continuation to the call it starts.
fn write_assigned_property(
    runtime: &mut Runtime,
    mut state: ObjectAssignContinuation,
    key: PropertyKey,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<AssignWrite, NativeFailure> {
    let name = own_key_name(&key)?;
    // An array target's `length` needs the resumable numeric conversion, whose
    // `RangeError` outranks the write itself. The conversion resumes into the
    // length write, so the assignment is queued behind it: its own continuation
    // is prepended to the call the conversion starts, and a conversion that
    // completes immediately falls through to the ordinary write below.
    if is_array_length_target(runtime, &state.target, &key)? {
        let conversion = array_length_write_target(
            state.target.duplicate(),
            name,
            LengthWriteReport::Throwing,
            &value,
        );
        let realm = state.realm;
        let origin = state.origin.clone();
        state.stage = AssignStage::AwaitWrite;
        state.pending = Some(key);
        let dispatch = begin_operator_primitive_conversion(
            runtime,
            value,
            OperatorPrimitiveHint::Number,
            conversion,
            realm,
            return_to,
            origin,
            execution_budget,
        )?;
        return match dispatch {
            // The length converted and was written without user code, so the
            // walk continues directly.
            NativeDispatch::Immediate(_) => {
                state.pending = None;
                state.stage = AssignStage::AwaitRead;
                Ok(AssignWrite::Complete(Box::new(state)))
            }
            NativeDispatch::Call(mut call) => {
                queue_assignment_behind(&mut call, state)?;
                Ok(AssignWrite::Suspended(Box::new(NativeDispatch::Call(call))))
            }
            // A length write is either immediate or one conversion call; every
            // other dispatch shape is unreachable here.
            NativeDispatch::Frame(_)
            | NativeDispatch::Pair(_, _)
            | NativeDispatch::ForOfRecord { .. }
            | NativeDispatch::ForOfStep { .. }
            | NativeDispatch::ForOfClosed
            | NativeDispatch::CopyDataPropertiesDone => Err(EngineFault::RuntimeInvariant {
                message: "array length conversion produced an unexpected dispatch",
            }
            .into()),
        };
    }
    match write_static_property(
        runtime,
        state.realm,
        &state.target,
        key.clone(),
        value,
        true,
        execution_budget,
    )? {
        PropertyWriteOutcome::Complete => Ok(AssignWrite::Complete(Box::new(state))),
        PropertyWriteOutcome::Setter {
            function,
            receiver,
            value,
        } => {
            let mut arguments = Vec::new();
            arguments
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: 1,
                })?;
            arguments.push(value);
            state.pending = Some(key);
            state.stage = AssignStage::AwaitWrite;
            assign_suspend(
                state,
                function,
                receiver,
                CallArguments::from_values(arguments),
                return_to,
            )
            .map(|dispatch| AssignWrite::Suspended(Box::new(dispatch)))
        }
        PropertyWriteOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            state.realm,
            state.origin,
            Some(&own_key_name(&key)?),
            failure,
        )?)),
    }
}

/// Queues the rest of an assignment behind a call the write already started.
///
/// The call's own continuations run first, so the assignment resumes only after
/// the length write it was waiting on has completed.
fn queue_assignment_behind(
    call: &mut NativeCall,
    state: ObjectAssignContinuation,
) -> Result<(), NativeFailure> {
    call.continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    call.continuations
        .insert(0, NativeContinuation::ObjectAssign(Box::new(state)));
    Ok(())
}

/// Suspends an assignment on one call.
fn assign_suspend(
    state: ObjectAssignContinuation,
    function: FunctionId,
    receiver: StoredValue,
    arguments: CallArguments,
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
    continuations.push(NativeContinuation::ObjectAssign(Box::new(state)));
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
