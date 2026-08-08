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

//! Dynamic Function installation, constructor completion, and source rendering.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) fn completed_dynamic_function_source(
    arguments: Vec<StoredValue>,
    family: DynamicFunctionFamily,
) -> Result<OrdinaryDynamicFunctionSource, NativeFailure> {
    let mut converted = Vec::new();
    converted
        .try_reserve_exact(arguments.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: arguments.len(),
        })?;
    for argument in arguments {
        let StoredValue::String(argument) = argument else {
            return Err(EngineFault::RuntimeInvariant {
                message: "completed dynamic Function source retained a non-string argument",
            }
            .into());
        };
        converted.push(argument);
    }
    if converted.is_empty() {
        return Ok(OrdinaryDynamicFunctionSource::for_family(
            family,
            Arc::from([]),
            JsString::empty(),
        ));
    }
    let body = converted.pop().ok_or(EngineFault::RuntimeInvariant {
        message: "nonempty dynamic Function arguments lost their body",
    })?;
    Ok(OrdinaryDynamicFunctionSource::for_family(
        family,
        Arc::from(converted),
        body,
    ))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "verified compilation, installation, rollback, and frame admission form one failure-atomic boundary"
)]
pub(super) fn finish_dynamic_function_constructor(
    runtime: &mut Runtime,
    native: NativeFunction,
    construction: Option<FunctionId>,
    source: OrdinaryDynamicFunctionSource,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    active_frames: usize,
    active_frame_values: u64,
    compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget.charge_dynamic_compilation(&source)?;
    let family = source.family();
    let authority = match compiler.compile(source) {
        Ok(authority) => authority,
        Err(DynamicFunctionCompileFailure::Syntax { message }) => {
            return Err(NativeFailure::Abrupt(PendingException {
                realm: native.realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::SyntaxError,
                    message,
                },
                origin,
            }));
        }
        Err(error @ DynamicFunctionCompileFailure::Engine { .. }) => {
            return Err(NativeFailure::Execution(error.into()));
        }
    };

    let exception_authority = Arc::clone(&authority);
    let installation = {
        let mut context = Context {
            runtime,
            realm: native.realm,
        };
        context.install_dynamic_function_script_during_execution(authority)
    };
    let mut installed = match installation {
        Ok(installed) => installed,
        Err(crate::InstallError::GlobalDeclarationRejected {
            name,
            kind: _,
            function,
            pc,
            source_span,
        }) => {
            let (message, declaration_origin) =
                global_declaration_error(&exception_authority, &name, function, pc, source_span)
                    .map_err(NativeFailure::Execution)?;
            return Err(NativeFailure::Abrupt(PendingException {
                realm: native.realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::TypeError,
                    message,
                },
                origin: declaration_origin,
            }));
        }
        Err(error) => {
            return Err(NativeFailure::Execution(ExecutionError::from(error)));
        }
    };
    let dynamic_return_values = u64::from(construction.is_some());
    let plan = match plan_frame(
        runtime,
        installed.function,
        active_frames,
        active_frame_values.saturating_add(dynamic_return_values),
        0,
        false,
    ) {
        Ok(plan) => plan,
        Err(error) => {
            retire_failed_dynamic_root(runtime, installed)?;
            return Err(NativeFailure::Execution(error));
        }
    };
    let global = match runtime.realm_global_object(native.realm) {
        Ok(global) => global,
        Err(fault) => {
            retire_failed_dynamic_root(runtime, installed)?;
            return Err(NativeFailure::Execution(fault.into()));
        }
    };
    let frame = match create_frame(
        runtime,
        plan,
        StoredValue::Object(global),
        None,
        FrameArguments::Owned(CallArguments::empty()),
        return_to,
        None,
    ) {
        Ok(frame) => frame,
        Err(error) => {
            retire_failed_dynamic_root(runtime, installed)?;
            return Err(NativeFailure::Execution(error));
        }
    };
    if let Err(error) = installed.commit_environment() {
        retire_failed_dynamic_root(runtime, installed)?;
        return Err(NativeFailure::Execution(error.into()));
    }
    let mut frame = frame;
    frame.reserved_values = frame.reserved_values.saturating_add(dynamic_return_values);
    frame.dynamic_return = Some(DynamicFunctionReturn {
        root: installed,
        realm: native.realm,
        kind: DynamicRootKind::Function(family),
        construction,
        origin: Some(origin),
    });
    Ok(NativeDispatch::Frame(frame))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "eval compilation, installation, frame admission, and rollback form one audited boundary"
)]
pub(super) fn finish_indirect_eval(
    runtime: &mut Runtime,
    realm: RealmId,
    source: JsString,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    active_root_frames: &[Frame],
    active_frames: usize,
    active_frame_values: u64,
    compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget.charge_indirect_eval_compilation(&source)?;
    let authority = match compiler.compile_indirect_eval(IndirectEvalCompileRequest::new(source)) {
        Ok(authority) => authority,
        Err(DynamicFunctionCompileFailure::Syntax { message }) => {
            return Err(NativeFailure::Abrupt(PendingException {
                realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::SyntaxError,
                    message,
                },
                origin,
            }));
        }
        Err(error @ DynamicFunctionCompileFailure::Engine { .. }) => {
            return Err(NativeFailure::Execution(error.into()));
        }
    };

    let exception_authority = Arc::clone(&authority);
    let installation = {
        let mut context = Context { runtime, realm };
        context.install_indirect_eval_script_during_execution(authority)
    };
    let mut installed = match installation {
        Ok(installed) => installed,
        Err(crate::InstallError::GlobalDeclarationRejected {
            name,
            kind,
            function,
            pc,
            source_span,
        }) => {
            let (message, declaration_origin) =
                global_declaration_error(&exception_authority, &name, function, pc, source_span)
                    .map_err(NativeFailure::Execution)?;
            let snapshot = capture_error_stack(runtime, active_root_frames, &origin)
                .map_err(NativeFailure::Execution)?;
            let stack = render_error_stack(runtime, &snapshot).map_err(NativeFailure::Execution)?;
            let exception_kind = match kind {
                GlobalDeclarationRejectionKind::BindingConflict => ExceptionKind::SyntaxError,
                GlobalDeclarationRejectionKind::ObjectDefinitionRejected => {
                    ExceptionKind::TypeError
                }
            };
            return Err(NativeFailure::Abrupt(PendingException {
                realm,
                payload: PendingExceptionPayload::FrozenEngineError {
                    kind: exception_kind,
                    message,
                    stack,
                },
                origin: declaration_origin,
            }));
        }
        Err(error) => return Err(NativeFailure::Execution(error.into())),
    };
    let plan = match plan_frame(
        runtime,
        installed.function,
        active_frames,
        active_frame_values,
        0,
        false,
    ) {
        Ok(plan) => plan,
        Err(error) => {
            retire_failed_dynamic_root(runtime, installed)?;
            return Err(NativeFailure::Execution(error));
        }
    };
    let global = match runtime.realm_global_object(realm) {
        Ok(global) => global,
        Err(fault) => {
            retire_failed_dynamic_root(runtime, installed)?;
            return Err(NativeFailure::Execution(fault.into()));
        }
    };
    let mut frame = match create_frame(
        runtime,
        plan,
        StoredValue::Object(global),
        None,
        FrameArguments::Owned(CallArguments::empty()),
        return_to,
        None,
    ) {
        Ok(frame) => frame,
        Err(error) => {
            retire_failed_dynamic_root(runtime, installed)?;
            return Err(NativeFailure::Execution(error));
        }
    };
    if let Err(error) = installed.commit_environment() {
        retire_failed_dynamic_root(runtime, installed)?;
        return Err(NativeFailure::Execution(error.into()));
    }
    frame.dynamic_return = Some(DynamicFunctionReturn {
        root: installed,
        realm,
        kind: DynamicRootKind::IndirectEval,
        construction: None,
        origin: None,
    });
    Ok(NativeDispatch::Frame(frame))
}

