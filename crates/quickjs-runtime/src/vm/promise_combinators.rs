//! Resumable ECMAScript Promise constructor combinators.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use crate::object::PromiseCapability;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::enum_variant_names,
    reason = "the Await prefix makes every variant visibly denote a suspended observable operation"
)]
enum PromiseCombinatorStage {
    AwaitPromiseResolve,
    AwaitIteratorMethod,
    AwaitIterator,
    AwaitNextMethod,
    AwaitNextResult,
    AwaitDone,
    AwaitValue,
    AwaitResolvedInput,
    AwaitThenMethod,
    AwaitThenCall,
    AwaitFinalSettlement,
    AwaitCloseReturn,
    AwaitCloseCall,
}

pub(super) struct PromiseCombinatorContinuation {
    constructor: FunctionId,
    kind: PromiseCombinatorKind,
    shared: Rc<RefCell<PromiseCombinatorShared>>,
    iterable: StoredValue,
    iterator: Option<StoredValue>,
    next: Option<StoredValue>,
    result: Option<StoredValue>,
    next_promise: Option<StoredValue>,
    on_fulfilled: Option<StoredValue>,
    on_rejected: Option<StoredValue>,
    promise_resolve: Option<FunctionId>,
    abrupt_reason: Option<StoredValue>,
    index: usize,
    iterator_done: bool,
    realm: RealmId,
    stage: PromiseCombinatorStage,
    origin: JsStackFrame,
}

