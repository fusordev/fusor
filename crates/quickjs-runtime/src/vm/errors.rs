/*
 * JavaScript Error intrinsic execution derived from QuickJS.
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

//! Resumable Error constructors and `Error.prototype.toString`.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

#[derive(Clone, Copy)]
pub(super) enum ErrorConstructorStage {
    AwaitPrototype,
    AwaitCause,
}

pub(super) struct ErrorConstructorContinuation {
    pub(super) kind: crate::runtime::ErrorIntrinsicKind,
    pub(super) new_target: FunctionId,
    pub(super) message: StoredValue,
    pub(super) options: Option<StoredValue>,
    pub(super) aggregate_errors: Option<StoredValue>,
    pub(super) object: Option<ObjectId>,
    pub(super) stack: ErrorStackSnapshot,
    pub(super) realm: RealmId,
    pub(super) stage: ErrorConstructorStage,
    pub(super) origin: JsStackFrame,
}

impl ErrorConstructorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.options.is_some()))
            .saturating_add(u64::from(self.aggregate_errors.is_some()))
            .saturating_add(u64::from(self.object.is_some()))
            .saturating_add(self.stack.retained_values())
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
        trace_stored_value_root(&self.message, mark);
        if let Some(options) = &self.options {
            trace_stored_value_root(options, mark);
        }
        if let Some(errors) = &self.aggregate_errors {
            trace_stored_value_root(errors, mark);
        }
        if let Some(object) = self.object {
            mark(CollectionRoot::Heap(HeapReference::Object(object)));
        }
        self.stack.trace_roots(mark);
    }
}

#[derive(Clone, Copy)]
pub(super) enum ErrorToStringStage {
    AwaitName,
    AwaitMessage,
}

pub(super) struct ErrorToStringContinuation {
    pub(super) receiver: StoredValue,
    pub(super) name: Option<JsString>,
    pub(super) realm: RealmId,
    pub(super) stage: ErrorToStringStage,
    pub(super) origin: JsStackFrame,
}

impl ErrorToStringContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(u64::from(self.name.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.receiver, mark);
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Error construction retains its exact constructor, arguments, realm, caller continuation, and execution authority"
)]
pub(super) fn begin_error_constructor(
    runtime: &mut Runtime,
    function: FunctionId,
    kind: crate::runtime::ErrorIntrinsicKind,
    mut arguments: CallArguments,
    construction: Option<FunctionId>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    active_root_frames: &[Frame],
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let new_target = construction.unwrap_or(function);
    let (aggregate_errors, message, options) =
        if kind == crate::runtime::ErrorIntrinsicKind::AggregateError {
            (
                Some(arguments.take_first_or_undefined()),
                arguments.take_first_or_undefined(),
                arguments.take_first(),
            )
        } else {
            (
                None,
                arguments.take_first_or_undefined(),
                arguments.take_first(),
            )
        };
    let stack = capture_error_stack(runtime, active_root_frames, &origin)?;
    let state = ErrorConstructorContinuation {
        kind,
        new_target,
        message,
        options,
        aggregate_errors,
        object: None,
        stack,
        realm,
        stage: ErrorConstructorStage::AwaitPrototype,
        origin,
    };
    read_error_constructor_prototype(runtime, state, return_to, execution_budget)
}

fn read_error_constructor_prototype(
    runtime: &mut Runtime,
    state: ErrorConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let receiver = StoredValue::Function(state.new_target);
    charge_heap_property_lookup(runtime, &receiver, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    match read_heap_property_for_receiver(
        runtime,
        HeapReference::Function(state.new_target),
        receiver,
        &key,
    )? {
        PropertyReadOutcome::Value(value) => {
            advance_error_constructor(runtime, state, value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            error_constructor_getter_call(state, function, receiver, return_to)
        }
        PropertyReadOutcome::Failed(_) => Err(EngineFault::RuntimeInvariant {
            message: "Error newTarget prototype Get failed as a primitive",
        }
        .into()),
    }
}

pub(super) fn advance_error_constructor(
    runtime: &mut Runtime,
    mut state: ErrorConstructorContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ErrorConstructorStage::AwaitPrototype => {
            let prototype = completion.heap_reference().map_or_else(
                || {
                    let target_realm = runtime.function_realm(state.new_target)?;
                    runtime
                        .realm_error_intrinsic_prototype(target_realm, state.kind)
                        .map(HeapReference::Object)
                },
                Ok,
            )?;
            state.object = Some(runtime.allocate_error_with_prototype(prototype)?);
            begin_error_message(runtime, state, return_to, execution_budget)
        }
        ErrorConstructorStage::AwaitCause => {
            define_error_property(runtime, &state, PredefinedAtom::Cause, completion)?;
            finish_error_constructor(runtime, state, return_to, execution_budget)
        }
    }
}

fn begin_error_message(
    runtime: &mut Runtime,
    mut state: ErrorConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let message = std::mem::replace(&mut state.message, StoredValue::Undefined);
    if matches!(message, StoredValue::Undefined) {
        return begin_error_cause(runtime, state, return_to, execution_budget);
    }
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_operator_primitive_conversion(
        runtime,
        message,
        OperatorPrimitiveHint::String,
        OperatorPrimitiveTarget::ErrorConstructorMessage(state),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn finish_error_constructor_message(
    runtime: &mut Runtime,
    state: ErrorConstructorContinuation,
    message: JsString,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    define_error_property(
        runtime,
        &state,
        PredefinedAtom::Message,
        StoredValue::String(message),
    )?;
    begin_error_cause(runtime, state, return_to, execution_budget)
}

fn begin_error_cause(
    runtime: &mut Runtime,
    mut state: ErrorConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(options) = state.options.take() else {
        return finish_error_constructor(runtime, state, return_to, execution_budget);
    };
    let Some(reference) = options.heap_reference() else {
        return finish_error_constructor(runtime, state, return_to, execution_budget);
    };
    let key = runtime.predefined_property_key(PredefinedAtom::Cause);
    charge_heap_property_lookup(runtime, &options, execution_budget)?;
    if lookup_heap_property(runtime, Some(reference), &key)?.is_none() {
        return finish_error_constructor(runtime, state, return_to, execution_budget);
    }
    charge_heap_property_lookup(runtime, &options, execution_budget)?;
    state.stage = ErrorConstructorStage::AwaitCause;
    match read_heap_property_for_receiver(runtime, reference, options, &key)? {
        PropertyReadOutcome::Value(value) => {
            advance_error_constructor(runtime, state, value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            error_constructor_getter_call(state, function, receiver, return_to)
        }
        PropertyReadOutcome::Failed(_) => Err(EngineFault::RuntimeInvariant {
            message: "object-valued Error options produced a primitive property failure",
        }
        .into()),
    }
}

fn finish_error_constructor(
    runtime: &mut Runtime,
    mut state: ErrorConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let object = state.object.ok_or(EngineFault::RuntimeInvariant {
        message: "Error constructor completed without an allocated object",
    })?;
    if let Some(iterable) = state.aggregate_errors.take() {
        return begin_aggregate_error_collection(
            runtime,
            object,
            iterable,
            state.stack,
            state.realm,
            return_to,
            state.origin,
            execution_budget,
        );
    }
    let stack = render_error_stack(runtime, &state.stack)?;
    runtime.define_error_data_property(
        object,
        PredefinedAtom::Stack,
        StoredValue::String(stack),
    )?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn define_error_property(
    runtime: &mut Runtime,
    state: &ErrorConstructorContinuation,
    key: PredefinedAtom,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    let object = state.object.ok_or(EngineFault::RuntimeInvariant {
        message: "Error property definition preceded object allocation",
    })?;
    runtime.define_error_data_property(object, key, value)?;
    Ok(())
}

fn error_constructor_getter_call(
    state: ErrorConstructorContinuation,
    function: FunctionId,
    receiver: StoredValue,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let origin = state.origin.clone();
    native_getter_call(
        function,
        receiver,
        NativeContinuation::ErrorConstructor(state),
        return_to,
        origin,
    )
}

pub(super) fn begin_error_to_string(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if receiver.heap_reference().is_none() {
        return Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("not an object")?,
            },
            origin,
        }));
    }
    let state = ErrorToStringContinuation {
        receiver,
        name: None,
        realm,
        stage: ErrorToStringStage::AwaitName,
        origin,
    };
    read_error_to_string_property(
        runtime,
        state,
        PredefinedAtom::Name,
        return_to,
        execution_budget,
    )
}

pub(super) fn advance_error_to_string(
    runtime: &mut Runtime,
    mut state: ErrorToStringContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        ErrorToStringStage::AwaitName => {
            if matches!(completion, StoredValue::Undefined) {
                state.name = Some(JsString::from_utf8("Error")?);
                state.stage = ErrorToStringStage::AwaitMessage;
                return read_error_to_string_property(
                    runtime,
                    state,
                    PredefinedAtom::Message,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                completion,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::ErrorToStringName(state),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        ErrorToStringStage::AwaitMessage => {
            if matches!(completion, StoredValue::Undefined) {
                return finish_error_to_string(state, JsString::empty());
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                completion,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::ErrorToStringMessage(state),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

pub(super) fn finish_error_to_string_name(
    runtime: &mut Runtime,
    mut state: ErrorToStringContinuation,
    name: JsString,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.name = Some(name);
    state.stage = ErrorToStringStage::AwaitMessage;
    read_error_to_string_property(
        runtime,
        state,
        PredefinedAtom::Message,
        return_to,
        execution_budget,
    )
}

pub(super) fn finish_error_to_string_message(
    state: ErrorToStringContinuation,
    message: JsString,
) -> Result<NativeDispatch, NativeFailure> {
    finish_error_to_string(state, message)
}

fn finish_error_to_string(
    state: ErrorToStringContinuation,
    message: JsString,
) -> Result<NativeDispatch, NativeFailure> {
    let name = state.name.ok_or(EngineFault::RuntimeInvariant {
        message: "Error.prototype.toString completed without a name",
    })?;
    let rendered = if name.is_empty() {
        message
    } else if message.is_empty() {
        name
    } else {
        concat_error_strings(
            &name,
            &JsString::from_utf8(": ")?,
            &message,
            state.realm,
            &state.origin,
        )?
    };
    Ok(NativeDispatch::Immediate(StoredValue::String(rendered)))
}

fn read_error_to_string_property(
    runtime: &mut Runtime,
    state: ErrorToStringContinuation,
    key: PredefinedAtom,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let reference = state
        .receiver
        .heap_reference()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Error.prototype.toString receiver lost its heap reference",
        })?;
    charge_heap_property_lookup(runtime, &state.receiver, execution_budget)?;
    let key = runtime.predefined_property_key(key);
    match read_heap_property_for_receiver(runtime, reference, state.receiver.duplicate(), &key)? {
        PropertyReadOutcome::Value(value) => {
            advance_error_to_string(runtime, state, value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            let origin = state.origin.clone();
            native_getter_call(
                function,
                receiver,
                NativeContinuation::ErrorToString(state),
                return_to,
                origin,
            )
        }
        PropertyReadOutcome::Failed(_) => Err(EngineFault::RuntimeInvariant {
            message: "heap Error.prototype.toString receiver produced a primitive property failure",
        }
        .into()),
    }
}

fn native_getter_call(
    function: FunctionId,
    receiver: StoredValue,
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
        arguments: CallArguments::empty(),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

fn concat_error_strings(
    left: &JsString,
    separator: &JsString,
    right: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<JsString, NativeFailure> {
    let concat = left.concat(separator).and_then(|value| value.concat(right));
    match concat {
        Ok(value) => Ok(value),
        Err(JsStringError::TooLong { .. }) => Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::InternalError,
                message: JsString::from_utf8("string too long")?,
            },
            origin: origin.clone(),
        })),
        Err(error) => Err(error.into()),
    }
}