#[allow(
    clippy::too_many_arguments,
    reason = "direct eval compilation and caller-frame admission form one audited boundary"
)]
pub(super) fn finish_direct_eval(
    runtime: &mut Runtime,
    caller: &mut Frame,
    realm: RealmId,
    request: DirectEvalCompileRequest,
    receiver: StoredValue,
    return_to: CallReturn,
    origin: JsStackFrame,
    active_frames: usize,
    active_frame_values: u64,
    compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    execution_budget: &mut ExecutionBudget,
) -> Result<Frame, NativeFailure> {
    execution_budget.charge_indirect_eval_compilation(request.source())?;
    let caller_bindings = request.shared_bindings();
    let authority = match compiler.compile_direct_eval(request) {
        Ok(authority) => authority,
        Err(DynamicFunctionCompileFailure::Syntax { message }) => {
            return Err(NativeFailure::Abrupt(PendingException {
                realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::SyntaxError,
                    message,
                },
                origin,
            }));
        }
        Err(error @ DynamicFunctionCompileFailure::Engine { .. }) => {
            return Err(NativeFailure::Execution(error.into()));
        }
    };
    let environment =
        materialize_direct_eval_environment(runtime, caller, &authority, &caller_bindings)
            .map_err(NativeFailure::Execution)?;

    let installation = {
        let mut context = Context { runtime, realm };
        context.install_direct_eval_script_during_execution(authority, environment.bindings())
    };
    let mut installed = match installation {
        Ok(installed) => installed,
        Err(error) => {
            rollback_direct_eval_environment(runtime, caller, environment)
                .map_err(|fault| NativeFailure::Execution(fault.into()))?;
            return Err(NativeFailure::Execution(error.into()));
        }
    };
    let plan = match plan_frame(
        runtime,
        installed.function,
        active_frames,
        active_frame_values,
        0,
        false,
    ) {
        Ok(plan) => plan,
        Err(error) => {
            let retirement = retire_failed_dynamic_root(runtime, installed);
            let rollback = rollback_direct_eval_environment(runtime, caller, environment);
            retirement?;
            rollback.map_err(|fault| NativeFailure::Execution(fault.into()))?;
            return Err(NativeFailure::Execution(error));
        }
    };
    let mut frame = match create_frame(
        runtime,
        plan,
        receiver,
        None,
        FrameArguments::Owned(CallArguments::empty()),
        Some(return_to),
        None,
    ) {
        Ok(frame) => frame,
        Err(error) => {
            let retirement = retire_failed_dynamic_root(runtime, installed);
            let rollback = rollback_direct_eval_environment(runtime, caller, environment);
            retirement?;
            rollback.map_err(|fault| NativeFailure::Execution(fault.into()))?;
            return Err(NativeFailure::Execution(error));
        }
    };
    if let Err(error) = installed.commit_environment() {
        let retirement = retire_failed_dynamic_root(runtime, installed);
        let rollback = rollback_direct_eval_environment(runtime, caller, environment);
        retirement?;
        rollback.map_err(|fault| NativeFailure::Execution(fault.into()))?;
        return Err(NativeFailure::Execution(error.into()));
    }
    frame.dynamic_return = Some(DynamicFunctionReturn {
        root: installed,
        realm,
        kind: DynamicRootKind::IndirectEval,
        construction: None,
        origin: None,
    });
    Ok(frame)
}