impl PromiseCombinatorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        let shared = self.shared.borrow();
        let shared_values = usize_to_u64(shared.values.len()).saturating_add(3);
        shared_values
            .saturating_add(2)
            .saturating_add(u64::from(self.iterator.is_some()))
            .saturating_add(u64::from(self.next.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
            .saturating_add(u64::from(self.next_promise.is_some()))
            .saturating_add(u64::from(self.on_fulfilled.is_some()))
            .saturating_add(u64::from(self.on_rejected.is_some()))
            .saturating_add(u64::from(self.promise_resolve.is_some()))
            .saturating_add(u64::from(self.abrupt_reason.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.constructor,
        )));
        trace_stored_value_root(&self.iterable, mark);
        for value in [
            self.iterator.as_ref(),
            self.next.as_ref(),
            self.result.as_ref(),
            self.next_promise.as_ref(),
            self.on_fulfilled.as_ref(),
            self.on_rejected.as_ref(),
            self.abrupt_reason.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            trace_stored_value_root(value, mark);
        }
        if let Some(resolve) = self.promise_resolve {
            mark(CollectionRoot::Heap(HeapReference::Function(resolve)));
        }
        trace_combinator_shared(&self.shared, mark);
    }

    const fn closes_on_abrupt(&self) -> bool {
        self.next.is_some()
            && !self.iterator_done
            && !matches!(
                self.stage,
                PromiseCombinatorStage::AwaitCloseReturn | PromiseCombinatorStage::AwaitCloseCall
            )
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "PerformPromiseAll-family entry keeps constructor, capability, iterable, Realm, source origin, caller completion, and execution authority explicit"
)]
pub(super) fn begin_promise_combinator(
    runtime: &mut Runtime,
    realm: RealmId,
    constructor: FunctionId,
    kind: PromiseCombinatorKind,
    iterable: StoredValue,
    capability: PromiseCapability,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let shared = Rc::new(RefCell::new(PromiseCombinatorShared {
        kind,
        capability,
        values: Vec::new(),
        remaining: 1,
    }));
    let state = PromiseCombinatorContinuation {
        constructor,
        kind,
        shared,
        iterable,
        iterator: None,
        next: None,
        result: None,
        next_promise: None,
        on_fulfilled: None,
        on_rejected: None,
        promise_resolve: None,
        abrupt_reason: None,
        index: 0,
        iterator_done: false,
        realm,
        stage: PromiseCombinatorStage::AwaitPromiseResolve,
        origin,
    };
    read_combinator_property(
        runtime,
        state,
        runtime.predefined_property_key(PredefinedAtom::Resolve),
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "the four PerformPromise combinators share one specification-ordered resumable iterator machine"
)]
pub(super) fn advance_promise_combinator(
    runtime: &mut Runtime,
    mut state: PromiseCombinatorContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        PromiseCombinatorStage::AwaitPromiseResolve => {
            let StoredValue::Function(resolve) = completion else {
                return reject_combinator_type_error(
                    runtime,
                    state,
                    "Promise constructor resolve is not callable",
                    return_to,
                    execution_budget,
                );
            };
            state.promise_resolve = Some(resolve);
            state.stage = PromiseCombinatorStage::AwaitIteratorMethod;
            read_combinator_property(
                runtime,
                state,
                runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
                return_to,
                execution_budget,
            )
        }
        PromiseCombinatorStage::AwaitIteratorMethod => {
            let StoredValue::Function(method) = completion else {
                return reject_combinator_type_error(
                    runtime,
                    state,
                    "value is not iterable",
                    return_to,
                    execution_budget,
                );
            };
            let receiver = state.iterable.duplicate();
            state.stage = PromiseCombinatorStage::AwaitIterator;
            call_combinator_function(method, receiver, CallArguments::empty(), state, return_to)
        }
        PromiseCombinatorStage::AwaitIterator => {
            if completion.heap_reference().is_none() {
                return reject_combinator_type_error(
                    runtime,
                    state,
                    "iterator is not an object",
                    return_to,
                    execution_budget,
                );
            }
            state.iterator = Some(completion);
            state.stage = PromiseCombinatorStage::AwaitNextMethod;
            read_combinator_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        PromiseCombinatorStage::AwaitNextMethod => {
            state.next = Some(completion);
            call_combinator_next(runtime, state, return_to, execution_budget)
        }
        PromiseCombinatorStage::AwaitNextResult => {
            if completion.heap_reference().is_none() {
                return reject_combinator_type_error(
                    runtime,
                    state,
                    "iterator must return an object",
                    return_to,
                    execution_budget,
                );
            }
            state.result = Some(completion);
            state.stage = PromiseCombinatorStage::AwaitDone;
            read_combinator_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        PromiseCombinatorStage::AwaitDone => {
            if completion.is_truthy() {
                state.iterator_done = true;
                return finish_combinator_iteration(runtime, state, return_to);
            }
            state.stage = PromiseCombinatorStage::AwaitValue;
            read_combinator_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        PromiseCombinatorStage::AwaitValue => {
            state.result = None;
            if state.kind != PromiseCombinatorKind::Race {
                let mut shared =
                    state
                        .shared
                        .try_borrow_mut()
                        .map_err(|_| EngineFault::RuntimeInvariant {
                            message: "Promise combinator shared state is already borrowed",
                        })?;
                shared
                    .values
                    .try_reserve(1)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: 1,
                    })?;
                shared.values.push(None);
            }
            let resolve = state.promise_resolve.ok_or(EngineFault::RuntimeInvariant {
                message: "Promise combinator lost the constructor resolve function",
            })?;
            let receiver = StoredValue::Function(state.constructor);
            state.stage = PromiseCombinatorStage::AwaitResolvedInput;
            call_combinator_function(
                resolve,
                receiver,
                promise_call_arguments([completion])?,
                state,
                return_to,
            )
        }
        PromiseCombinatorStage::AwaitResolvedInput => {
            state.next_promise = Some(completion);
            prepare_combinator_handlers(runtime, &mut state)?;
            state.stage = PromiseCombinatorStage::AwaitThenMethod;
            read_combinator_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Then),
                return_to,
                execution_budget,
            )
        }
        PromiseCombinatorStage::AwaitThenMethod => {
            let StoredValue::Function(then) = completion else {
                return reject_combinator_type_error(
                    runtime,
                    state,
                    "Promise combinator input then is not callable",
                    return_to,
                    execution_budget,
                );
            };
            let receiver = state
                .next_promise
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Promise combinator then lookup has no resolved input",
                })?
                .duplicate();
            let on_fulfilled = state
                .on_fulfilled
                .take()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Promise combinator then call has no fulfillment handler",
                })?;
            let on_rejected = state
                .on_rejected
                .take()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Promise combinator then call has no rejection handler",
                })?;
            state.stage = PromiseCombinatorStage::AwaitThenCall;
            call_combinator_function(
                then,
                receiver,
                promise_call_arguments([on_fulfilled, on_rejected])?,
                state,
                return_to,
            )
        }
        PromiseCombinatorStage::AwaitThenCall => {
            state.next_promise = None;
            state.index = state.index.checked_add(1).ok_or_else(|| {
                NativeFailure::Execution(ExecutionError::LimitExceeded {
                    resource: RuntimeResource::FrameValues,
                    limit: MAX_SAFE_INTEGER,
                    observed: u64::MAX,
                })
            })?;
            if usize_to_u64(state.index) >= MAX_SAFE_INTEGER {
                return reject_combinator_range_error(
                    runtime,
                    state,
                    "Promise combinator input limit exceeded",
                    return_to,
                    execution_budget,
                );
            }
            call_combinator_next(runtime, state, return_to, execution_budget)
        }
        PromiseCombinatorStage::AwaitFinalSettlement => {
            let promise = state
                .shared
                .try_borrow()
                .map_err(|_| EngineFault::RuntimeInvariant {
                    message: "Promise combinator shared state is mutably borrowed after settlement",
                })?
                .capability
                .promise
                .duplicate();
            Ok(NativeDispatch::Immediate(promise))
        }
        PromiseCombinatorStage::AwaitCloseReturn => {
            let StoredValue::Function(close) = completion else {
                return reject_stored_combinator_reason(state, return_to);
            };
            let receiver = state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Promise combinator IteratorClose has no iterator",
                })?
                .duplicate();
            state.stage = PromiseCombinatorStage::AwaitCloseCall;
            call_combinator_function(close, receiver, CallArguments::empty(), state, return_to)
        }
        PromiseCombinatorStage::AwaitCloseCall => reject_stored_combinator_reason(state, return_to),
    }
}

