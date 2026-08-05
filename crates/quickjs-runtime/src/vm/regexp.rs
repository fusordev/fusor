/*
 * JavaScript RegExp semantics derived from QuickJS.
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

//! Resumable `RegExp` construction, accessors, and builtin execution.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

const CANONICAL_FLAG_ACCESSORS: [RegExpFlag; 8] = [
    RegExpFlag::HasIndices,
    RegExpFlag::Global,
    RegExpFlag::IgnoreCase,
    RegExpFlag::Multiline,
    RegExpFlag::DotAll,
    RegExpFlag::Unicode,
    RegExpFlag::UnicodeSets,
    RegExpFlag::Sticky,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::enum_variant_names,
    reason = "the Await prefix makes every observable constructor boundary explicit"
)]
enum RegExpConstructorStage {
    AwaitMatch,
    AwaitConstructor,
    AwaitSource,
    AwaitFlags,
    AwaitPrototype,
    AwaitPatternConversion,
    AwaitFlagsConversion,
}

pub(super) struct RegExpConstructorContinuation {
    function: FunctionId,
    realm: RealmId,
    new_target: FunctionId,
    called: bool,
    pattern: StoredValue,
    flags: StoredValue,
    pattern_is_regexp: bool,
    pattern_is_branded: bool,
    pattern_value: Option<StoredValue>,
    flags_value: Option<StoredValue>,
    prototype: Option<HeapReference>,
    source: Option<JsString>,
    original_flags: Option<JsString>,
    stage: RegExpConstructorStage,
    origin: JsStackFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::enum_variant_names,
    reason = "the Await prefix distinguishes property reads from their following conversions"
)]
enum RegExpToStringStage {
    AwaitSource,
    AwaitSourceConversion,
    AwaitFlags,
    AwaitFlagsConversion,
}

pub(super) struct RegExpToStringContinuation {
    receiver: StoredValue,
    source: Option<JsString>,
    realm: RealmId,
    stage: RegExpToStringStage,
    origin: JsStackFrame,
}

pub(super) struct RegExpFlagsContinuation {
    receiver: StoredValue,
    next: usize,
    result: JsString,
    realm: RealmId,
    origin: JsStackFrame,
}

pub(super) struct RegExpEscapeContinuation {
    realm: RealmId,
    origin: JsStackFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RegExpCompileStage {
    AwaitPatternConversion,
    AwaitFlagsConversion,
}

pub(super) struct RegExpCompileContinuation {
    object: ObjectId,
    pattern: StoredValue,
    flags: StoredValue,
    source: Option<JsString>,
    original_flags: Option<JsString>,
    realm: RealmId,
    stage: RegExpCompileStage,
    origin: JsStackFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::enum_variant_names,
    reason = "the Await prefix documents the brand-first builtin-exec boundary sequence"
)]
enum RegExpExecStage {
    AwaitInputConversion,
    AwaitLastIndex,
    AwaitLastIndexConversion,
}

pub(super) struct RegExpExecContinuation {
    object: ObjectId,
    input: Option<JsString>,
    test_mode: bool,
    realm: RealmId,
    stage: RegExpExecStage,
    origin: JsStackFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::enum_variant_names,
    reason = "the Await prefix documents each generic RegExpExec protocol suspension"
)]
enum RegExpTestStage {
    AwaitInputConversion,
    AwaitExec,
    AwaitExecResult,
}

pub(super) struct RegExpTestContinuation {
    receiver: StoredValue,
    input: Option<JsString>,
    realm: RealmId,
    stage: RegExpTestStage,
    origin: JsStackFrame,
}

pub(super) enum RegExpContinuation {
    Constructor(Box<RegExpConstructorContinuation>),
    Flags(Box<RegExpFlagsContinuation>),
    ToString(Box<RegExpToStringContinuation>),
    Escape(RegExpEscapeContinuation),
    Compile(Box<RegExpCompileContinuation>),
    Exec(Box<RegExpExecContinuation>),
    Test(Box<RegExpTestContinuation>),
}

impl RegExpContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        match self {
            Self::Constructor(state) => 5_u64
                .saturating_add(u64::from(state.pattern_value.is_some()))
                .saturating_add(u64::from(state.flags_value.is_some()))
                .saturating_add(u64::from(state.source.is_some()))
                .saturating_add(u64::from(state.original_flags.is_some())),
            Self::Flags(_) => 2,
            Self::ToString(state) => 1_u64.saturating_add(u64::from(state.source.is_some())),
            Self::Escape(_) => 0,
            Self::Compile(state) => 3_u64
                .saturating_add(u64::from(state.source.is_some()))
                .saturating_add(u64::from(state.original_flags.is_some())),
            Self::Exec(state) => 1_u64.saturating_add(u64::from(state.input.is_some())),
            Self::Test(state) => 1_u64.saturating_add(u64::from(state.input.is_some())),
        }
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::Constructor(state) => {
                mark(CollectionRoot::Heap(HeapReference::Function(
                    state.function,
                )));
                mark(CollectionRoot::Heap(HeapReference::Function(
                    state.new_target,
                )));
                trace_stored_value_root(&state.pattern, mark);
                trace_stored_value_root(&state.flags, mark);
                if let Some(value) = &state.pattern_value {
                    trace_stored_value_root(value, mark);
                }
                if let Some(value) = &state.flags_value {
                    trace_stored_value_root(value, mark);
                }
                if let Some(prototype) = state.prototype {
                    mark(CollectionRoot::Heap(prototype));
                }
            }
            Self::Flags(state) => trace_stored_value_root(&state.receiver, mark),
            Self::ToString(state) => trace_stored_value_root(&state.receiver, mark),
            Self::Escape(_) => {}
            Self::Compile(state) => {
                mark(CollectionRoot::Heap(HeapReference::Object(state.object)));
                trace_stored_value_root(&state.pattern, mark);
                trace_stored_value_root(&state.flags, mark);
            }
            Self::Exec(state) => {
                mark(CollectionRoot::Heap(HeapReference::Object(state.object)));
            }
            Self::Test(state) => trace_stored_value_root(&state.receiver, mark),
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "RegExp construction retains the active function, call inputs, caller continuation, and execution budget across every observable protocol boundary"
)]
pub(super) fn begin_regexp_constructor(
    runtime: &mut Runtime,
    function: FunctionId,
    realm: RealmId,
    mut inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let construction = inputs.new_target;
    let state = RegExpConstructorContinuation {
        function,
        realm,
        new_target: construction.unwrap_or(function),
        called: construction.is_none(),
        pattern: inputs.arguments.take_first_or_undefined(),
        flags: inputs.arguments.take_first_or_undefined(),
        pattern_is_regexp: false,
        pattern_is_branded: false,
        pattern_value: None,
        flags_value: None,
        prototype: None,
        source: None,
        original_flags: None,
        stage: RegExpConstructorStage::AwaitMatch,
        origin,
    };
    if matches!(
        state.pattern,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolMatch);
        read_constructor_property(runtime, state, &key, return_to, execution_budget)
    } else {
        finish_constructor_is_regexp(runtime, state, false, return_to, execution_budget)
    }
}

pub(super) fn advance_regexp_continuation(
    runtime: &mut Runtime,
    state: RegExpContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state {
        RegExpContinuation::Constructor(state) => {
            advance_regexp_constructor(runtime, *state, completion, return_to, execution_budget)
        }
        RegExpContinuation::Flags(state) => {
            advance_regexp_flags(runtime, *state, &completion, return_to, execution_budget)
        }
        RegExpContinuation::ToString(state) => {
            advance_regexp_to_string(runtime, *state, completion, return_to, execution_budget)
        }
        RegExpContinuation::Escape(state) => {
            let text = operator_primitive_to_string(completion, state.realm, &state.origin)?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                escape_regexp_text(&text)?,
            )))
        }
        RegExpContinuation::Compile(state) => {
            advance_regexp_compile(runtime, *state, completion, return_to, execution_budget)
        }
        RegExpContinuation::Exec(state) => {
            advance_regexp_exec(runtime, *state, completion, return_to, execution_budget)
        }
        RegExpContinuation::Test(state) => {
            advance_regexp_test(runtime, *state, completion, return_to, execution_budget)
        }
    }
}

fn advance_regexp_constructor(
    runtime: &mut Runtime,
    mut state: RegExpConstructorContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        RegExpConstructorStage::AwaitMatch => {
            let branded = regexp_branded_object(runtime, &state.pattern)?;
            let is_regexp = if matches!(completion, StoredValue::Undefined) {
                branded
            } else {
                completion.is_truthy()
            };
            state.pattern_is_branded = branded;
            finish_constructor_is_regexp(runtime, state, is_regexp, return_to, execution_budget)
        }
        RegExpConstructorStage::AwaitConstructor => {
            if matches!(completion, StoredValue::Function(function) if function == state.function) {
                return Ok(NativeDispatch::Immediate(state.pattern));
            }
            prepare_constructor_pattern(runtime, state, return_to, execution_budget)
        }
        RegExpConstructorStage::AwaitSource => {
            state.pattern_value = Some(completion);
            if matches!(state.flags, StoredValue::Undefined) {
                state.stage = RegExpConstructorStage::AwaitFlags;
                let key = runtime.predefined_property_key(PredefinedAtom::Flags);
                read_constructor_property(runtime, state, &key, return_to, execution_budget)
            } else {
                state.flags_value = Some(state.flags.duplicate());
                read_constructor_prototype(runtime, state, return_to, execution_budget)
            }
        }
        RegExpConstructorStage::AwaitFlags => {
            state.flags_value = Some(completion);
            read_constructor_prototype(runtime, state, return_to, execution_budget)
        }
        RegExpConstructorStage::AwaitPrototype => {
            state.prototype = Some(match completion {
                StoredValue::Function(function) => HeapReference::Function(function),
                StoredValue::Object(object) => HeapReference::Object(object),
                StoredValue::Undefined
                | StoredValue::Null
                | StoredValue::Boolean(_)
                | StoredValue::Number(_)
                | StoredValue::BigInt(_)
                | StoredValue::String(_)
                | StoredValue::Symbol(_) => {
                    let target_realm = runtime.function_realm(state.new_target)?;
                    HeapReference::Object(runtime.realm_regexp_prototype(target_realm)?)
                }
            });
            begin_constructor_pattern_conversion(runtime, state, return_to, execution_budget)
        }
        RegExpConstructorStage::AwaitPatternConversion => {
            state.source = Some(operator_primitive_to_string(
                completion,
                state.realm,
                &state.origin,
            )?);
            begin_constructor_flags_conversion(runtime, state, return_to, execution_budget)
        }
        RegExpConstructorStage::AwaitFlagsConversion => {
            state.original_flags = Some(operator_primitive_to_string(
                completion,
                state.realm,
                &state.origin,
            )?);
            finish_regexp_constructor(runtime, state)
        }
    }
}

fn finish_constructor_is_regexp(
    runtime: &mut Runtime,
    mut state: RegExpConstructorContinuation,
    is_regexp: bool,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.pattern_is_regexp = is_regexp;
    state.pattern_is_branded = regexp_branded_object(runtime, &state.pattern)?;
    if state.called && is_regexp && matches!(state.flags, StoredValue::Undefined) {
        state.stage = RegExpConstructorStage::AwaitConstructor;
        let key = runtime.predefined_property_key(PredefinedAtom::Constructor);
        return read_constructor_property(runtime, state, &key, return_to, execution_budget);
    }
    prepare_constructor_pattern(runtime, state, return_to, execution_budget)
}

fn prepare_constructor_pattern(
    runtime: &mut Runtime,
    mut state: RegExpConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.pattern_is_branded {
        let StoredValue::Object(object) = state.pattern else {
            return Err(EngineFault::RuntimeInvariant {
                message: "RegExp-branded constructor pattern was not an object",
            }
            .into());
        };
        let internal = runtime
            .regexp_state(object)?
            .ok_or(EngineFault::RuntimeInvariant {
                message: "RegExp brand disappeared during construction",
            })?;
        state.pattern_value = Some(StoredValue::String(internal.source().clone()));
        state.flags_value = Some(if matches!(state.flags, StoredValue::Undefined) {
            StoredValue::String(internal.flags().clone())
        } else {
            state.flags.duplicate()
        });
        return read_constructor_prototype(runtime, state, return_to, execution_budget);
    }
    if state.pattern_is_regexp {
        state.stage = RegExpConstructorStage::AwaitSource;
        let key = runtime.predefined_property_key(PredefinedAtom::Source);
        return read_constructor_property(runtime, state, &key, return_to, execution_budget);
    }
    state.pattern_value = Some(state.pattern.duplicate());
    state.flags_value = Some(state.flags.duplicate());
    read_constructor_prototype(runtime, state, return_to, execution_budget)
}

fn read_constructor_prototype(
    runtime: &mut Runtime,
    mut state: RegExpConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = RegExpConstructorStage::AwaitPrototype;
    let receiver = StoredValue::Function(state.new_target);
    charge_heap_property_lookup(runtime, &receiver, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    match read_static_property(runtime, state.realm, &receiver, &key)? {
        PropertyReadOutcome::Value(value) => {
            advance_regexp_constructor(runtime, state, value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => call_regexp_function(
            function,
            receiver,
            CallArguments::empty(),
            RegExpContinuation::Constructor(Box::new(state)),
            return_to,
        ),
        PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            state.realm,
            state.origin,
            Some(&JsString::from_utf8("prototype")?),
            failure,
        )?)),
    }
}

fn read_constructor_property(
    runtime: &mut Runtime,
    state: RegExpConstructorContinuation,
    key: &PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_heap_property_lookup(runtime, &state.pattern, execution_budget)?;
    match read_static_property(runtime, state.realm, &state.pattern, key)? {
        PropertyReadOutcome::Value(value) => {
            advance_regexp_constructor(runtime, state, value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => call_regexp_function(
            function,
            receiver,
            CallArguments::empty(),
            RegExpContinuation::Constructor(Box::new(state)),
            return_to,
        ),
        PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            state.realm,
            state.origin,
            None,
            failure,
        )?)),
    }
}

fn begin_constructor_pattern_conversion(
    runtime: &mut Runtime,
    mut state: RegExpConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let value = state
        .pattern_value
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "RegExp constructor lost its pattern value",
        })?
        .duplicate();
    if matches!(value, StoredValue::Undefined) {
        state.source = Some(JsString::empty());
        return begin_constructor_flags_conversion(runtime, state, return_to, execution_budget);
    }
    state.stage = RegExpConstructorStage::AwaitPatternConversion;
    convert_regexp_value(
        runtime,
        RegExpContinuation::Constructor(Box::new(state)),
        value,
        OperatorPrimitiveHint::String,
        return_to,
        execution_budget,
    )
}

fn begin_constructor_flags_conversion(
    runtime: &mut Runtime,
    mut state: RegExpConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let value = state
        .flags_value
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "RegExp constructor lost its flags value",
        })?
        .duplicate();
    if matches!(value, StoredValue::Undefined) {
        state.original_flags = Some(JsString::empty());
        return finish_regexp_constructor(runtime, state);
    }
    state.stage = RegExpConstructorStage::AwaitFlagsConversion;
    convert_regexp_value(
        runtime,
        RegExpContinuation::Constructor(Box::new(state)),
        value,
        OperatorPrimitiveHint::String,
        return_to,
        execution_budget,
    )
}

fn finish_regexp_constructor(
    runtime: &mut Runtime,
    state: RegExpConstructorContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    let source = state.source.ok_or(EngineFault::RuntimeInvariant {
        message: "RegExp constructor completed without source text",
    })?;
    let flags = state.original_flags.ok_or(EngineFault::RuntimeInvariant {
        message: "RegExp constructor completed without flag text",
    })?;
    let pattern_units = fallible_code_units(&source)?;
    let flag_units = fallible_code_units(&flags)?;
    let matcher = match quickjs_regexp::CompiledRegExp::compile_utf16(
        &pattern_units,
        &flag_units,
        quickjs_regexp::CompileLimits::default(),
    ) {
        Ok(matcher) => matcher,
        Err(quickjs_regexp::CompileError::ResourceLimit(_)) => {
            return Err(ExecutionError::LimitExceeded {
                resource: RuntimeResource::FrameValues,
                limit: u64::from(source.len()).saturating_add(u64::from(flags.len())),
                observed: u64::from(source.len())
                    .saturating_add(u64::from(flags.len()))
                    .saturating_add(1),
            }
            .into());
        }
        Err(error) => {
            return regexp_syntax_error(state.realm, state.origin, &error.to_string());
        }
    };
    let prototype = state.prototype.ok_or(EngineFault::RuntimeInvariant {
        message: "RegExp constructor completed without a prototype",
    })?;
    let object = runtime.allocate_regexp_object(prototype, source, flags, matcher)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(super) fn regexp_flag_getter(
    runtime: &Runtime,
    realm: RealmId,
    receiver: &StoredValue,
    flag: RegExpFlag,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(object) = receiver {
        if let Some(state) = runtime.regexp_state(*object)? {
            return Ok(NativeDispatch::Immediate(StoredValue::Boolean(
                state
                    .flags()
                    .code_units()
                    .any(|unit| unit == flag.code_unit()),
            )));
        }
        if *object == runtime.realm_regexp_prototype(realm)? {
            return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
        }
    }
    regexp_type_error(realm, origin, "not a RegExp")
}

pub(super) fn regexp_source_getter(
    runtime: &Runtime,
    realm: RealmId,
    receiver: &StoredValue,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(object) = receiver {
        if let Some(state) = runtime.regexp_state(*object)? {
            return Ok(NativeDispatch::Immediate(StoredValue::String(
                escape_regexp_pattern(state.source())?,
            )));
        }
        if *object == runtime.realm_regexp_prototype(realm)? {
            return Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8("(?:)")?,
            )));
        }
    }
    regexp_type_error(realm, origin, "not a RegExp")
}

pub(super) fn begin_regexp_flags(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if !matches!(receiver, StoredValue::Function(_) | StoredValue::Object(_)) {
        return regexp_type_error(realm, origin, "not an object");
    }
    read_next_regexp_flag(
        runtime,
        RegExpFlagsContinuation {
            receiver,
            next: 0,
            result: JsString::empty(),
            realm,
            origin,
        },
        return_to,
        execution_budget,
    )
}

fn advance_regexp_flags(
    runtime: &mut Runtime,
    mut state: RegExpFlagsContinuation,
    completion: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let flag = *CANONICAL_FLAG_ACCESSORS
        .get(state.next)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "RegExp flags continuation advanced past its final accessor",
        })?;
    if completion.is_truthy() {
        state.result = state
            .result
            .concat(&JsString::from_code_units([flag.code_unit()])?)?;
    }
    state.next = state.next.saturating_add(1);
    read_next_regexp_flag(runtime, state, return_to, execution_budget)
}

fn read_next_regexp_flag(
    runtime: &mut Runtime,
    state: RegExpFlagsContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(flag) = CANONICAL_FLAG_ACCESSORS.get(state.next).copied() else {
        return Ok(NativeDispatch::Immediate(StoredValue::String(state.result)));
    };
    charge_heap_property_lookup(runtime, &state.receiver, execution_budget)?;
    let key = runtime.predefined_property_key(flag.atom());
    match read_static_property(runtime, state.realm, &state.receiver, &key)? {
        PropertyReadOutcome::Value(value) => {
            advance_regexp_flags(runtime, state, &value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => call_regexp_function(
            function,
            receiver,
            CallArguments::empty(),
            RegExpContinuation::Flags(Box::new(state)),
            return_to,
        ),
        PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            state.realm,
            state.origin,
            None,
            failure,
        )?)),
    }
}

pub(super) fn begin_regexp_to_string(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if !matches!(receiver, StoredValue::Function(_) | StoredValue::Object(_)) {
        return regexp_type_error(realm, origin, "not an object");
    }
    read_regexp_to_string_property(
        runtime,
        RegExpToStringContinuation {
            receiver,
            source: None,
            realm,
            stage: RegExpToStringStage::AwaitSource,
            origin,
        },
        PredefinedAtom::Source,
        return_to,
        execution_budget,
    )
}

fn advance_regexp_to_string(
    runtime: &mut Runtime,
    mut state: RegExpToStringContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        RegExpToStringStage::AwaitSource => {
            state.stage = RegExpToStringStage::AwaitSourceConversion;
            convert_regexp_value(
                runtime,
                RegExpContinuation::ToString(Box::new(state)),
                completion,
                OperatorPrimitiveHint::String,
                return_to,
                execution_budget,
            )
        }
        RegExpToStringStage::AwaitSourceConversion => {
            state.source = Some(operator_primitive_to_string(
                completion,
                state.realm,
                &state.origin,
            )?);
            state.stage = RegExpToStringStage::AwaitFlags;
            read_regexp_to_string_property(
                runtime,
                state,
                PredefinedAtom::Flags,
                return_to,
                execution_budget,
            )
        }
        RegExpToStringStage::AwaitFlags => {
            state.stage = RegExpToStringStage::AwaitFlagsConversion;
            convert_regexp_value(
                runtime,
                RegExpContinuation::ToString(Box::new(state)),
                completion,
                OperatorPrimitiveHint::String,
                return_to,
                execution_budget,
            )
        }
        RegExpToStringStage::AwaitFlagsConversion => {
            let flags = operator_primitive_to_string(completion, state.realm, &state.origin)?;
            let source = state.source.ok_or(EngineFault::RuntimeInvariant {
                message: "RegExp toString lost its source",
            })?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8("/")?
                    .concat(&source)?
                    .concat(&JsString::from_utf8("/")?)?
                    .concat(&flags)?,
            )))
        }
    }
}

fn read_regexp_to_string_property(
    runtime: &mut Runtime,
    state: RegExpToStringContinuation,
    atom: PredefinedAtom,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    charge_heap_property_lookup(runtime, &state.receiver, execution_budget)?;
    let key = runtime.predefined_property_key(atom);
    match read_static_property(runtime, state.realm, &state.receiver, &key)? {
        PropertyReadOutcome::Value(value) => {
            advance_regexp_to_string(runtime, state, value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => call_regexp_function(
            function,
            receiver,
            CallArguments::empty(),
            RegExpContinuation::ToString(Box::new(state)),
            return_to,
        ),
        PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            state.realm,
            state.origin,
            None,
            failure,
        )?)),
    }
}

pub(super) fn begin_regexp_escape(
    runtime: &mut Runtime,
    realm: RealmId,
    value: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    convert_regexp_value(
        runtime,
        RegExpContinuation::Escape(RegExpEscapeContinuation { realm, origin }),
        value,
        OperatorPrimitiveHint::String,
        return_to,
        execution_budget,
    )
}

pub(super) fn begin_regexp_compile(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(object) = receiver else {
        return regexp_type_error(realm, origin, "not a RegExp");
    };
    if runtime.regexp_state(*object)?.is_none() {
        return regexp_type_error(realm, origin, "not a RegExp");
    }
    let pattern = arguments.take_first_or_undefined();
    let flags = arguments.take_first_or_undefined();
    let (pattern, flags) = if let StoredValue::Object(pattern_object) = pattern {
        if let Some(internal) = runtime.regexp_state(pattern_object)? {
            if !matches!(flags, StoredValue::Undefined) {
                return regexp_type_error(realm, origin, "flags must be undefined");
            }
            (
                StoredValue::String(internal.source().clone()),
                StoredValue::String(internal.flags().clone()),
            )
        } else {
            (StoredValue::Object(pattern_object), flags)
        }
    } else {
        (pattern, flags)
    };
    let mut state = RegExpCompileContinuation {
        object: *object,
        pattern,
        flags,
        source: None,
        original_flags: None,
        realm,
        stage: RegExpCompileStage::AwaitPatternConversion,
        origin,
    };
    if matches!(state.pattern, StoredValue::Undefined) {
        state.source = Some(JsString::empty());
        begin_regexp_compile_flags(runtime, state, return_to, execution_budget)
    } else {
        let value = state.pattern.duplicate();
        convert_regexp_value(
            runtime,
            RegExpContinuation::Compile(Box::new(state)),
            value,
            OperatorPrimitiveHint::String,
            return_to,
            execution_budget,
        )
    }
}

fn advance_regexp_compile(
    runtime: &mut Runtime,
    mut state: RegExpCompileContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        RegExpCompileStage::AwaitPatternConversion => {
            state.source = Some(operator_primitive_to_string(
                completion,
                state.realm,
                &state.origin,
            )?);
            begin_regexp_compile_flags(runtime, state, return_to, execution_budget)
        }
        RegExpCompileStage::AwaitFlagsConversion => {
            state.original_flags = Some(operator_primitive_to_string(
                completion,
                state.realm,
                &state.origin,
            )?);
            finish_regexp_compile(runtime, state, execution_budget)
        }
    }
}

fn begin_regexp_compile_flags(
    runtime: &mut Runtime,
    mut state: RegExpCompileContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.flags, StoredValue::Undefined) {
        state.original_flags = Some(JsString::empty());
        return finish_regexp_compile(runtime, state, execution_budget);
    }
    state.stage = RegExpCompileStage::AwaitFlagsConversion;
    let value = state.flags.duplicate();
    convert_regexp_value(
        runtime,
        RegExpContinuation::Compile(Box::new(state)),
        value,
        OperatorPrimitiveHint::String,
        return_to,
        execution_budget,
    )
}

fn finish_regexp_compile(
    runtime: &mut Runtime,
    state: RegExpCompileContinuation,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = state.source.ok_or(EngineFault::RuntimeInvariant {
        message: "RegExp compile completed without source text",
    })?;
    let flags = state.original_flags.ok_or(EngineFault::RuntimeInvariant {
        message: "RegExp compile completed without flag text",
    })?;
    let pattern_units = fallible_code_units(&source)?;
    let flag_units = fallible_code_units(&flags)?;
    let matcher = match quickjs_regexp::CompiledRegExp::compile_utf16(
        &pattern_units,
        &flag_units,
        quickjs_regexp::CompileLimits::default(),
    ) {
        Ok(matcher) => matcher,
        Err(quickjs_regexp::CompileError::ResourceLimit(_)) => {
            return Err(ExecutionError::LimitExceeded {
                resource: RuntimeResource::FrameValues,
                limit: u64::from(source.len()).saturating_add(u64::from(flags.len())),
                observed: u64::from(source.len())
                    .saturating_add(u64::from(flags.len()))
                    .saturating_add(1),
            }
            .into());
        }
        Err(error) => return regexp_syntax_error(state.realm, state.origin, &error.to_string()),
    };
    runtime
        .regexp_state_mut(state.object)?
        .ok_or(EngineFault::RuntimeInvariant {
            message: "RegExp brand disappeared during compile",
        })?
        .reinitialize(source, flags, matcher);
    write_regexp_last_index_value(
        runtime,
        state.object,
        state.realm,
        &state.origin,
        0,
        execution_budget,
    )?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(state.object)))
}

pub(super) fn begin_regexp_exec(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: &StoredValue,
    input: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(object) = receiver else {
        return regexp_type_error(realm, origin, "not a RegExp");
    };
    if runtime.regexp_state(*object)?.is_none() {
        return regexp_type_error(realm, origin, "not a RegExp");
    }
    let state = RegExpExecContinuation {
        object: *object,
        input: None,
        test_mode: false,
        realm,
        stage: RegExpExecStage::AwaitInputConversion,
        origin,
    };
    convert_regexp_value(
        runtime,
        RegExpContinuation::Exec(Box::new(state)),
        input,
        OperatorPrimitiveHint::String,
        return_to,
        execution_budget,
    )
}

fn advance_regexp_exec(
    runtime: &mut Runtime,
    mut state: RegExpExecContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        RegExpExecStage::AwaitInputConversion => {
            state.input = Some(operator_primitive_to_string(
                completion,
                state.realm,
                &state.origin,
            )?);
            state.stage = RegExpExecStage::AwaitLastIndex;
            let receiver = StoredValue::Object(state.object);
            charge_heap_property_lookup(runtime, &receiver, execution_budget)?;
            let key = runtime.predefined_property_key(PredefinedAtom::LastIndex);
            match read_static_property(runtime, state.realm, &receiver, &key)? {
                PropertyReadOutcome::Value(value) => {
                    advance_regexp_exec(runtime, state, value, return_to, execution_budget)
                }
                PropertyReadOutcome::Getter { function, receiver } => call_regexp_function(
                    function,
                    receiver,
                    CallArguments::empty(),
                    RegExpContinuation::Exec(Box::new(state)),
                    return_to,
                ),
                PropertyReadOutcome::Failed(failure) => {
                    Err(NativeFailure::Abrupt(property_exception_at(
                        state.realm,
                        state.origin,
                        Some(&JsString::from_utf8("lastIndex")?),
                        failure,
                    )?))
                }
            }
        }
        RegExpExecStage::AwaitLastIndex => {
            state.stage = RegExpExecStage::AwaitLastIndexConversion;
            convert_regexp_value(
                runtime,
                RegExpContinuation::Exec(Box::new(state)),
                completion,
                OperatorPrimitiveHint::Number,
                return_to,
                execution_budget,
            )
        }
        RegExpExecStage::AwaitLastIndexConversion => {
            let last_index =
                number_to_length(operator_to_number(completion, state.realm, &state.origin)?);
            finish_regexp_builtin_exec(runtime, &state, last_index, execution_budget)
        }
    }
}

fn begin_regexp_builtin_exec_for_test(
    runtime: &mut Runtime,
    object: ObjectId,
    input: JsString,
    realm: RealmId,
    origin: JsStackFrame,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = RegExpExecContinuation {
        object,
        input: Some(input),
        test_mode: true,
        realm,
        stage: RegExpExecStage::AwaitLastIndex,
        origin,
    };
    let receiver = StoredValue::Object(object);
    charge_heap_property_lookup(runtime, &receiver, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::LastIndex);
    match read_static_property(runtime, realm, &receiver, &key)? {
        PropertyReadOutcome::Value(value) => {
            advance_regexp_exec(runtime, state, value, return_to, execution_budget)
        }
        PropertyReadOutcome::Getter { function, receiver } => call_regexp_function(
            function,
            receiver,
            CallArguments::empty(),
            RegExpContinuation::Exec(Box::new(state)),
            return_to,
        ),
        PropertyReadOutcome::Failed(failure) => Err(NativeFailure::Abrupt(property_exception_at(
            realm,
            state.origin,
            Some(&JsString::from_utf8("lastIndex")?),
            failure,
        )?)),
    }
}

fn finish_regexp_builtin_exec(
    runtime: &mut Runtime,
    state: &RegExpExecContinuation,
    mut last_index: u64,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let input = state.input.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "RegExp builtin exec lost its input string",
    })?;
    let input_units = fallible_code_units(input)?;
    let (global, sticky, has_indices, capture_names, execution) = {
        let internal =
            runtime
                .regexp_state(state.object)?
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "RegExp brand disappeared before execution",
                })?;
        let global = string_has_code_unit(internal.flags(), u16::from(b'g'));
        let sticky = string_has_code_unit(internal.flags(), u16::from(b'y'));
        let has_indices = string_has_code_unit(internal.flags(), u16::from(b'd'));
        if !global && !sticky {
            last_index = 0;
        }
        let start_index = if last_index > u64::from(input.len()) {
            None
        } else {
            usize::try_from(last_index).ok()
        };
        let capture_names = fallible_capture_names(internal.matcher())?;
        let execution = start_index.map(|start_index| {
            internal.matcher().execute_counted(
                &input_units,
                start_index,
                quickjs_regexp::ExecLimits {
                    max_steps: execution_budget.remaining_instructions(),
                    ..quickjs_regexp::ExecLimits::default()
                },
            )
        });
        (global, sticky, has_indices, capture_names, execution)
    };
    let matched = match execution {
        None => None,
        Some((result, steps)) => {
            execution_budget.charge_instructions(steps)?;
            match result {
                Ok(matched) => matched,
                Err(quickjs_regexp::ExecError::StepLimit) => {
                    execution_budget.charge_instructions(1)?;
                    return Err(EngineFault::RuntimeInvariant {
                        message: "RegExp step limit did not exhaust interpreter fuel",
                    }
                    .into());
                }
                Err(quickjs_regexp::ExecError::BacktrackLimit) => {
                    let limit =
                        u64::try_from(quickjs_regexp::ExecLimits::default().max_backtrack_states)
                            .unwrap_or(u64::MAX);
                    return Err(ExecutionError::LimitExceeded {
                        resource: RuntimeResource::RegExpBacktrackStates,
                        limit,
                        observed: limit.saturating_add(1),
                    }
                    .into());
                }
            }
        }
    };
    let Some(matched) = matched else {
        if global || sticky {
            write_regexp_last_index(runtime, state, 0, execution_budget)?;
        }
        return Ok(NativeDispatch::Immediate(if state.test_mode {
            StoredValue::Boolean(false)
        } else {
            StoredValue::Null
        }));
    };
    let whole = matched.range();
    if global || sticky {
        write_regexp_last_index(
            runtime,
            state,
            u64::try_from(whole.end).unwrap_or(u64::MAX),
            execution_budget,
        )?;
    }
    if state.test_mode {
        return Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)));
    }
    materialize_regexp_match(
        runtime,
        state.realm,
        input,
        &matched,
        &capture_names,
        has_indices,
    )
}

fn write_regexp_last_index(
    runtime: &mut Runtime,
    state: &RegExpExecContinuation,
    index: u64,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    write_regexp_last_index_value(
        runtime,
        state.object,
        state.realm,
        &state.origin,
        index,
        execution_budget,
    )
}

fn fallible_capture_names(
    matcher: &quickjs_regexp::CompiledRegExp,
) -> Result<Vec<Option<JsString>>, NativeFailure> {
    let source_names = matcher.capture_names();
    let mut capture_names = Vec::new();
    capture_names
        .try_reserve_exact(source_names.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: source_names.len(),
        })?;
    for name in source_names {
        capture_names.push(match name {
            Some(name) => Some(JsString::from_utf8(name)?),
            None => None,
        });
    }
    Ok(capture_names)
}

fn write_regexp_last_index_value(
    runtime: &mut Runtime,
    object: ObjectId,
    realm: RealmId,
    origin: &JsStackFrame,
    index: u64,
    execution_budget: &mut ExecutionBudget,
) -> Result<(), NativeFailure> {
    let receiver = StoredValue::Object(object);
    let key = runtime.predefined_property_key(PredefinedAtom::LastIndex);
    let index = u32::try_from(index).map_err(|_| EngineFault::RuntimeInvariant {
        message: "RegExp lastIndex exceeded the JavaScript string domain",
    })?;
    match write_static_property(
        runtime,
        realm,
        &receiver,
        key,
        StoredValue::Number(JsNumber::from_f64(f64::from(index))),
        true,
        execution_budget,
    )? {
        PropertyWriteOutcome::Complete => Ok(()),
        PropertyWriteOutcome::Setter { .. } => Err(EngineFault::RuntimeInvariant {
            message: "RegExp lastIndex own data property became an accessor",
        }
        .into()),
        PropertyWriteOutcome::Failed(_) => {
            let Err(error) =
                regexp_type_error(realm, origin.clone(), "cannot write RegExp lastIndex")
            else {
                return Err(EngineFault::RuntimeInvariant {
                    message: "RegExp lastIndex TypeError unexpectedly completed",
                }
                .into());
            };
            Err(error)
        }
    }
}

fn materialize_regexp_match(
    runtime: &mut Runtime,
    realm: RealmId,
    input: &JsString,
    matched: &quickjs_regexp::Match,
    capture_names: &[Option<JsString>],
    has_indices: bool,
) -> Result<NativeDispatch, NativeFailure> {
    let mut captures = Vec::new();
    captures
        .try_reserve_exact(matched.captures.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: matched.captures.len(),
        })?;
    for range in &matched.captures {
        captures.push(match range {
            Some(range) => StoredValue::String(slice_match_range(input, range)?),
            None => StoredValue::Undefined,
        });
    }
    let groups = materialize_named_capture_groups(runtime, capture_names, &captures)?;
    let result = runtime.allocate_array(realm, captures)?;
    append_match_property(
        runtime,
        result,
        PredefinedAtom::Index,
        match_position_value(matched.range().start)?,
    )?;
    append_match_property(
        runtime,
        result,
        PredefinedAtom::Input,
        StoredValue::String(input.clone()),
    )?;
    append_match_property(runtime, result, PredefinedAtom::Groups, groups)?;
    if has_indices {
        let indices = materialize_match_indices(runtime, realm, &matched.captures, capture_names)?;
        append_match_property(
            runtime,
            result,
            PredefinedAtom::Indices,
            StoredValue::Object(indices),
        )?;
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
}

fn materialize_match_indices(
    runtime: &mut Runtime,
    realm: RealmId,
    ranges: &[Option<std::ops::Range<usize>>],
    capture_names: &[Option<JsString>],
) -> Result<ObjectId, NativeFailure> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(ranges.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: ranges.len(),
        })?;
    for range in ranges {
        values.push(match range {
            Some(range) => {
                let mut pair_values = Vec::new();
                pair_values
                    .try_reserve_exact(2)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: 2,
                    })?;
                pair_values.push(match_position_value(range.start)?);
                pair_values.push(match_position_value(range.end)?);
                let pair = runtime.allocate_array(realm, pair_values)?;
                StoredValue::Object(pair)
            }
            None => StoredValue::Undefined,
        });
    }
    let groups = materialize_named_capture_groups(runtime, capture_names, &values)?;
    let indices = runtime.allocate_array(realm, values)?;
    append_match_property(runtime, indices, PredefinedAtom::Groups, groups)?;
    Ok(indices)
}

fn materialize_named_capture_groups(
    runtime: &mut Runtime,
    capture_names: &[Option<JsString>],
    values: &[StoredValue],
) -> Result<StoredValue, NativeFailure> {
    if !capture_names.iter().any(Option::is_some) {
        return Ok(StoredValue::Undefined);
    }
    let mut named: Vec<(JsString, StoredValue)> = Vec::new();
    for (index, name) in capture_names.iter().enumerate().skip(1) {
        let Some(name) = name else {
            continue;
        };
        let value = values
            .get(index)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "RegExp capture names exceeded capture values",
            })?
            .duplicate();
        if let Some((_, existing)) = named.iter_mut().find(|(existing, _)| existing == name) {
            if !matches!(value, StoredValue::Undefined) {
                *existing = value;
            }
        } else {
            named
                .try_reserve(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: 1,
                })?;
            named.push((name.clone(), value));
        }
    }
    let groups = runtime.allocate_ordinary_object_with_optional_prototype(None)?;
    for (name, value) in named {
        let key = runtime.property_key_from_string(&name)?;
        runtime.append_data_property(
            HeapReference::Object(groups),
            key,
            PropertyLayout::data(true, true, true),
            value,
        )?;
    }
    Ok(StoredValue::Object(groups))
}

fn append_match_property(
    runtime: &mut Runtime,
    object: ObjectId,
    atom: PredefinedAtom,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    runtime.append_data_property(
        HeapReference::Object(object),
        runtime.predefined_property_key(atom),
        PropertyLayout::data(true, true, true),
        value,
    )?;
    Ok(())
}

fn slice_match_range(
    input: &JsString,
    range: &std::ops::Range<usize>,
) -> Result<JsString, NativeFailure> {
    let start = u32::try_from(range.start).map_err(|_| EngineFault::RuntimeInvariant {
        message: "RegExp match start exceeded the JavaScript string domain",
    })?;
    let end = u32::try_from(range.end).map_err(|_| EngineFault::RuntimeInvariant {
        message: "RegExp match end exceeded the JavaScript string domain",
    })?;
    Ok(input.slice(start..end)?)
}

fn match_position_value(position: usize) -> Result<StoredValue, NativeFailure> {
    let position = u32::try_from(position).map_err(|_| EngineFault::RuntimeInvariant {
        message: "RegExp match position exceeded the JavaScript string domain",
    })?;
    Ok(StoredValue::Number(JsNumber::from_f64(f64::from(position))))
}

pub(super) fn begin_regexp_test(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: StoredValue,
    input: StoredValue,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if !matches!(receiver, StoredValue::Function(_) | StoredValue::Object(_)) {
        return regexp_type_error(realm, origin, "not an object");
    }
    let state = RegExpTestContinuation {
        receiver,
        input: None,
        realm,
        stage: RegExpTestStage::AwaitInputConversion,
        origin,
    };
    convert_regexp_value(
        runtime,
        RegExpContinuation::Test(Box::new(state)),
        input,
        OperatorPrimitiveHint::String,
        return_to,
        execution_budget,
    )
}

fn advance_regexp_test(
    runtime: &mut Runtime,
    mut state: RegExpTestContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        RegExpTestStage::AwaitInputConversion => {
            state.input = Some(operator_primitive_to_string(
                completion,
                state.realm,
                &state.origin,
            )?);
            state.stage = RegExpTestStage::AwaitExec;
            charge_heap_property_lookup(runtime, &state.receiver, execution_budget)?;
            let key = runtime.predefined_property_key(PredefinedAtom::Exec);
            match read_static_property(runtime, state.realm, &state.receiver, &key)? {
                PropertyReadOutcome::Value(value) => {
                    advance_regexp_test(runtime, state, value, return_to, execution_budget)
                }
                PropertyReadOutcome::Getter { function, receiver } => call_regexp_function(
                    function,
                    receiver,
                    CallArguments::empty(),
                    RegExpContinuation::Test(Box::new(state)),
                    return_to,
                ),
                PropertyReadOutcome::Failed(failure) => {
                    Err(NativeFailure::Abrupt(property_exception_at(
                        state.realm,
                        state.origin,
                        Some(&JsString::from_utf8("exec")?),
                        failure,
                    )?))
                }
            }
        }
        RegExpTestStage::AwaitExec => match completion {
            StoredValue::Function(function) => {
                let input = state.input.as_ref().ok_or(EngineFault::RuntimeInvariant {
                    message: "RegExp test lost its input",
                })?;
                let mut arguments = Vec::new();
                arguments
                    .try_reserve_exact(1)
                    .map_err(|_| ExecutionError::AllocationFailed {
                        resource: RuntimeResource::FrameValues,
                        additional: 1,
                    })?;
                arguments.push(StoredValue::String(input.clone()));
                state.stage = RegExpTestStage::AwaitExecResult;
                let receiver = state.receiver.duplicate();
                call_regexp_function(
                    function,
                    receiver,
                    CallArguments::from_values(arguments),
                    RegExpContinuation::Test(Box::new(state)),
                    return_to,
                )
            }
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_)
            | StoredValue::Object(_) => {
                let StoredValue::Object(object) = state.receiver else {
                    return regexp_type_error(state.realm, state.origin, "not a RegExp");
                };
                if runtime.regexp_state(object)?.is_none() {
                    return regexp_type_error(state.realm, state.origin, "not a RegExp");
                }
                let input = state.input.ok_or(EngineFault::RuntimeInvariant {
                    message: "RegExp test lost its input",
                })?;
                begin_regexp_builtin_exec_for_test(
                    runtime,
                    object,
                    input,
                    state.realm,
                    state.origin,
                    return_to,
                    execution_budget,
                )
            }
        },
        RegExpTestStage::AwaitExecResult => match completion {
            StoredValue::Null => Ok(NativeDispatch::Immediate(StoredValue::Boolean(false))),
            StoredValue::Function(_) | StoredValue::Object(_) => {
                Ok(NativeDispatch::Immediate(StoredValue::Boolean(true)))
            }
            StoredValue::Undefined
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_) => regexp_type_error(
                state.realm,
                state.origin,
                "RegExp exec returned a primitive",
            ),
        },
    }
}

fn regexp_branded_object(runtime: &Runtime, value: &StoredValue) -> Result<bool, NativeFailure> {
    let StoredValue::Object(object) = value else {
        return Ok(false);
    };
    Ok(runtime.regexp_state(*object)?.is_some())
}

fn convert_regexp_value(
    runtime: &mut Runtime,
    state: RegExpContinuation,
    value: StoredValue,
    hint: OperatorPrimitiveHint,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (realm, origin) = regexp_continuation_context(&state);
    begin_operator_primitive_conversion(
        runtime,
        value,
        hint,
        OperatorPrimitiveTarget::RegExpValue(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn regexp_continuation_context(state: &RegExpContinuation) -> (RealmId, JsStackFrame) {
    match state {
        RegExpContinuation::Constructor(state) => (state.realm, state.origin.clone()),
        RegExpContinuation::Flags(state) => (state.realm, state.origin.clone()),
        RegExpContinuation::ToString(state) => (state.realm, state.origin.clone()),
        RegExpContinuation::Escape(state) => (state.realm, state.origin.clone()),
        RegExpContinuation::Compile(state) => (state.realm, state.origin.clone()),
        RegExpContinuation::Exec(state) => (state.realm, state.origin.clone()),
        RegExpContinuation::Test(state) => (state.realm, state.origin.clone()),
    }
}

fn call_regexp_function(
    function: FunctionId,
    receiver: StoredValue,
    arguments: CallArguments,
    continuation: RegExpContinuation,
    return_to: Option<CallReturn>,
) -> Result<NativeDispatch, NativeFailure> {
    let (_, origin) = regexp_continuation_context(&continuation);
    let mut continuations = Vec::new();
    continuations
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    continuations.push(NativeContinuation::RegExp(Box::new(continuation)));
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

fn fallible_code_units(value: &JsString) -> Result<Vec<u16>, NativeFailure> {
    let length = usize::try_from(value.len()).map_err(|_| EngineFault::RuntimeInvariant {
        message: "JavaScript string length exceeded usize",
    })?;
    let mut units = Vec::new();
    units
        .try_reserve_exact(length)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: length,
        })?;
    units.extend(value.code_units());
    Ok(units)
}

fn string_has_code_unit(value: &JsString, expected: u16) -> bool {
    value.code_units().any(|unit| unit == expected)
}

fn escape_regexp_pattern(source: &JsString) -> Result<JsString, NativeFailure> {
    if source.is_empty() {
        return Ok(JsString::from_utf8("(?:)")?);
    }
    let capacity = usize::try_from(source.len())
        .unwrap_or(usize::MAX)
        .saturating_mul(6);
    let mut output = Vec::new();
    output
        .try_reserve_exact(capacity)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: capacity,
        })?;
    for unit in source.code_units() {
        match unit {
            unit if unit == u16::from(b'/') => {
                output.push(u16::from(b'\\'));
                output.push(unit);
            }
            unit if unit == u16::from(b'\n') => output.extend([u16::from(b'\\'), u16::from(b'n')]),
            unit if unit == u16::from(b'\r') => output.extend([u16::from(b'\\'), u16::from(b'r')]),
            0x2028 | 0x2029 => push_unicode_escape_units(&mut output, unit),
            _ => output.push(unit),
        }
    }
    Ok(JsString::from_code_units(output)?)
}

fn escape_regexp_text(source: &JsString) -> Result<JsString, NativeFailure> {
    let capacity = usize::try_from(source.len())
        .unwrap_or(usize::MAX)
        .saturating_mul(6);
    let mut output = Vec::new();
    output
        .try_reserve_exact(capacity)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: capacity,
        })?;
    let units = fallible_code_units(source)?;
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        if index == 0 && is_ascii_letter_or_digit(unit) {
            push_hex_escape_units(&mut output, unit);
            index += 1;
            continue;
        }
        if is_regexp_syntax_character(unit) || unit == u16::from(b'/') {
            output.push(u16::from(b'\\'));
            output.push(unit);
        } else if is_other_ascii_punctuator(unit) || unit == u16::from(b' ') {
            push_hex_escape_units(&mut output, unit);
        } else {
            match unit {
                unit if unit == u16::from(b'\t') => {
                    output.extend([u16::from(b'\\'), u16::from(b't')]);
                }
                unit if unit == u16::from(b'\n') => {
                    output.extend([u16::from(b'\\'), u16::from(b'n')]);
                }
                0x000b => output.extend([u16::from(b'\\'), u16::from(b'v')]),
                0x000c => output.extend([u16::from(b'\\'), u16::from(b'f')]),
                unit if unit == u16::from(b'\r') => {
                    output.extend([u16::from(b'\\'), u16::from(b'r')]);
                }
                _ if is_ecmascript_whitespace_or_line_terminator(unit)
                    || is_lone_surrogate(&units, index) =>
                {
                    push_unicode_escape_units(&mut output, unit);
                }
                _ => output.push(unit),
            }
        }
        index += 1;
    }
    Ok(JsString::from_code_units(output)?)
}

fn is_ascii_letter_or_digit(unit: u16) -> bool {
    u8::try_from(unit).is_ok_and(|unit| char::from(unit).is_ascii_alphanumeric())
}

fn is_regexp_syntax_character(unit: u16) -> bool {
    matches!(
        u8::try_from(unit),
        Ok(b'^'
            | b'$'
            | b'\\'
            | b'.'
            | b'*'
            | b'+'
            | b'?'
            | b'('
            | b')'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'|')
    )
}

fn is_other_ascii_punctuator(unit: u16) -> bool {
    matches!(
        u8::try_from(unit),
        Ok(b','
            | b'-'
            | b'='
            | b'<'
            | b'>'
            | b'#'
            | b'&'
            | b'!'
            | b'%'
            | b':'
            | b';'
            | b'@'
            | b'~'
            | b'\''
            | b'`'
            | b'"')
    )
}

fn is_ecmascript_whitespace_or_line_terminator(unit: u16) -> bool {
    matches!(
        unit,
        0x0009 | 0x000b | 0x000c | 0x0020 | 0x00a0 | 0x1680 | 0x2000
            ..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff | 0x000a | 0x000d
    )
}

fn is_lone_surrogate(units: &[u16], index: usize) -> bool {
    let unit = units[index];
    if (0xd800..=0xdbff).contains(&unit) {
        return !units
            .get(index + 1)
            .is_some_and(|next| (0xdc00..=0xdfff).contains(next));
    }
    if (0xdc00..=0xdfff).contains(&unit) {
        return index == 0 || !(0xd800..=0xdbff).contains(&units[index - 1]);
    }
    false
}

fn push_hex_escape_units(output: &mut Vec<u16>, unit: u16) {
    output.extend([u16::from(b'\\'), u16::from(b'x')]);
    output.push(hex_digit((unit >> 4) & 0x0f));
    output.push(hex_digit(unit & 0x0f));
}

fn push_unicode_escape_units(output: &mut Vec<u16>, unit: u16) {
    output.extend([u16::from(b'\\'), u16::from(b'u')]);
    output.push(hex_digit((unit >> 12) & 0x0f));
    output.push(hex_digit((unit >> 8) & 0x0f));
    output.push(hex_digit((unit >> 4) & 0x0f));
    output.push(hex_digit(unit & 0x0f));
}

fn hex_digit(nibble: u16) -> u16 {
    if nibble < 10 {
        u16::from(b'0') + nibble
    } else {
        u16::from(b'a') + (nibble - 10)
    }
}

pub(super) fn regexp_type_error(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}

fn regexp_syntax_error(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::SyntaxError,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}