#[allow(
    clippy::too_many_lines,
    reason = "one resumable Object.prototype.toString entry keeps every primitive wrapper and branded default-tag branch in specification order"
)]
pub(super) fn begin_object_prototype_to_string(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (reference, default_tag) = match &receiver {
        StoredValue::Undefined => {
            let tag = JsString::from_utf8("Undefined")?;
            return format_object_prototype_to_string(&tag);
        }
        StoredValue::Null => {
            let tag = JsString::from_utf8("Null")?;
            return format_object_prototype_to_string(&tag);
        }
        StoredValue::Boolean(value) => {
            return begin_boxed_boolean_object_prototype_to_string(
                runtime, realm, *value, return_to, origin,
            );
        }
        StoredValue::Number(value) => {
            return begin_boxed_number_object_prototype_to_string(
                runtime, realm, *value, return_to, origin,
            );
        }
        StoredValue::BigInt(value) => {
            return begin_boxed_bigint_object_prototype_to_string(
                runtime,
                realm,
                Arc::clone(value),
                return_to,
                origin,
            );
        }
        StoredValue::String(value) => {
            return begin_boxed_string_object_prototype_to_string(
                runtime,
                realm,
                value.clone(),
                return_to,
                origin,
            );
        }
        StoredValue::Function(function) => (
            HeapReference::Function(*function),
            ObjectPrototypeTag::Function,
        ),
        StoredValue::Object(object) => (
            HeapReference::Object(*object),
            if runtime.is_arguments_object(*object)? {
                ObjectPrototypeTag::Arguments
            } else if proxy_aware_is_array(
                runtime,
                receiver.duplicate(),
                realm,
                origin.clone().unwrap_or_else(native_function_host_origin),
            )? {
                ObjectPrototypeTag::Array
            } else if runtime.boxed_boolean(*object)?.is_some() {
                ObjectPrototypeTag::Boolean
            } else if runtime.boxed_number(*object)?.is_some() {
                ObjectPrototypeTag::Number
            } else if runtime.boxed_bigint(*object)?.is_some() {
                ObjectPrototypeTag::BigInt
            } else if runtime.boxed_string(*object)?.is_some() {
                ObjectPrototypeTag::String
            } else if runtime.boxed_symbol(*object)?.is_some() {
                ObjectPrototypeTag::Symbol
            } else if runtime.date_value(*object)?.is_some() {
                ObjectPrototypeTag::Date
            } else if let Some(state) = runtime.array_buffer_state(*object)? {
                if state.is_shared() {
                    ObjectPrototypeTag::SharedArrayBuffer
                } else {
                    ObjectPrototypeTag::ArrayBuffer
                }
            } else if runtime
                .objects
                .get(*object)
                .ok_or(EngineFault::StaleHeapEdge {
                    edge: "object",
                    index: object.index(),
                    generation: object.generation(),
                })?
                .is_error()
            {
                ObjectPrototypeTag::Error
            } else if runtime
                .objects
                .get(*object)
                .ok_or(EngineFault::StaleHeapEdge {
                    edge: "object",
                    index: object.index(),
                    generation: object.generation(),
                })?
                .is_promise()
            {
                ObjectPrototypeTag::Promise
            } else {
                ObjectPrototypeTag::Object
            },
        ),
        StoredValue::Symbol(value) => {
            return begin_boxed_symbol_object_prototype_to_string(
                runtime,
                realm,
                value.clone(),
                return_to,
                origin,
            );
        }
    };
    let to_string_tag = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag);
    begin_intrinsic_get(
        runtime,
        realm,
        reference,
        receiver,
        &to_string_tag,
        IntrinsicGetContinuation::ObjectPrototypeToString {
            default_tag,
            temporary_receiver: None,
        },
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_boxed_boolean_object_prototype_to_string(
    runtime: &mut Runtime,
    realm: RealmId,
    value: bool,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let continuations = reserve_intrinsic_get_continuation()?;
    let to_string_tag = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag);
    let collection_pending = runtime.collection_pending;
    let temporary = runtime.allocate_boxed_boolean(realm, value)?;
    let receiver = StoredValue::Object(temporary);
    let continuation = IntrinsicGetContinuation::ObjectPrototypeToString {
        default_tag: ObjectPrototypeTag::Boolean,
        temporary_receiver: Some(temporary),
    };
    let outcome = match read_heap_property_for_receiver(
        runtime,
        HeapReference::Object(temporary),
        receiver,
        &to_string_tag,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            return Err(error.into());
        }
    };
    match outcome {
        PropertyReadOutcome::Value(tag) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            finish_object_prototype_to_string(ObjectPrototypeTag::Boolean, tag)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            Ok(intrinsic_getter_call_with_reserved_continuation(
                function,
                receiver,
                continuation,
                return_to,
                origin,
                continuations,
            ))
        }
        PropertyReadOutcome::Failed(_) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            Err(EngineFault::RuntimeInvariant {
                message: "Boolean boxing intrinsic Get produced a primitive property failure",
            }
            .into())
        }
    }
}