fn prepare_combinator_handlers(
    runtime: &mut Runtime,
    state: &mut PromiseCombinatorContinuation,
) -> Result<(), NativeFailure> {
    if state.kind == PromiseCombinatorKind::Race {
        let shared = state
            .shared
            .try_borrow()
            .map_err(|_| EngineFault::RuntimeInvariant {
                message: "Promise.race shared state is mutably borrowed",
            })?;
        state.on_fulfilled = Some(StoredValue::Function(shared.capability.resolve));
        state.on_rejected = Some(StoredValue::Function(shared.capability.reject));
        return Ok(());
    }

    let (resolve, reject) = runtime.allocate_promise_combinator_elements(
        state.realm,
        state.kind,
        state.index,
        &state.shared,
    )?;
    {
        let mut shared =
            state
                .shared
                .try_borrow_mut()
                .map_err(|_| EngineFault::RuntimeInvariant {
                    message: "Promise combinator shared state is already borrowed",
                })?;
        shared.remaining =
            shared
                .remaining
                .checked_add(1)
                .ok_or(ExecutionError::LimitExceeded {
                    resource: RuntimeResource::FrameValues,
                    limit: u64::MAX,
                    observed: u64::MAX,
                })?;
        state.on_fulfilled = Some(match state.kind {
            PromiseCombinatorKind::All | PromiseCombinatorKind::AllSettled => {
                StoredValue::Function(resolve.ok_or(EngineFault::RuntimeInvariant {
                    message: "Promise combinator allocation omitted its resolve element",
                })?)
            }
            PromiseCombinatorKind::Any => StoredValue::Function(shared.capability.resolve),
            PromiseCombinatorKind::Race => unreachable!("Promise.race returned above"),
        });
        state.on_rejected = Some(match state.kind {
            PromiseCombinatorKind::All => StoredValue::Function(shared.capability.reject),
            PromiseCombinatorKind::AllSettled | PromiseCombinatorKind::Any => {
                StoredValue::Function(reject.ok_or(EngineFault::RuntimeInvariant {
                    message: "Promise combinator allocation omitted its reject element",
                })?)
            }
            PromiseCombinatorKind::Race => unreachable!("Promise.race returned above"),
        });
    }
    Ok(())
}

