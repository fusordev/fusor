/*
 * JavaScript bytecode execution and closure semantics derived from QuickJS.
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

//! Promise-backed async-function activation and `await` suspension.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) fn allocate_async_function_settlement(
    runtime: &mut Runtime,
    realm: RealmId,
) -> Result<NativeContinuation, ExecutionError> {
    let prototype = runtime.realm_promise_prototype(realm)?;
    let promise = runtime.allocate_promise_with_prototype(HeapReference::Object(prototype))?;
    let (resolve, reject) = runtime.allocate_promise_resolving_functions(promise, realm)?;
    Ok(NativeContinuation::Promise(
        PromiseContinuation::AsyncFunctionSettlement {
            capability: crate::object::PromiseCapability {
                promise: StoredValue::Object(promise),
                resolve,
                reject,
            },
        },
    ))
}

pub(super) fn begin_async_await(
    runtime: &mut Runtime,
    realm: RealmId,
    value: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let constructor = runtime.realm_promise_constructor(realm)?;
    let dispatch = begin_promise_resolve_with_constructor(
        runtime,
        realm,
        constructor,
        value,
        return_to,
        origin.clone(),
        execution_budget,
    )?;
    match dispatch {
        NativeDispatch::Immediate(value) => finish_async_await(&value, origin),
        NativeDispatch::Call(mut call) => {
            let mut outer = Vec::new();
            outer
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 1,
                })?;
            outer.push(NativeContinuation::AsyncAwait { origin });
            prepend_native_continuations(&mut call, outer)?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            let mut outer = Vec::new();
            outer
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::Frames,
                    additional: 1,
                })?;
            outer.push(NativeContinuation::AsyncAwait { origin });
            attach_native_continuations(&mut frame, outer)?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "PromiseResolve produced a structured await result",
        }
        .into()),
    }
}

pub(super) fn finish_async_await(
    value: &StoredValue,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(promise) = value else {
        return Err(EngineFault::RuntimeInvariant {
            message: "PromiseResolve for await produced a non-object",
        }
        .into());
    };
    Ok(NativeDispatch::AsyncAwait {
        promise: *promise,
        origin,
    })
}

fn async_settlement_index(frame: &Frame) -> Result<usize, EngineFault> {
    let index = frame
        .native_returns
        .iter()
        .rposition(|continuation| {
            matches!(
                continuation,
                NativeContinuation::Promise(PromiseContinuation::AsyncFunctionSettlement { .. })
            )
        })
        .ok_or(EngineFault::RuntimeInvariant {
            message: "awaiting frame has no async-function settlement continuation",
        })?;
    if index.saturating_add(1) != frame.native_returns.len() {
        return Err(EngineFault::RuntimeInvariant {
            message: "async-function settlement is not the innermost native continuation",
        });
    }
    Ok(index)
}

fn async_output_promise(frame: &Frame, index: usize) -> Result<ObjectId, EngineFault> {
    let Some(NativeContinuation::Promise(PromiseContinuation::AsyncFunctionSettlement {
        capability,
    })) = frame.native_returns.get(index)
    else {
        return Err(EngineFault::RuntimeInvariant {
            message: "async settlement index selected another continuation",
        });
    };
    let StoredValue::Object(promise) = capability.promise else {
        return Err(EngineFault::RuntimeInvariant {
            message: "async-function capability does not retain a Promise object",
        });
    };
    Ok(promise)
}

pub(super) fn suspend_async_function(
    runtime: &mut Runtime,
    mut frame: Frame,
    promise: ObjectId,
    origin: JsStackFrame,
) -> Result<(StoredValue, Vec<NativeContinuation>, Option<CallReturn>), ExecutionError> {
    if !runtime
        .objects
        .get(promise)
        .is_some_and(crate::object::HeapObject::is_promise)
    {
        return Err(EngineFault::StaleHeapEdge {
            edge: "await Promise",
            index: promise.index(),
            generation: promise.generation(),
        }
        .into());
    }
    let settlement_index = async_settlement_index(&frame)?;
    let activation = async_output_promise(&frame, settlement_index)?;
    runtime
        .async_function_states
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;

    let return_to = frame.return_to.take();
    let mut outer = std::mem::take(&mut frame.native_returns);
    let settlement = outer.pop().ok_or(EngineFault::RuntimeInvariant {
        message: "async settlement disappeared during suspension",
    })?;
    frame.native_returns.push(settlement);
    frame.reserved_values = frame
        .reserved_values
        .saturating_sub(native_continuation_values(&outer));

    if runtime
        .async_function_states
        .insert(
            activation,
            AsyncFunctionRecord {
                frame,
                awaiting: promise,
                origin,
            },
        )
        .is_some()
    {
        return Err(EngineFault::RuntimeInvariant {
            message: "async-function activation was already suspended",
        }
        .into());
    }
    if let Err(error) = perform_async_function_await(runtime, promise, activation) {
        let removed = runtime.async_function_states.remove(&activation);
        debug_assert!(removed.is_some());
        return Err(match error {
            NativeFailure::Execution(error) => error,
            NativeFailure::Abrupt(_) | NativeFailure::AbruptAfterTransient(_) => {
                EngineFault::RuntimeInvariant {
                    message: "internal async reaction registration threw JavaScript",
                }
                .into()
            }
        });
    }
    runtime.collection_pending = true;
    Ok((StoredValue::Object(activation), outer, return_to))
}

pub(super) fn begin_async_function_resume(
    runtime: &mut Runtime,
    activation: ObjectId,
    kind: crate::object::PromiseReactionKind,
    argument: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let record =
        runtime
            .async_function_states
            .remove(&activation)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "async-function reaction lost its suspended activation",
            })?;
    if !runtime.objects.contains(record.awaiting) {
        return Err(EngineFault::StaleHeapEdge {
            edge: "suspended await Promise",
            index: record.awaiting.index(),
            generation: record.awaiting.generation(),
        }
        .into());
    }
    let mut frame = record.frame;
    match kind {
        crate::object::PromiseReactionKind::Fulfill => push(&mut frame, argument),
        crate::object::PromiseReactionKind::Reject => {
            let realm = code(runtime, frame.code)?.realm;
            frame.resume_abrupt = Some(PendingException {
                realm,
                payload: PendingExceptionPayload::ThrownValue(argument),
                origin: record.origin,
            });
        }
    }
    runtime.collection_pending = true;
    Ok(NativeDispatch::Frame(frame))
}