/// Tags an `Object(bigint)` through a throwaway wrapper.
///
/// The wrapper exists only so a user-supplied `Symbol.toStringTag` on
/// `BigInt.prototype` is still consulted; it is removed again before the result
/// becomes observable.
fn begin_boxed_bigint_object_prototype_to_string(
    runtime: &mut Runtime,
    realm: RealmId,
    value: Arc<JsBigInt>,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let continuations = reserve_intrinsic_get_continuation()?;
    let to_string_tag = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag);
    let collection_pending = runtime.collection_pending;
    let temporary = runtime.allocate_boxed_bigint(realm, value)?;
    let receiver = StoredValue::Object(temporary);
    let continuation = IntrinsicGetContinuation::ObjectPrototypeToString {
        default_tag: ObjectPrototypeTag::BigInt,
        temporary_receiver: Some(temporary),
    };
    let outcome = match read_heap_property_for_receiver(
        runtime,
        HeapReference::Object(temporary),
        receiver,
        &to_string_tag,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            return Err(error.into());
        }
    };
    match outcome {
        PropertyReadOutcome::Value(tag) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            finish_object_prototype_to_string(ObjectPrototypeTag::BigInt, tag)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            Ok(intrinsic_getter_call_with_reserved_continuation(
                function,
                receiver,
                continuation,
                return_to,
                origin,
                continuations,
            ))
        }
        PropertyReadOutcome::Failed(_) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            Err(EngineFault::RuntimeInvariant {
                message: "BigInt boxing intrinsic Get produced a primitive property failure",
            }
            .into())
        }
    }
}

fn begin_boxed_number_object_prototype_to_string(
    runtime: &mut Runtime,
    realm: RealmId,
    value: JsNumber,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let continuations = reserve_intrinsic_get_continuation()?;
    let to_string_tag = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag);
    let collection_pending = runtime.collection_pending;
    let temporary = runtime.allocate_boxed_number(realm, value)?;
    let receiver = StoredValue::Object(temporary);
    let continuation = IntrinsicGetContinuation::ObjectPrototypeToString {
        default_tag: ObjectPrototypeTag::Number,
        temporary_receiver: Some(temporary),
    };
    let outcome = match read_heap_property_for_receiver(
        runtime,
        HeapReference::Object(temporary),
        receiver,
        &to_string_tag,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            return Err(error.into());
        }
    };
    match outcome {
        PropertyReadOutcome::Value(tag) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            finish_object_prototype_to_string(ObjectPrototypeTag::Number, tag)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            Ok(intrinsic_getter_call_with_reserved_continuation(
                function,
                receiver,
                continuation,
                return_to,
                origin,
                continuations,
            ))
        }
        PropertyReadOutcome::Failed(_) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            Err(EngineFault::RuntimeInvariant {
                message: "Number boxing intrinsic Get produced a primitive property failure",
            }
            .into())
        }
    }
}

fn begin_boxed_string_object_prototype_to_string(
    runtime: &mut Runtime,
    realm: RealmId,
    value: JsString,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let continuations = reserve_intrinsic_get_continuation()?;
    let to_string_tag = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag);
    let collection_pending = runtime.collection_pending;
    let temporary = runtime.allocate_boxed_string(realm, value)?;
    let receiver = StoredValue::Object(temporary);
    let continuation = IntrinsicGetContinuation::ObjectPrototypeToString {
        default_tag: ObjectPrototypeTag::String,
        temporary_receiver: Some(temporary),
    };
    let outcome = match read_heap_property_for_receiver(
        runtime,
        HeapReference::Object(temporary),
        receiver,
        &to_string_tag,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            return Err(error.into());
        }
    };
    match outcome {
        PropertyReadOutcome::Value(tag) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            finish_object_prototype_to_string(ObjectPrototypeTag::String, tag)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            Ok(intrinsic_getter_call_with_reserved_continuation(
                function,
                receiver,
                continuation,
                return_to,
                origin,
                continuations,
            ))
        }
        PropertyReadOutcome::Failed(_) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            Err(EngineFault::RuntimeInvariant {
                message: "String boxing intrinsic Get produced a primitive property failure",
            }
            .into())
        }
    }
}

fn begin_boxed_symbol_object_prototype_to_string(
    runtime: &mut Runtime,
    realm: RealmId,
    value: crate::Atom,
    return_to: Option<CallReturn>,
    origin: Option<JsStackFrame>,
) -> Result<NativeDispatch, NativeFailure> {
    let continuations = reserve_intrinsic_get_continuation()?;
    let to_string_tag = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag);
    let collection_pending = runtime.collection_pending;
    let temporary = runtime.allocate_boxed_symbol(realm, value)?;
    let receiver = StoredValue::Object(temporary);
    let continuation = IntrinsicGetContinuation::ObjectPrototypeToString {
        default_tag: ObjectPrototypeTag::Symbol,
        temporary_receiver: Some(temporary),
    };
    let outcome = match read_heap_property_for_receiver(
        runtime,
        HeapReference::Object(temporary),
        receiver,
        &to_string_tag,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            return Err(error.into());
        }
    };
    match outcome {
        PropertyReadOutcome::Value(tag) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            finish_object_prototype_to_string(ObjectPrototypeTag::Symbol, tag)
        }
        PropertyReadOutcome::Getter { function, receiver } => {
            Ok(intrinsic_getter_call_with_reserved_continuation(
                function,
                receiver,
                continuation,
                return_to,
                origin,
                continuations,
            ))
        }
        PropertyReadOutcome::Failed(_) => {
            remove_unobservable_temporary_wrapper(runtime, temporary, collection_pending);
            Err(EngineFault::RuntimeInvariant {
                message: "Symbol boxing intrinsic Get produced a primitive property failure",
            }
            .into())
        }
    }
}