fn finish_combinator_iteration(
    runtime: &mut Runtime,
    state: PromiseCombinatorContinuation,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let (remaining, capability, kind) = {
        let mut shared =
            state
                .shared
                .try_borrow_mut()
                .map_err(|_| EngineFault::RuntimeInvariant {
                    message: "Promise combinator shared state is already borrowed",
                })?;
        shared.remaining =
            shared
                .remaining
                .checked_sub(1)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Promise combinator remaining count underflowed",
                })?;
        (shared.remaining, shared.capability.clone(), shared.kind)
    };
    if remaining != 0 || kind == PromiseCombinatorKind::Race {
        return Ok(NativeDispatch::Immediate(capability.promise));
    }
    let values = take_combinator_values(&state.shared)?;
    let (resolve, result) = match kind {
        PromiseCombinatorKind::All | PromiseCombinatorKind::AllSettled => (
            true,
            StoredValue::Object(runtime.allocate_array(state.realm, values)?),
        ),
        PromiseCombinatorKind::Any => (
            false,
            StoredValue::Object(runtime.allocate_promise_any_error(state.realm, values)?),
        ),
        PromiseCombinatorKind::Race => unreachable!("Promise.race returned above"),
    };
    let function = if resolve {
        capability.resolve
    } else {
        capability.reject
    };
    let mut state = state;
    state.stage = PromiseCombinatorStage::AwaitFinalSettlement;
    call_combinator_function(
        function,
        StoredValue::Undefined,
        promise_call_arguments([result])?,
        state,
        return_to,
    )
}

fn call_combinator_next(
    runtime: &mut Runtime,
    mut state: PromiseCombinatorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let next = state.next.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "Promise combinator iterator advance has no next method",
    })?;
    let StoredValue::Function(next) = next else {
        return reject_combinator_type_error(
            runtime,
            state,
            "iterator next is not callable",
            return_to,
            execution_budget,
        );
    };
    execution_budget.charge_instructions(1)?;
    let receiver = state
        .iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Promise combinator iterator advance has no iterator",
        })?
        .duplicate();
    state.stage = PromiseCombinatorStage::AwaitNextResult;
    call_combinator_function(*next, receiver, CallArguments::empty(), state, return_to)
}

fn call_combinator_function(
    function: FunctionId,
    receiver: StoredValue,
    arguments: CallArguments,
    state: PromiseCombinatorContinuation,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments,
        return_to,
        origin,
        continuations: one_combinator_continuation(state)?,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "property-key ownership remains local to one resumable Get boundary"
)]
fn read_combinator_property(
    runtime: &mut Runtime,
    state: PromiseCombinatorContinuation,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (base, property_name) = match state.stage {
        PromiseCombinatorStage::AwaitPromiseResolve => {
            (&StoredValue::Function(state.constructor), "resolve")
        }
        PromiseCombinatorStage::AwaitIteratorMethod => (&state.iterable, "Symbol.iterator"),
        PromiseCombinatorStage::AwaitNextMethod => (
            state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Promise combinator next lookup has no iterator",
                })?,
            "next",
        ),
        PromiseCombinatorStage::AwaitDone => (
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "Promise combinator done lookup has no iterator result",
            })?,
            "done",
        ),
        PromiseCombinatorStage::AwaitValue => (
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "Promise combinator value lookup has no iterator result",
            })?,
            "value",
        ),
        PromiseCombinatorStage::AwaitThenMethod => (
            state
                .next_promise
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Promise combinator then lookup has no resolved input",
                })?,
            "then",
        ),
        PromiseCombinatorStage::AwaitIterator
        | PromiseCombinatorStage::AwaitNextResult
        | PromiseCombinatorStage::AwaitResolvedInput
        | PromiseCombinatorStage::AwaitThenCall
        | PromiseCombinatorStage::AwaitFinalSettlement
        | PromiseCombinatorStage::AwaitCloseReturn
        | PromiseCombinatorStage::AwaitCloseCall => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Promise combinator call stage attempted a property read",
            }
            .into());
        }
    };
    let base = base.duplicate();
    charge_iterator_property_lookup(runtime, &base, execution_budget)?;
    let name = JsString::from_utf8(property_name)?;
    let dispatch = match begin_value_get(
        runtime,
        &base,
        key,
        Some(&name),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(pending)) => {
            return resume_promise_combinator_abrupt(
                runtime,
                state,
                pending,
                return_to,
                execution_budget,
            );
        }
        Err(failure) => return Err(failure),
    };
    continue_get_after(
        dispatch,
        state,
        promise_combinator_continuation,
        |state, value| {
            advance_promise_combinator(runtime, state, value, return_to, execution_budget)
        },
        "Promise combinator Get produced a structured result",
    )
}