fn remove_unobservable_temporary_wrapper(
    runtime: &mut Runtime,
    temporary: ObjectId,
    collection_pending: bool,
) {
    let removed = runtime.objects.remove(temporary);
    if let Some(object) = removed {
        runtime.object_properties = runtime
            .object_properties
            .saturating_sub(usize_to_u64(object.record.property_count()));
    }
    runtime.collection_pending = collection_pending;
}

pub(super) fn finish_object_prototype_to_string(
    default_tag: ObjectPrototypeTag,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let tag = match value {
        StoredValue::String(tag) => tag,
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::Symbol(_)
        | StoredValue::Function(_)
        | StoredValue::Object(_) => JsString::from_utf8(default_tag.name())?,
    };
    format_object_prototype_to_string(&tag)
}

fn format_object_prototype_to_string(tag: &JsString) -> Result<NativeDispatch, NativeFailure> {
    let value = JsString::from_utf8("[object ")?
        .concat(tag)?
        .concat(&JsString::from_utf8("]")?)?;
    Ok(NativeDispatch::Immediate(StoredValue::String(value)))
}

pub(super) fn function_to_string(
    runtime: &Runtime,
    function: FunctionId,
    realm: RealmId,
    origin: Option<&JsStackFrame>,
) -> Result<JsString, NativeFailure> {
    let node = runtime
        .functions
        .get(function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "function",
            index: function.index(),
            generation: function.generation(),
        })?;
    if let FunctionImplementation::Bytecode(bytecode) = &node.implementation {
        let installed = code(runtime, bytecode.code)?;
        let function = installed.authority.function(bytecode.template).ok_or(
            EngineFault::InvalidClosureEnvironment {
                function: bytecode.template,
            },
        )?;
        return Ok(JsString::from_utf8(
            function.metadata().source().function_source(),
        )?);
    }

    let name_key = runtime.predefined_property_key(PredefinedAtom::Name);
    let name = native_function_name_to_string(
        read_heap_property(runtime, HeapReference::Function(function), &name_key)?,
        realm,
        origin,
    )?;
    Ok(JsString::from_utf8("function ")?
        .concat(&name)?
        .concat(&JsString::from_utf8("() {\n    [native code]\n}")?)?)
}

fn native_function_name_to_string(
    value: StoredValue,
    realm: RealmId,
    origin: Option<&JsStackFrame>,
) -> Result<JsString, NativeFailure> {
    match value {
        StoredValue::Undefined => Ok(JsString::empty()),
        StoredValue::Null => Ok(JsString::from_utf8("null")?),
        StoredValue::Boolean(false) => Ok(JsString::from_utf8("false")?),
        StoredValue::Boolean(true) => Ok(JsString::from_utf8("true")?),
        StoredValue::Number(value) => Ok(value.to_javascript_string()?),
        StoredValue::BigInt(value) => Ok(bigint_decimal_string(&value)?),
        StoredValue::String(value) => Ok(value),
        StoredValue::Symbol(_) => {
            let Some(origin) = origin else {
                return Err(NativeFailure::Execution(
                    EngineFault::RuntimeInvariant {
                        message: "host Symbol-to-string error has no source origin",
                    }
                    .into(),
                ));
            };
            Err(NativeFailure::Abrupt(PendingException {
                realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::TypeError,
                    message: JsString::from_utf8("cannot convert symbol to string")?,
                },
                origin: origin.clone(),
            }))
        }
        StoredValue::Function(_) | StoredValue::Object(_) => Err(NativeFailure::Execution(
            EngineFault::RuntimeInvariant {
                message: "native function name ToPrimitive is not implemented",
            }
            .into(),
        )),
    }
}

pub(super) fn native_function_host_origin() -> JsStackFrame {
    dynamic_function_host_origin(NativeFunctionKind::OrdinaryFunctionConstructor)
}

pub(super) fn dynamic_function_host_origin(kind: NativeFunctionKind) -> JsStackFrame {
    let (source_name, function_name, end) = match kind {
        NativeFunctionKind::OrdinaryFunctionConstructor => ("<native Function>", "Function", 8),
        NativeFunctionKind::GeneratorFunctionConstructor => {
            ("<native GeneratorFunction>", "GeneratorFunction", 17)
        }
        NativeFunctionKind::AsyncFunctionConstructor => {
            ("<native AsyncFunction>", "AsyncFunction", 13)
        }
        NativeFunctionKind::AsyncGeneratorFunctionConstructor => (
            "<native AsyncGeneratorFunction>",
            "AsyncGeneratorFunction",
            22,
        ),
        _ => ("<native dynamic function>", "dynamic function", 16),
    };
    JsStackFrame::new(
        FunctionTemplateId::new(0),
        BytecodePc::ZERO,
        Arc::from(source_name),
        Arc::from(function_name),
        SourceByteSpan::new(0, end),
    )
}

fn retire_failed_dynamic_root(
    runtime: &mut Runtime,
    installed: InstalledRoot,
) -> Result<(), NativeFailure> {
    runtime
        .retire_dynamic_root(installed)
        .map_err(|fault| NativeFailure::Execution(fault.into()))
}

pub(super) fn dynamic_function_source_code_units(source: &OrdinaryDynamicFunctionSource) -> u64 {
    const FUNCTION_WRAPPER_CODE_UNITS: u64 = 28;
    const GENERATOR_WRAPPER_CODE_UNITS: u64 = 29;
    const ASYNC_WRAPPER_CODE_UNITS: u64 = 34;
    const ASYNC_GENERATOR_WRAPPER_CODE_UNITS: u64 = 35;
    let wrapper_units = match source.family() {
        DynamicFunctionFamily::Function => FUNCTION_WRAPPER_CODE_UNITS,
        DynamicFunctionFamily::GeneratorFunction => GENERATOR_WRAPPER_CODE_UNITS,
        DynamicFunctionFamily::AsyncFunction => ASYNC_WRAPPER_CODE_UNITS,
        DynamicFunctionFamily::AsyncGeneratorFunction => ASYNC_GENERATOR_WRAPPER_CODE_UNITS,
    };
    let parameter_units = source.parameters().iter().fold(0_u64, |total, parameter| {
        total.saturating_add(u64::from(parameter.len()))
    });
    let separator_units = usize_to_u64(source.parameters().len().saturating_sub(1));
    wrapper_units
        .saturating_add(parameter_units)
        .saturating_add(separator_units)
        .saturating_add(u64::from(source.body().len()))
}

pub(super) fn dynamic_source_primitive_to_string(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<JsString, NativeFailure> {
    match value {
        StoredValue::Undefined => Ok(JsString::from_utf8("undefined")?),
        StoredValue::Null => Ok(JsString::from_utf8("null")?),
        StoredValue::Boolean(false) => Ok(JsString::from_utf8("false")?),
        StoredValue::Boolean(true) => Ok(JsString::from_utf8("true")?),
        StoredValue::Number(value) => Ok(value.to_javascript_string()?),
        StoredValue::BigInt(value) => Ok(bigint_decimal_string(&value)?),
        StoredValue::String(value) => Ok(value),
        StoredValue::Symbol(_) => Err(NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8("cannot convert symbol to string")?,
            },
            origin: origin.clone(),
        })),
        StoredValue::Function(_) | StoredValue::Object(_) => Err(EngineFault::RuntimeInvariant {
            message: "object reached primitive dynamic Function source conversion",
        }
        .into()),
    }
}

pub(super) fn finish_dynamic_function_return(
    runtime: &mut Runtime,
    dynamic: DynamicFunctionReturn,
    value: StoredValue,
) -> Result<DynamicFunctionCompletion, ExecutionError> {
    let completion = match (dynamic.kind, dynamic.construction) {
        (DynamicRootKind::Function(family), Some(new_target)) => {
            apply_dynamic_constructor_prototype(runtime, new_target, family, value)
        }
        (DynamicRootKind::Function(_) | DynamicRootKind::IndirectEval, None) => Ok(value),
        (DynamicRootKind::IndirectEval, Some(_)) => Err(ConstructorCompletionError::Execution(
            EngineFault::RuntimeInvariant {
                message: "indirect eval root retained a construction target",
            }
            .into(),
        )),
    };
    let retirement = runtime.retire_dynamic_root(dynamic.root);
    retirement?;
    match completion {
        Ok(value) => Ok(DynamicFunctionCompletion::Value(value)),
        Err(ConstructorCompletionError::Execution(error)) => Err(error),
        Err(ConstructorCompletionError::TypeError(message)) => {
            let Some(origin) = dynamic.origin else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "host dynamic construction has no verified exception origin",
                }
                .into());
            };
            Ok(DynamicFunctionCompletion::Abrupt(PendingException {
                realm: dynamic.realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::TypeError,
                    message,
                },
                origin,
            }))
        }
    }
}

pub(super) enum DynamicFunctionCompletion {
    Value(StoredValue),
    Abrupt(PendingException),
}

enum ConstructorCompletionError {
    TypeError(JsString),
    Execution(ExecutionError),
}

impl From<ExecutionError> for ConstructorCompletionError {
    fn from(error: ExecutionError) -> Self {
        Self::Execution(error)
    }
}

impl From<EngineFault> for ConstructorCompletionError {
    fn from(error: EngineFault) -> Self {
        Self::Execution(error.into())
    }
}

impl From<JsStringError> for ConstructorCompletionError {
    fn from(error: JsStringError) -> Self {
        Self::Execution(error.into())
    }
}

fn apply_dynamic_constructor_prototype(
    runtime: &mut Runtime,
    new_target: FunctionId,
    family: DynamicFunctionFamily,
    completion: StoredValue,
) -> Result<StoredValue, ConstructorCompletionError> {
    let target = match &completion {
        StoredValue::Undefined | StoredValue::Null => {
            return Err(ConstructorCompletionError::TypeError(JsString::from_utf8(
                "not an object",
            )?));
        }
        StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            return Ok(completion);
        }
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
    };
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let requested =
        read_heap_property(runtime, HeapReference::Function(new_target), &prototype_key)?;
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(function),
        StoredValue::Object(object) => HeapReference::Object(object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let realm = runtime.function_realm(new_target)?;
            match family {
                DynamicFunctionFamily::Function => {
                    HeapReference::Function(runtime.realm_function_prototype(realm)?)
                }
                DynamicFunctionFamily::GeneratorFunction => {
                    HeapReference::Object(runtime.realm_generator_function_prototype(realm)?)
                }
                DynamicFunctionFamily::AsyncFunction => {
                    HeapReference::Object(runtime.realm_async_function_prototype(realm)?)
                }
                DynamicFunctionFamily::AsyncGeneratorFunction => {
                    HeapReference::Object(runtime.realm_async_generator_function_prototype(realm)?)
                }
            }
        }
    };
    if !runtime.replace_prototype_checked(target, Some(prototype))? {
        return Err(ConstructorCompletionError::TypeError(JsString::from_utf8(
            "circular prototype chain",
        )?));
    }
    Ok(completion)
}