fn promise_combinator_continuation(state: PromiseCombinatorContinuation) -> NativeContinuation {
    NativeContinuation::PromiseCombinator(Box::new(state))
}

pub(super) fn resume_promise_combinator_abrupt(
    runtime: &mut Runtime,
    mut state: PromiseCombinatorContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(
        state.stage,
        PromiseCombinatorStage::AwaitCloseReturn | PromiseCombinatorStage::AwaitCloseCall
    ) {
        return reject_stored_combinator_reason(state, return_to);
    }
    if state.closes_on_abrupt() {
        state.abrupt_reason = Some(pending_exception_value(runtime, pending)?);
        return begin_combinator_close(runtime, state, return_to, execution_budget);
    }
    reject_combinator_pending(runtime, state, pending, return_to)
}

fn begin_combinator_close(
    runtime: &mut Runtime,
    mut state: PromiseCombinatorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let iterator = state
        .iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Promise combinator IteratorClose has no iterator",
        })?
        .duplicate();
    state.stage = PromiseCombinatorStage::AwaitCloseReturn;
    charge_iterator_property_lookup(runtime, &iterator, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::Return);
    let dispatch = match begin_value_get(
        runtime,
        &iterator,
        key,
        None,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(_)) => {
            return reject_stored_combinator_reason(state, return_to);
        }
        Err(failure) => return Err(failure),
    };
    continue_get_after(
        dispatch,
        state,
        promise_combinator_continuation,
        |state, value| {
            advance_promise_combinator(runtime, state, value, return_to, execution_budget)
        },
        "Promise combinator IteratorClose Get produced a structured result",
    )
}

fn reject_stored_combinator_reason(
    mut state: PromiseCombinatorContinuation,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let reason = state
        .abrupt_reason
        .take()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Promise combinator IteratorClose lost the original abrupt reason",
        })?;
    let capability = state
        .shared
        .try_borrow()
        .map_err(|_| EngineFault::RuntimeInvariant {
            message: "Promise combinator shared state is mutably borrowed during rejection",
        })?
        .capability
        .clone();
    call_capability_settlement(capability, false, reason, return_to, state.origin)
}

fn reject_combinator_pending(
    runtime: &mut Runtime,
    state: PromiseCombinatorContinuation,
    pending: PendingException,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let reason = pending_exception_value(runtime, pending)?;
    let capability = state
        .shared
        .try_borrow()
        .map_err(|_| EngineFault::RuntimeInvariant {
            message: "Promise combinator shared state is mutably borrowed during rejection",
        })?
        .capability
        .clone();
    call_capability_settlement(capability, false, reason, return_to, state.origin)
}

fn reject_combinator_type_error(
    runtime: &mut Runtime,
    state: PromiseCombinatorContinuation,
    message: &'static str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    reject_combinator_exception(
        runtime,
        state,
        ExceptionKind::TypeError,
        message,
        return_to,
        execution_budget,
    )
}