pub(super) fn is_canonical_realm_eval(
    runtime: &Runtime,
    function: FunctionId,
    realm: RealmId,
) -> Result<bool, EngineFault> {
    let node = runtime
        .functions
        .get(function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "function",
            index: function.index(),
            generation: function.generation(),
        })?;
    Ok(matches!(
        node.native(),
        Some(native) if native.kind == NativeFunctionKind::Eval && native.realm == realm
    ))
}

pub(super) fn direct_eval_compile_request(
    runtime: &Runtime,
    frame: &Frame,
    source: JsString,
    scope_index: u16,
) -> Result<DirectEvalCompileRequest, ExecutionError> {
    let installed_code = code(runtime, frame.code)?;
    let template = installed_code.authority.function(frame.template).ok_or(
        EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        },
    )?;
    let installed_index = usize::try_from(frame.template.get()).map_err(|_| {
        EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        }
    })?;
    let installed = installed_code.templates.get(installed_index).ok_or(
        EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        },
    )?;
    let bindings = direct_eval_caller_bindings(&template, installed, frame.template, scope_index)?;
    let function_code = runtime
        .functions
        .get(frame.function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "function",
            index: frame.function.index(),
            generation: frame.function.generation(),
        })?
        .bytecode()?;
    let kind = template.metadata().executable_kind();
    let function_context = !matches!(
        kind,
        CompilerExecutableKind::GlobalScript
            | CompilerExecutableKind::IndirectEvalScript
            | CompilerExecutableKind::DirectEvalScript
            | CompilerExecutableKind::DynamicFunctionScript
    );
    Ok(DirectEvalCompileRequest::new(source, frame.strict)
        .with_bindings(bindings.into())
        .with_scope_index(scope_index)
        .with_new_target(function_context)
        .with_super_property(function_code.home_object.is_some())
        .with_super_call(matches!(
            frame.constructor_state,
            ConstructorState::DerivedUninitialized | ConstructorState::DerivedInitialized
        ))
        .with_arguments_allowed(function_context))
}

#[allow(
    clippy::too_many_lines,
    reason = "the adjusted lexical chain, function body, and inherited closures share one ordered caller snapshot"
)]
fn direct_eval_caller_bindings(
    template: &VerifiedBytecodeFunction,
    installed: &InstalledTemplate,
    function: FunctionTemplateId,
    scope_index: u16,
) -> Result<Vec<DirectEvalCallerBinding>, ExecutionError> {
    let domains = template.function().control_flow().domains();
    let argument_count = domains.argument_count();
    let local_count = domains.local_count();
    let variables = template.metadata().variables();
    let closures = template.metadata().closures();
    let capacity = usize::try_from(argument_count)
        .ok()
        .and_then(|arguments| {
            usize::try_from(local_count)
                .ok()
                .and_then(|locals| arguments.checked_add(locals))
        })
        .and_then(|variables| variables.checked_add(closures.len()))
        .ok_or(ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: usize::MAX,
        })?;
    let mut bindings = Vec::new();
    bindings
        .try_reserve_exact(capacity)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: capacity,
        })?;

    let mut is_argument_scope = scope_index == 0;
    let mut lexical_local = (scope_index >= 2).then(|| u32::from(scope_index - 2));
    while let Some(local) = lexical_local {
        if local >= local_count {
            return Err(EngineFault::InvalidClosureEnvironment { function }.into());
        }
        let variable = variables
            .get(argument_count.saturating_add(local) as usize)
            .ok_or(EngineFault::InvalidClosureEnvironment { function })?;
        if !variable.has_scope() {
            return Err(EngineFault::InvalidClosureEnvironment { function }.into());
        }
        push_direct_eval_caller_binding(
            &mut bindings,
            installed,
            variable.name(),
            variable.policy(),
            DirectEvalCallerBindingLocation::Local(local),
        )?;
        lexical_local = match variable.scope_next() {
            ScopeLink::Local(parent) => Some(parent),
            ScopeLink::End => None,
            ScopeLink::ArgumentScopeEnd => {
                is_argument_scope = true;
                None
            }
        };
    }

    if is_argument_scope {
        for local in 0..local_count {
            let variable = variables
                .get(argument_count.saturating_add(local) as usize)
                .ok_or(EngineFault::InvalidClosureEnvironment { function })?;
            if !variable.has_scope()
                && variable.policy().kind() == CompilerBindingKind::FunctionName
            {
                push_direct_eval_caller_binding(
                    &mut bindings,
                    installed,
                    variable.name(),
                    variable.policy(),
                    DirectEvalCallerBindingLocation::Local(local),
                )?;
            }
        }
    } else {
        for argument in 0..argument_count {
            let variable = variables
                .get(argument as usize)
                .ok_or(EngineFault::InvalidClosureEnvironment { function })?;
            push_direct_eval_caller_binding(
                &mut bindings,
                installed,
                variable.name(),
                variable.policy(),
                DirectEvalCallerBindingLocation::Argument(argument),
            )?;
        }
        for local in 0..local_count {
            let variable = variables
                .get(argument_count.saturating_add(local) as usize)
                .ok_or(EngineFault::InvalidClosureEnvironment { function })?;
            if !variable.has_scope() {
                push_direct_eval_caller_binding(
                    &mut bindings,
                    installed,
                    variable.name(),
                    variable.policy(),
                    DirectEvalCallerBindingLocation::Local(local),
                )?;
            }
        }
    }

    for (closure, definition) in closures.iter().enumerate() {
        if !matches!(definition.binding(), CompilerClosureBinding::Captured(_)) {
            continue;
        }
        let closure = u32::try_from(closure)
            .map_err(|_| EngineFault::InvalidClosureEnvironment { function })?;
        push_direct_eval_caller_binding(
            &mut bindings,
            installed,
            definition.name(),
            definition.policy(),
            DirectEvalCallerBindingLocation::Closure(closure),
        )?;
    }
    Ok(bindings)
}

fn push_direct_eval_caller_binding(
    bindings: &mut Vec<DirectEvalCallerBinding>,
    installed: &InstalledTemplate,
    name: Option<quickjs_bytecode::AtomPoolIndex>,
    policy: quickjs_bytecode::CompilerBindingPolicy,
    location: DirectEvalCallerBindingLocation,
) -> Result<(), ExecutionError> {
    if matches!(
        policy.kind(),
        CompilerBindingKind::ClassFieldKey
            | CompilerBindingKind::ClassPrivateName
            | CompilerBindingKind::ClassStaticReceiver
            | CompilerBindingKind::GlobalReference
    ) {
        return Ok(());
    }
    let Some(name) = name else {
        return Ok(());
    };
    let atom = installed
        .atoms
        .get(name.get() as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "direct-eval caller atom",
            index: name.get(),
        })?;
    let name = atom
        .description()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "direct-eval caller binding has no string atom",
        })?
        .clone();
    if name.code_units().eq("_ret_".encode_utf16()) {
        return Ok(());
    }
    bindings.push(DirectEvalCallerBinding::new(name, policy, location));
    Ok(())
}

pub(super) fn function_is_constructor(
    runtime: &Runtime,
    mut function: FunctionId,
) -> Result<bool, ExecutionError> {
    let mut remaining = runtime.functions.len().saturating_add(1);
    loop {
        let node = runtime
            .functions
            .get(function)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "function",
                index: function.index(),
                generation: function.generation(),
            })?;
        match &node.implementation {
            FunctionImplementation::Bytecode(bytecode) => {
                let template = code(runtime, bytecode.code)?
                    .authority
                    .function(bytecode.template)
                    .ok_or(EngineFault::InvalidClosureEnvironment {
                        function: bytecode.template,
                    })?;
                return Ok(template.metadata().executable_kind()
                    == CompilerExecutableKind::ClassConstructor
                    || template
                        .function()
                        .control_flow()
                        .function_header()
                        .flags()
                        .has_prototype());
            }
            FunctionImplementation::Native(native) => return Ok(native.kind.is_constructor()),
            FunctionImplementation::PromiseResolving(_)
            | FunctionImplementation::PromiseCapabilityExecutor(_)
            | FunctionImplementation::PromiseFinally(_)
            | FunctionImplementation::PromiseCombinatorElement(_)
            | FunctionImplementation::ProxyRevoker(_) => return Ok(false),
            FunctionImplementation::Proxy(proxy) => return Ok(proxy.constructable),
            FunctionImplementation::Bound(bound) => {
                if remaining == 0 {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "bound-function target chain exceeds the heap size",
                    }
                    .into());
                }
                remaining -= 1;
                function = bound.target;
            }
        }
    }
}

pub(super) fn bytecode_function_is_constructor(
    runtime: &Runtime,
    function: FunctionId,
) -> Result<bool, ExecutionError> {
    let bytecode = runtime
        .functions
        .get(function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "function",
            index: function.index(),
            generation: function.generation(),
        })?
        .bytecode()?;
    let template = code(runtime, bytecode.code)?
        .authority
        .function(bytecode.template)
        .ok_or(EngineFault::InvalidClosureEnvironment {
            function: bytecode.template,
        })?;
    Ok(
        template.metadata().executable_kind() == CompilerExecutableKind::ClassConstructor
            || template
                .function()
                .control_flow()
                .function_header()
                .flags()
                .has_prototype(),
    )
}

pub(super) fn bytecode_function_is_class_constructor(
    runtime: &Runtime,
    function: FunctionId,
) -> Result<bool, ExecutionError> {
    let bytecode = runtime
        .functions
        .get(function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "function",
            index: function.index(),
            generation: function.generation(),
        })?
        .bytecode()?;
    let template = code(runtime, bytecode.code)?
        .authority
        .function(bytecode.template)
        .ok_or(EngineFault::InvalidClosureEnvironment {
            function: bytecode.template,
        })?;
    Ok(template.metadata().executable_kind() == CompilerExecutableKind::ClassConstructor)
}

pub(super) fn create_ordinary_constructor_receiver(
    runtime: &mut Runtime,
    new_target: FunctionId,
) -> Result<ObjectId, ExecutionError> {
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let requested =
        read_heap_property(runtime, HeapReference::Function(new_target), &prototype_key)?;
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(function),
        StoredValue::Object(object) => HeapReference::Object(object),
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::String(_)
        | StoredValue::Symbol(_) => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_object_prototype(realm)?)
        }
    };
    runtime.allocate_ordinary_object_with_prototype(prototype)
}

pub(super) fn retire_active_dynamic_roots(
    runtime: &mut Runtime,
    frames: &mut [Frame],
) -> Result<(), EngineFault> {
    let mut first_failure = None;
    for dynamic in frames
        .iter_mut()
        .rev()
        .filter_map(|frame| frame.dynamic_return.take())
    {
        if let Err(fault) = runtime.retire_dynamic_root(dynamic.root)
            && first_failure.is_none()
        {
            first_failure = Some(fault);
        }
    }
    first_failure.map_or(Ok(()), Err)
}