fn reject_combinator_range_error(
    runtime: &mut Runtime,
    state: PromiseCombinatorContinuation,
    message: &'static str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    reject_combinator_exception(
        runtime,
        state,
        ExceptionKind::RangeError,
        message,
        return_to,
        execution_budget,
    )
}

fn reject_combinator_exception(
    runtime: &mut Runtime,
    state: PromiseCombinatorContinuation,
    kind: ExceptionKind,
    message: &'static str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let pending = PendingException {
        realm: state.realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin: state.origin.clone(),
    };
    resume_promise_combinator_abrupt(runtime, state, pending, return_to, execution_budget)
}

pub(super) fn dispatch_promise_combinator_element(
    runtime: &mut Runtime,
    element: &PromiseCombinatorElementFunction,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let completion = arguments.take_first_or_undefined();
    if element.already_called.replace(true) {
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    }
    let stored = match element.kind {
        PromiseCombinatorElementKind::AllResolve | PromiseCombinatorElementKind::AnyReject => {
            completion
        }
        PromiseCombinatorElementKind::AllSettledResolve => StoredValue::Object(
            runtime.allocate_promise_settlement_record(element.realm, true, completion)?,
        ),
        PromiseCombinatorElementKind::AllSettledReject => StoredValue::Object(
            runtime.allocate_promise_settlement_record(element.realm, false, completion)?,
        ),
    };
    let (remaining, capability, kind) = {
        let mut shared =
            element
                .shared
                .try_borrow_mut()
                .map_err(|_| EngineFault::RuntimeInvariant {
                    message: "Promise combinator element shared state is already borrowed",
                })?;
        let slot = shared
            .values
            .get_mut(element.index)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Promise combinator element index is outside the result list",
            })?;
        if slot.is_some() {
            return Err(EngineFault::RuntimeInvariant {
                message: "Promise combinator element result slot was already filled",
            }
            .into());
        }
        *slot = Some(stored);
        shared.remaining =
            shared
                .remaining
                .checked_sub(1)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Promise combinator element remaining count underflowed",
                })?;
        (shared.remaining, shared.capability.clone(), shared.kind)
    };
    if remaining != 0 {
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    }
    let values = take_combinator_values(&element.shared)?;
    let (resolve, result) = match kind {
        PromiseCombinatorKind::All | PromiseCombinatorKind::AllSettled => (
            true,
            StoredValue::Object(runtime.allocate_array(element.realm, values)?),
        ),
        PromiseCombinatorKind::Any => (
            false,
            StoredValue::Object(runtime.allocate_promise_any_error(element.realm, values)?),
        ),
        PromiseCombinatorKind::Race => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Promise.race reached an element closure",
            }
            .into());
        }
    };
    call_promise_capability_direct(&capability, resolve, result, return_to, origin)
}

fn take_combinator_values(
    shared: &Rc<RefCell<PromiseCombinatorShared>>,
) -> Result<Vec<StoredValue>, NativeFailure> {
    let mut shared = shared
        .try_borrow_mut()
        .map_err(|_| EngineFault::RuntimeInvariant {
            message: "Promise combinator result list is already borrowed",
        })?;
    let slots = std::mem::take(&mut shared.values);
    let mut values = Vec::new();
    values
        .try_reserve_exact(slots.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: slots.len(),
        })?;
    for slot in slots {
        values.push(slot.ok_or(EngineFault::RuntimeInvariant {
            message: "Promise combinator settled with an empty result slot",
        })?);
    }
    Ok(values)
}

fn one_combinator_continuation(
    state: PromiseCombinatorContinuation,
) -> Result<Vec<NativeContinuation>, NativeFailure> {
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::PromiseCombinator(Box::new(state)));
    Ok(continuations)
}

fn trace_combinator_shared(
    shared: &Rc<RefCell<PromiseCombinatorShared>>,
    mark: &mut dyn FnMut(CollectionRoot),
) {
    let shared = shared.borrow();
    trace_promise_capability(&shared.capability, mark);
    for value in shared.values.iter().flatten() {
        trace_stored_value_root(value, mark);
    }
}
