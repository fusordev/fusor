//! Resumable ECMA-402 locale-list canonicalization.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

use quickjs_intl::{
    CollatorRequestOptions, CollatorSensitivity, CollatorState, CollatorUsage, LocaleComponents,
    LocaleOptionKind, LocaleOptions, LocaleWeekInfo, apply_locale_options, calendars_of_locale,
    canonicalize_locale, canonicalize_locale_option, collations_of_locale,
    collator_supported_locales, compare_with_collator, hour_cycles_of_locale, locale_components,
    maximize_locale, minimize_locale, numbering_systems_of_locale, resolve_collator,
    supported_values, text_direction_of_locale, time_zones_of_locale, week_info_of_locale,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlLocaleConstructorStage {
    AwaitTagPrimitive,
    ReadOption,
    AwaitOption,
    AwaitOptionPrimitive,
    AwaitPrototype,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlLocaleOption {
    Language,
    Script,
    Region,
    Variants,
    Calendar,
    Collation,
    FirstDayOfWeek,
    HourCycle,
    CaseFirst,
    Numeric,
    NumberingSystem,
}

impl IntlLocaleOption {
    const ALL: [Self; 11] = [
        Self::Language,
        Self::Script,
        Self::Region,
        Self::Variants,
        Self::Calendar,
        Self::Collation,
        Self::FirstDayOfWeek,
        Self::HourCycle,
        Self::CaseFirst,
        Self::Numeric,
        Self::NumberingSystem,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Language => "language",
            Self::Script => "script",
            Self::Region => "region",
            Self::Variants => "variants",
            Self::Calendar => "calendar",
            Self::Collation => "collation",
            Self::FirstDayOfWeek => "firstDayOfWeek",
            Self::HourCycle => "hourCycle",
            Self::CaseFirst => "caseFirst",
            Self::Numeric => "numeric",
            Self::NumberingSystem => "numberingSystem",
        }
    }

    const fn string_kind(self) -> Option<LocaleOptionKind> {
        match self {
            Self::Language => Some(LocaleOptionKind::Language),
            Self::Script => Some(LocaleOptionKind::Script),
            Self::Region => Some(LocaleOptionKind::Region),
            Self::Variants => Some(LocaleOptionKind::Variants),
            Self::Calendar => Some(LocaleOptionKind::Calendar),
            Self::Collation => Some(LocaleOptionKind::Collation),
            Self::FirstDayOfWeek => Some(LocaleOptionKind::FirstDayOfWeek),
            Self::HourCycle => Some(LocaleOptionKind::HourCycle),
            Self::CaseFirst => Some(LocaleOptionKind::CaseFirst),
            Self::NumberingSystem => Some(LocaleOptionKind::NumberingSystem),
            Self::Numeric => None,
        }
    }

    fn store(self, options: &mut LocaleOptions, value: String) {
        match self {
            Self::Language => options.language = Some(value),
            Self::Script => options.script = Some(value),
            Self::Region => options.region = Some(value),
            Self::Variants => options.variants = Some(value),
            Self::Calendar => options.calendar = Some(value),
            Self::Collation => options.collation = Some(value),
            Self::FirstDayOfWeek => options.first_day_of_week = Some(value),
            Self::HourCycle => options.hour_cycle = Some(value),
            Self::CaseFirst => options.case_first = Some(value),
            Self::NumberingSystem => options.numbering_system = Some(value),
            Self::Numeric => unreachable!("numeric is stored as a Boolean"),
        }
    }
}

pub(super) struct IntlLocaleConstructorContinuation {
    new_target: FunctionId,
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    locale: Option<JsString>,
    locale_options: LocaleOptions,
    option_index: usize,
    realm: RealmId,
    stage: IntlLocaleConstructorStage,
    origin: JsStackFrame,
}

impl IntlLocaleConstructorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64.saturating_add(u64::from(self.options_object.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
        trace_stored_value_root(&self.options_argument, mark);
        if let Some(options) = &self.options_object {
            trace_stored_value_root(options, mark);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlLocaleListStage {
    AwaitLength,
    AwaitLengthConversion,
    Next,
    AwaitHas,
    AwaitElement,
    AwaitElementString,
}

enum IntlLocaleListTarget {
    ReturnArray,
    CollatorConstructor(Box<IntlCollatorConstructorContinuation>),
    CollatorSupportedLocalesOf(Box<IntlCollatorSupportedLocalesContinuation>),
}

impl IntlLocaleListTarget {
    fn retained_values(&self) -> u64 {
        match self {
            Self::ReturnArray => 0,
            Self::CollatorConstructor(state) => state.retained_values(),
            Self::CollatorSupportedLocalesOf(state) => state.retained_values(),
        }
    }

    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::ReturnArray => {}
            Self::CollatorConstructor(state) => state.trace_roots(mark),
            Self::CollatorSupportedLocalesOf(state) => state.trace_roots(mark),
        }
    }
}

/// One suspended `CanonicalizeLocaleList` operation.
pub(super) struct IntlLocaleListContinuation {
    source: StoredValue,
    seen: Vec<StoredValue>,
    index: u64,
    length: u64,
    realm: RealmId,
    stage: IntlLocaleListStage,
    target: IntlLocaleListTarget,
    origin: JsStackFrame,
}

impl IntlLocaleListContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64
            .saturating_add(usize_to_u64(self.seen.len()))
            .saturating_add(self.target.retained_values())
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.source, mark);
        for value in &self.seen {
            trace_stored_value_root(value, mark);
        }
        self.target.trace_roots(mark);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlCollatorConstructorStage {
    ReadOption,
    AwaitOption,
    AwaitOptionPrimitive,
    AwaitPrototype,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlCollatorOption {
    Usage,
    LocaleMatcher,
    Collation,
    Numeric,
    CaseFirst,
    Sensitivity,
    IgnorePunctuation,
}

impl IntlCollatorOption {
    const ALL: [Self; 7] = [
        Self::Usage,
        Self::LocaleMatcher,
        Self::Collation,
        Self::Numeric,
        Self::CaseFirst,
        Self::Sensitivity,
        Self::IgnorePunctuation,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::LocaleMatcher => "localeMatcher",
            Self::Collation => "collation",
            Self::Numeric => "numeric",
            Self::CaseFirst => "caseFirst",
            Self::Sensitivity => "sensitivity",
            Self::IgnorePunctuation => "ignorePunctuation",
        }
    }

    const fn is_boolean(self) -> bool {
        matches!(self, Self::Numeric | Self::IgnorePunctuation)
    }
}

pub(super) struct IntlCollatorConstructorContinuation {
    new_target: FunctionId,
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    options: CollatorRequestOptions,
    resolved: Option<CollatorState>,
    option_index: usize,
    realm: RealmId,
    stage: IntlCollatorConstructorStage,
    origin: JsStackFrame,
}

impl IntlCollatorConstructorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64.saturating_add(u64::from(self.options_object.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
        trace_stored_value_root(&self.options_argument, mark);
        if let Some(options) = &self.options_object {
            trace_stored_value_root(options, mark);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlCollatorSupportedLocalesStage {
    ReadLocaleMatcher,
    AwaitLocaleMatcher,
    AwaitLocaleMatcherPrimitive,
}

pub(super) struct IntlCollatorSupportedLocalesContinuation {
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    realm: RealmId,
    stage: IntlCollatorSupportedLocalesStage,
    origin: JsStackFrame,
}

impl IntlCollatorSupportedLocalesContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(u64::from(self.options_object.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options_argument, mark);
        if let Some(options) = &self.options_object {
            trace_stored_value_root(options, mark);
        }
    }
}

pub(super) struct IntlCollatorCompareContinuation {
    collator: ObjectId,
    second: StoredValue,
    first: Option<JsString>,
    realm: RealmId,
    origin: JsStackFrame,
}

impl IntlCollatorCompareContinuation {
    #[allow(
        clippy::unused_self,
        reason = "the continuation owns two retained frame values on every comparison stage"
    )]
    pub(super) fn retained_values(&self) -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.collator)));
        trace_stored_value_root(&self.second, mark);
    }
}

pub(super) fn begin_intl_supported_values_of(
    runtime: &mut Runtime,
    key: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match key {
        StoredValue::Object(object) => begin_operator_primitive_conversion(
            runtime,
            StoredValue::Object(object),
            OperatorPrimitiveHint::String,
            OperatorPrimitiveTarget::IntlSupportedValuesOf,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        StoredValue::Function(function) => begin_operator_primitive_conversion(
            runtime,
            StoredValue::Function(function),
            OperatorPrimitiveHint::String,
            OperatorPrimitiveTarget::IntlSupportedValuesOf,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        key => finish_intl_supported_values_of(runtime, key, realm, &origin),
    }
}

pub(super) fn finish_intl_supported_values_of(
    runtime: &mut Runtime,
    key: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let key = operator_primitive_to_string(key, realm, origin)?;
    let key = key.to_utf8_lossy()?;
    let Some(values) = supported_values(&key) else {
        return intl_locale_list_error(
            realm,
            origin.clone(),
            ExceptionKind::RangeError,
            "invalid key for Intl.supportedValuesOf",
        );
    };
    intl_locale_string_array(runtime, realm, values)
}

pub(super) fn begin_intl_locale_constructor(
    runtime: &mut Runtime,
    mut inputs: CallInputs,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = inputs.new_target else {
        return intl_locale_list_error(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Intl.Locale requires 'new'",
        );
    };
    let tag = inputs.arguments.take_first_or_undefined();
    let options_argument = inputs.arguments.take_first_or_undefined();
    let state = IntlLocaleConstructorContinuation {
        new_target,
        options_argument,
        options_object: None,
        locale: None,
        locale_options: LocaleOptions::default(),
        option_index: 0,
        realm,
        stage: IntlLocaleConstructorStage::AwaitTagPrimitive,
        origin,
    };
    match tag {
        StoredValue::String(tag) => {
            begin_intl_locale_after_tag(runtime, state, &tag, return_to, execution_budget)
        }
        StoredValue::Object(object) => {
            if let Some(locale) = runtime.intl_locale_value(object)?.cloned() {
                return begin_intl_locale_after_tag(
                    runtime,
                    state,
                    &locale,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                StoredValue::Object(object),
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::IntlLocaleConstructor(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        StoredValue::Function(function) => {
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                StoredValue::Function(function),
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::IntlLocaleConstructor(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        StoredValue::Undefined
        | StoredValue::Null
        | StoredValue::Boolean(_)
        | StoredValue::Number(_)
        | StoredValue::BigInt(_)
        | StoredValue::Symbol(_) => intl_locale_list_error(
            state.realm,
            state.origin,
            ExceptionKind::TypeError,
            "Intl.Locale tag is not a string or object",
        ),
    }
}

fn begin_intl_locale_after_tag(
    runtime: &mut Runtime,
    mut state: IntlLocaleConstructorContinuation,
    tag: &JsString,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.locale = Some(canonicalize_js_locale(
        tag,
        state.realm,
        &state.origin,
        execution_budget,
    )?);
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_locale_options(runtime, state, return_to, execution_budget);
    }
    let options_argument = state.options_argument.duplicate();
    state.options_object = Some(
        match to_object_value(runtime, state.realm, options_argument, state.origin.clone())? {
            Ok(options) => options,
            Err(exception) => return Err(NativeFailure::Abrupt(exception)),
        },
    );
    state.stage = IntlLocaleConstructorStage::ReadOption;
    advance_intl_locale_constructor(runtime, state, None, return_to, execution_budget)
}

#[allow(
    clippy::too_many_lines,
    reason = "the Locale constructor's observable option order stays in one resumable state machine"
)]
pub(super) fn advance_intl_locale_constructor(
    runtime: &mut Runtime,
    mut state: IntlLocaleConstructorContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            IntlLocaleConstructorStage::AwaitTagPrimitive => {
                let primitive = take_intl_locale_constructor_completion(&mut completion)?;
                let tag = operator_primitive_to_string(primitive, state.realm, &state.origin)?;
                return begin_intl_locale_after_tag(
                    runtime,
                    state,
                    &tag,
                    return_to,
                    execution_budget,
                );
            }
            IntlLocaleConstructorStage::ReadOption => {
                let Some(option) = IntlLocaleOption::ALL.get(state.option_index).copied() else {
                    return finish_intl_locale_options(runtime, state, return_to, execution_budget);
                };
                let base = state
                    .options_object
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Intl.Locale option iteration lost its options object",
                    })?
                    .duplicate();
                charge_heap_property_lookup(runtime, &base, execution_budget)?;
                let name = JsString::from_utf8(option.name())?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = IntlLocaleConstructorStage::AwaitOption;
                let dispatch = begin_value_get(
                    runtime,
                    &base,
                    key,
                    Some(&name),
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                return continue_intl_locale_constructor_after(
                    dispatch,
                    state,
                    runtime,
                    return_to,
                    execution_budget,
                );
            }
            IntlLocaleConstructorStage::AwaitOption => {
                let value = take_intl_locale_constructor_completion(&mut completion)?;
                let option = IntlLocaleOption::ALL[state.option_index];
                if matches!(value, StoredValue::Undefined) {
                    state.option_index = state.option_index.saturating_add(1);
                    state.stage = IntlLocaleConstructorStage::ReadOption;
                    continue;
                }
                if matches!(option, IntlLocaleOption::Numeric) {
                    state.locale_options.numeric = Some(value.is_truthy());
                    state.option_index = state.option_index.saturating_add(1);
                    state.stage = IntlLocaleConstructorStage::ReadOption;
                    continue;
                }
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    state.stage = IntlLocaleConstructorStage::AwaitOptionPrimitive;
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::String,
                        OperatorPrimitiveTarget::IntlLocaleConstructor(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
                store_intl_locale_option(&mut state, option, &text)?;
                state.option_index = state.option_index.saturating_add(1);
                state.stage = IntlLocaleConstructorStage::ReadOption;
            }
            IntlLocaleConstructorStage::AwaitOptionPrimitive => {
                let primitive = take_intl_locale_constructor_completion(&mut completion)?;
                let option = IntlLocaleOption::ALL[state.option_index];
                let text = operator_primitive_to_string(primitive, state.realm, &state.origin)?;
                store_intl_locale_option(&mut state, option, &text)?;
                state.option_index = state.option_index.saturating_add(1);
                state.stage = IntlLocaleConstructorStage::ReadOption;
            }
            IntlLocaleConstructorStage::AwaitPrototype => {
                let requested = take_intl_locale_constructor_completion(&mut completion)?;
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
                        let target_realm = runtime.function_realm(state.new_target)?;
                        HeapReference::Object(runtime.realm_intl_locale_prototype(target_realm)?)
                    }
                };
                let locale = state.locale.ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.Locale allocation lost its canonical locale",
                })?;
                let object = runtime.allocate_intl_locale(prototype, locale)?;
                return Ok(NativeDispatch::Immediate(StoredValue::Object(object)));
            }
        }
    }
}

fn store_intl_locale_option(
    state: &mut IntlLocaleConstructorContinuation,
    option: IntlLocaleOption,
    text: &JsString,
) -> Result<(), NativeFailure> {
    let kind = option.string_kind().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.Locale numeric option reached string validation",
    })?;
    let input = text.to_utf8_lossy()?;
    let value = canonicalize_locale_option(kind, &input).map_err(|_| {
        NativeFailure::Abrupt(PendingException {
            realm: state.realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("invalid Intl.Locale option")
                    .expect("static Intl error is valid UTF-8"),
            },
            origin: state.origin.clone(),
        })
    })?;
    option.store(&mut state.locale_options, value);
    Ok(())
}

fn finish_intl_locale_options(
    runtime: &mut Runtime,
    mut state: IntlLocaleConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let locale = state.locale.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.Locale options lost their canonical tag",
    })?;
    let input = locale.to_utf8_lossy()?;
    execution_budget.charge_instructions(u64::from(locale.len()).saturating_add(1))?;
    let canonical = apply_locale_options(&input, &state.locale_options).map_err(|_| {
        NativeFailure::Abrupt(PendingException {
            realm: state.realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("invalid language tag")
                    .expect("static Intl error is valid UTF-8"),
            },
            origin: state.origin.clone(),
        })
    })?;
    state.locale = Some(JsString::from_utf8(&canonical)?);
    state.stage = IntlLocaleConstructorStage::AwaitPrototype;
    let base = StoredValue::Function(state.new_target);
    charge_heap_property_lookup(runtime, &base, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::Prototype);
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
    continue_intl_locale_constructor_after(dispatch, state, runtime, return_to, execution_budget)
}

fn continue_intl_locale_constructor_after(
    dispatch: NativeDispatch,
    state: IntlLocaleConstructorContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        intl_locale_constructor_continuation,
        |state, value| {
            advance_intl_locale_constructor(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.Locale property Get produced a structured result",
    )
}

fn intl_locale_constructor_continuation(
    state: IntlLocaleConstructorContinuation,
) -> NativeContinuation {
    NativeContinuation::IntlLocaleConstructor(Box::new(state))
}

fn take_intl_locale_constructor_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.Locale constructor resumed without a completion",
    })
}

pub(super) fn begin_intl_locale_prototype(
    runtime: &mut Runtime,
    method: IntlLocalePrototypeMethod,
    receiver: &StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(object) = receiver else {
        return intl_locale_list_error(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Intl.Locale method called on incompatible receiver",
        );
    };
    let Some(locale) = runtime.intl_locale_value(*object)?.cloned() else {
        return intl_locale_list_error(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Intl.Locale method called on incompatible receiver",
        );
    };
    match method {
        IntlLocalePrototypeMethod::ToString => {
            Ok(NativeDispatch::Immediate(StoredValue::String(locale)))
        }
        IntlLocalePrototypeMethod::Maximize | IntlLocalePrototypeMethod::Minimize => {
            let input = locale.to_utf8_lossy()?;
            let transformed = if matches!(method, IntlLocalePrototypeMethod::Maximize) {
                maximize_locale(&input)
            } else {
                minimize_locale(&input)
            }
            .map_err(|_| EngineFault::RuntimeInvariant {
                message: "a branded Intl.Locale contained an invalid locale",
            })?;
            let prototype = HeapReference::Object(runtime.realm_intl_locale_prototype(realm)?);
            let object =
                runtime.allocate_intl_locale(prototype, JsString::from_utf8(&transformed)?)?;
            Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
        }
        method if method.is_accessor() => Ok(NativeDispatch::Immediate(
            intl_locale_accessor_value(method, &locale)?,
        )),
        IntlLocalePrototypeMethod::GetCalendars
        | IntlLocalePrototypeMethod::GetCollations
        | IntlLocalePrototypeMethod::GetHourCycles
        | IntlLocalePrototypeMethod::GetNumberingSystems
        | IntlLocalePrototypeMethod::GetTextInfo
        | IntlLocalePrototypeMethod::GetTimeZones
        | IntlLocalePrototypeMethod::GetWeekInfo => {
            intl_locale_info_dispatch(runtime, method, &locale, realm)
        }
        _ => Err(EngineFault::RuntimeInvariant {
            message: "Intl.Locale accessor escaped accessor dispatch",
        }
        .into()),
    }
}

fn intl_locale_accessor_value(
    method: IntlLocalePrototypeMethod,
    locale: &JsString,
) -> Result<StoredValue, NativeFailure> {
    let components = intl_locale_components(locale)?;
    match method {
        IntlLocalePrototypeMethod::BaseName => {
            intl_locale_component_string(Some(components.base_name))
        }
        IntlLocalePrototypeMethod::Calendar => intl_locale_component_string(components.calendar),
        IntlLocalePrototypeMethod::CaseFirst => intl_locale_component_string(components.case_first),
        IntlLocalePrototypeMethod::Collation => intl_locale_component_string(components.collation),
        IntlLocalePrototypeMethod::FirstDayOfWeek => {
            intl_locale_component_string(components.first_day_of_week)
        }
        IntlLocalePrototypeMethod::HourCycle => intl_locale_component_string(components.hour_cycle),
        IntlLocalePrototypeMethod::Language => {
            intl_locale_component_string(Some(components.language))
        }
        IntlLocalePrototypeMethod::NumberingSystem => {
            intl_locale_component_string(components.numbering_system)
        }
        IntlLocalePrototypeMethod::Numeric => Ok(StoredValue::Boolean(components.numeric)),
        IntlLocalePrototypeMethod::Region => intl_locale_component_string(components.region),
        IntlLocalePrototypeMethod::Script => intl_locale_component_string(components.script),
        IntlLocalePrototypeMethod::Variants => intl_locale_component_string(components.variants),
        _ => Err(EngineFault::RuntimeInvariant {
            message: "non-accessor Intl.Locale method reached accessor dispatch",
        }
        .into()),
    }
}

fn intl_locale_info_dispatch(
    runtime: &mut Runtime,
    method: IntlLocalePrototypeMethod,
    locale: &JsString,
    realm: RealmId,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        IntlLocalePrototypeMethod::GetCalendars => intl_locale_string_array(
            runtime,
            realm,
            intl_locale_info(locale, calendars_of_locale)?,
        ),
        IntlLocalePrototypeMethod::GetCollations => intl_locale_string_array(
            runtime,
            realm,
            intl_locale_info(locale, collations_of_locale)?,
        ),
        IntlLocalePrototypeMethod::GetHourCycles => intl_locale_string_array(
            runtime,
            realm,
            intl_locale_info(locale, hour_cycles_of_locale)?,
        ),
        IntlLocalePrototypeMethod::GetNumberingSystems => intl_locale_string_array(
            runtime,
            realm,
            intl_locale_info(locale, numbering_systems_of_locale)?,
        ),
        IntlLocalePrototypeMethod::GetTextInfo => intl_locale_text_info(
            runtime,
            realm,
            intl_locale_info(locale, text_direction_of_locale)?,
        ),
        IntlLocalePrototypeMethod::GetTimeZones => {
            match intl_locale_info(locale, time_zones_of_locale)? {
                Some(values) => intl_locale_string_array(runtime, realm, values),
                None => Ok(NativeDispatch::Immediate(StoredValue::Undefined)),
            }
        }
        IntlLocalePrototypeMethod::GetWeekInfo => intl_locale_week_info(
            runtime,
            realm,
            intl_locale_info(locale, week_info_of_locale)?,
        ),
        _ => Err(EngineFault::RuntimeInvariant {
            message: "non-info Intl.Locale method reached info dispatch",
        }
        .into()),
    }
}

fn intl_locale_components(locale: &JsString) -> Result<LocaleComponents, NativeFailure> {
    let input = locale.to_utf8_lossy()?;
    locale_components(&input)
        .map_err(|_| EngineFault::RuntimeInvariant {
            message: "a branded Intl.Locale contained an invalid locale",
        })
        .map_err(Into::into)
}

fn intl_locale_component_string(value: Option<String>) -> Result<StoredValue, NativeFailure> {
    value.map_or(Ok(StoredValue::Undefined), |value| {
        Ok(StoredValue::String(JsString::from_utf8(&value)?))
    })
}

fn intl_locale_info<T>(
    locale: &JsString,
    operation: impl FnOnce(&str) -> Result<T, quickjs_intl::InvalidLocale>,
) -> Result<T, NativeFailure> {
    operation(&locale.to_utf8_lossy()?)
        .map_err(|_| EngineFault::RuntimeInvariant {
            message: "a branded Intl.Locale contained an invalid locale",
        })
        .map_err(Into::into)
}

fn intl_locale_string_array(
    runtime: &mut Runtime,
    realm: RealmId,
    values: Vec<String>,
) -> Result<NativeDispatch, NativeFailure> {
    let mut elements = Vec::new();
    elements
        .try_reserve_exact(values.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: values.len(),
        })?;
    for value in values {
        elements.push(StoredValue::String(JsString::from_utf8(&value)?));
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(
        runtime.allocate_array(realm, elements)?,
    )))
}

fn intl_locale_text_info(
    runtime: &mut Runtime,
    realm: RealmId,
    direction: Option<&'static str>,
) -> Result<NativeDispatch, NativeFailure> {
    let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    let name = JsString::from_utf8("direction")?;
    let key = runtime.property_key_from_string(&name)?;
    let value = match direction {
        Some(direction) => StoredValue::String(JsString::from_utf8(direction)?),
        None => StoredValue::Undefined,
    };
    runtime.append_data_property(
        HeapReference::Object(object),
        key,
        PropertyLayout::data(true, true, true),
        value,
    )?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn intl_locale_week_info(
    runtime: &mut Runtime,
    realm: RealmId,
    info: LocaleWeekInfo,
) -> Result<NativeDispatch, NativeFailure> {
    let weekend = info
        .weekend
        .into_iter()
        .map(|day| StoredValue::Number(JsNumber::from_i32(i32::from(day))))
        .collect();
    let weekend = runtime.allocate_array(realm, weekend)?;
    let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    for (name, value) in [
        (
            "firstDay",
            StoredValue::Number(JsNumber::from_i32(i32::from(info.first_day))),
        ),
        ("weekend", StoredValue::Object(weekend)),
    ] {
        let name = JsString::from_utf8(name)?;
        let key = runtime.property_key_from_string(&name)?;
        runtime.append_data_property(
            HeapReference::Object(object),
            key,
            PropertyLayout::data(true, true, true),
            value,
        )?;
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(super) fn begin_intl_collator_constructor(
    runtime: &mut Runtime,
    function: FunctionId,
    mut inputs: CallInputs,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let new_target = inputs.new_target.unwrap_or(function);
    let locales = inputs.arguments.take_first_or_undefined();
    let options_argument = inputs.arguments.take_first_or_undefined();
    let state = IntlCollatorConstructorContinuation {
        new_target,
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        options: CollatorRequestOptions::default(),
        resolved: None,
        option_index: 0,
        realm,
        stage: IntlCollatorConstructorStage::ReadOption,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::CollatorConstructor(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_collator_options(
    runtime: &mut Runtime,
    mut state: IntlCollatorConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_collator_options(runtime, state, return_to, execution_budget);
    }
    let options_argument = state.options_argument.duplicate();
    state.options_object = Some(
        match to_object_value(runtime, state.realm, options_argument, state.origin.clone())? {
            Ok(options) => options,
            Err(exception) => return Err(NativeFailure::Abrupt(exception)),
        },
    );
    state.stage = IntlCollatorConstructorStage::ReadOption;
    advance_intl_collator_constructor(runtime, state, None, return_to, execution_budget)
}

#[allow(
    clippy::too_many_lines,
    reason = "Collator option reads stay in their normative observable order in one resumable state machine"
)]
pub(super) fn advance_intl_collator_constructor(
    runtime: &mut Runtime,
    mut state: IntlCollatorConstructorContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            IntlCollatorConstructorStage::ReadOption => {
                let Some(option) = IntlCollatorOption::ALL.get(state.option_index).copied() else {
                    return finish_intl_collator_options(
                        runtime,
                        state,
                        return_to,
                        execution_budget,
                    );
                };
                let base = state
                    .options_object
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Intl.Collator option iteration lost its options object",
                    })?
                    .duplicate();
                charge_heap_property_lookup(runtime, &base, execution_budget)?;
                let name = JsString::from_utf8(option.name())?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = IntlCollatorConstructorStage::AwaitOption;
                let dispatch = begin_value_get(
                    runtime,
                    &base,
                    key,
                    Some(&name),
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                return continue_intl_collator_constructor_after(
                    dispatch,
                    state,
                    runtime,
                    return_to,
                    execution_budget,
                );
            }
            IntlCollatorConstructorStage::AwaitOption => {
                let value = take_intl_collator_constructor_completion(&mut completion)?;
                let option = IntlCollatorOption::ALL[state.option_index];
                if matches!(value, StoredValue::Undefined) {
                    advance_intl_collator_option(&mut state);
                    continue;
                }
                if option.is_boolean() {
                    store_intl_collator_boolean_option(
                        &mut state.options,
                        option,
                        value.is_truthy(),
                    );
                    advance_intl_collator_option(&mut state);
                    continue;
                }
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    state.stage = IntlCollatorConstructorStage::AwaitOptionPrimitive;
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::String,
                        OperatorPrimitiveTarget::IntlCollatorConstructor(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
                store_intl_collator_string_option(&mut state, option, &text)?;
                advance_intl_collator_option(&mut state);
            }
            IntlCollatorConstructorStage::AwaitOptionPrimitive => {
                let primitive = take_intl_collator_constructor_completion(&mut completion)?;
                let option = IntlCollatorOption::ALL[state.option_index];
                let text = operator_primitive_to_string(primitive, state.realm, &state.origin)?;
                store_intl_collator_string_option(&mut state, option, &text)?;
                advance_intl_collator_option(&mut state);
            }
            IntlCollatorConstructorStage::AwaitPrototype => {
                let requested = take_intl_collator_constructor_completion(&mut completion)?;
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
                        let target_realm = runtime.function_realm(state.new_target)?;
                        HeapReference::Object(runtime.realm_intl_collator_prototype(target_realm)?)
                    }
                };
                let resolved = state.resolved.ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.Collator allocation lost its resolved slots",
                })?;
                let object = runtime.allocate_intl_collator(prototype, resolved)?;
                return Ok(NativeDispatch::Immediate(StoredValue::Object(object)));
            }
        }
    }
}

fn advance_intl_collator_option(state: &mut IntlCollatorConstructorContinuation) {
    state.option_index = state.option_index.saturating_add(1);
    state.stage = IntlCollatorConstructorStage::ReadOption;
}

fn store_intl_collator_boolean_option(
    options: &mut CollatorRequestOptions,
    option: IntlCollatorOption,
    value: bool,
) {
    match option {
        IntlCollatorOption::Numeric => options.numeric = Some(value),
        IntlCollatorOption::IgnorePunctuation => options.ignore_punctuation = Some(value),
        _ => unreachable!("a string-valued Collator option reached Boolean storage"),
    }
}

fn store_intl_collator_string_option(
    state: &mut IntlCollatorConstructorContinuation,
    option: IntlCollatorOption,
    text: &JsString,
) -> Result<(), NativeFailure> {
    let value = text.to_utf8_lossy()?;
    match option {
        IntlCollatorOption::Usage => {
            state.options.usage = Some(match value.as_str() {
                "sort" => CollatorUsage::Sort,
                "search" => CollatorUsage::Search,
                _ => return invalid_intl_collator_option(state, option),
            });
        }
        IntlCollatorOption::LocaleMatcher => {
            if !matches!(value.as_str(), "lookup" | "best fit") {
                return invalid_intl_collator_option(state, option);
            }
        }
        IntlCollatorOption::Collation => {
            let Some(collation) = canonical_collation_option(&value) else {
                return invalid_intl_collator_option(state, option);
            };
            state.options.collation = Some(collation);
        }
        IntlCollatorOption::CaseFirst => {
            if !matches!(value.as_str(), "upper" | "lower" | "false") {
                return invalid_intl_collator_option(state, option);
            }
            state.options.case_first = Some(value);
        }
        IntlCollatorOption::Sensitivity => {
            state.options.sensitivity = Some(match value.as_str() {
                "base" => CollatorSensitivity::Base,
                "accent" => CollatorSensitivity::Accent,
                "case" => CollatorSensitivity::Case,
                "variant" => CollatorSensitivity::Variant,
                _ => return invalid_intl_collator_option(state, option),
            });
        }
        IntlCollatorOption::Numeric | IntlCollatorOption::IgnorePunctuation => {
            return Err(EngineFault::RuntimeInvariant {
                message: "a Boolean-valued Collator option reached string storage",
            }
            .into());
        }
    }
    Ok(())
}

fn canonical_collation_option(value: &str) -> Option<String> {
    if value.is_empty()
        || value.split('-').any(|subtag| {
            !(3..=8).contains(&subtag.len())
                || !subtag.is_ascii()
                || !subtag
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        })
    {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

fn invalid_intl_collator_option<T>(
    state: &IntlCollatorConstructorContinuation,
    option: IntlCollatorOption,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        state.realm,
        state.origin.clone(),
        ExceptionKind::RangeError,
        &format!("invalid Intl.Collator {} option", option.name()),
    )
}

fn finish_intl_collator_options(
    runtime: &mut Runtime,
    mut state: IntlCollatorConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget
        .charge_instructions(usize_to_u64(state.requested_locales.len()).saturating_add(1))?;
    state.resolved = Some(
        resolve_collator(&state.requested_locales, state.options.clone()).map_err(|_| {
            EngineFault::RuntimeInvariant {
                message: "canonical Collator inputs failed locale resolution",
            }
        })?,
    );
    state.stage = IntlCollatorConstructorStage::AwaitPrototype;
    let base = StoredValue::Function(state.new_target);
    charge_heap_property_lookup(runtime, &base, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::Prototype);
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
    continue_intl_collator_constructor_after(dispatch, state, runtime, return_to, execution_budget)
}

fn continue_intl_collator_constructor_after(
    dispatch: NativeDispatch,
    state: IntlCollatorConstructorContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        intl_collator_constructor_continuation,
        |state, value| {
            advance_intl_collator_constructor(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.Collator property Get produced a structured result",
    )
}

fn intl_collator_constructor_continuation(
    state: IntlCollatorConstructorContinuation,
) -> NativeContinuation {
    NativeContinuation::IntlCollatorConstructor(Box::new(state))
}

fn take_intl_collator_constructor_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.Collator constructor resumed without a completion",
    })
}

pub(super) fn begin_intl_collator_supported_locales_of(
    runtime: &mut Runtime,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let locales = arguments.take_first_or_undefined();
    let options_argument = arguments.take_first_or_undefined();
    let state = IntlCollatorSupportedLocalesContinuation {
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        realm,
        stage: IntlCollatorSupportedLocalesStage::ReadLocaleMatcher,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::CollatorSupportedLocalesOf(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_collator_supported_locales_options(
    runtime: &mut Runtime,
    mut state: IntlCollatorSupportedLocalesContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_collator_supported_locales(runtime, &state);
    }
    let options_argument = state.options_argument.duplicate();
    state.options_object = Some(
        match to_object_value(runtime, state.realm, options_argument, state.origin.clone())? {
            Ok(options) => options,
            Err(exception) => return Err(NativeFailure::Abrupt(exception)),
        },
    );
    state.stage = IntlCollatorSupportedLocalesStage::ReadLocaleMatcher;
    advance_intl_collator_supported_locales(runtime, state, None, return_to, execution_budget)
}

pub(super) fn advance_intl_collator_supported_locales(
    runtime: &mut Runtime,
    mut state: IntlCollatorSupportedLocalesContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    match state.stage {
        IntlCollatorSupportedLocalesStage::ReadLocaleMatcher => {
            let base = state
                .options_object
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.Collator.supportedLocalesOf lost its options object",
                })?
                .duplicate();
            charge_heap_property_lookup(runtime, &base, execution_budget)?;
            let name = JsString::from_utf8("localeMatcher")?;
            let key = runtime.property_key_from_string(&name)?;
            state.stage = IntlCollatorSupportedLocalesStage::AwaitLocaleMatcher;
            let dispatch = begin_value_get(
                runtime,
                &base,
                key,
                Some(&name),
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            )?;
            continue_intl_collator_supported_locales_after(
                dispatch,
                state,
                runtime,
                return_to,
                execution_budget,
            )
        }
        IntlCollatorSupportedLocalesStage::AwaitLocaleMatcher => {
            let value = take_intl_collator_supported_locales_completion(&mut completion)?;
            if matches!(value, StoredValue::Undefined) {
                return finish_intl_collator_supported_locales(runtime, &state);
            }
            if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                state.stage = IntlCollatorSupportedLocalesStage::AwaitLocaleMatcherPrimitive;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::IntlCollatorSupportedLocalesOf(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            let value = operator_primitive_to_string(value, state.realm, &state.origin)?;
            validate_intl_collator_locale_matcher(&state, &value)?;
            finish_intl_collator_supported_locales(runtime, &state)
        }
        IntlCollatorSupportedLocalesStage::AwaitLocaleMatcherPrimitive => {
            let value = take_intl_collator_supported_locales_completion(&mut completion)?;
            let value = operator_primitive_to_string(value, state.realm, &state.origin)?;
            validate_intl_collator_locale_matcher(&state, &value)?;
            finish_intl_collator_supported_locales(runtime, &state)
        }
    }
}

fn validate_intl_collator_locale_matcher(
    state: &IntlCollatorSupportedLocalesContinuation,
    value: &JsString,
) -> Result<(), NativeFailure> {
    if matches!(value.to_utf8_lossy()?.as_str(), "lookup" | "best fit") {
        return Ok(());
    }
    intl_locale_list_error(
        state.realm,
        state.origin.clone(),
        ExceptionKind::RangeError,
        "invalid Intl.Collator localeMatcher option",
    )
}

fn finish_intl_collator_supported_locales(
    runtime: &mut Runtime,
    state: &IntlCollatorSupportedLocalesContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    intl_locale_string_array(
        runtime,
        state.realm,
        collator_supported_locales(&state.requested_locales),
    )
}

fn continue_intl_collator_supported_locales_after(
    dispatch: NativeDispatch,
    state: IntlCollatorSupportedLocalesContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        intl_collator_supported_locales_continuation,
        |state, value| {
            advance_intl_collator_supported_locales(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.Collator.supportedLocalesOf property Get produced a structured result",
    )
}

fn intl_collator_supported_locales_continuation(
    state: IntlCollatorSupportedLocalesContinuation,
) -> NativeContinuation {
    NativeContinuation::IntlCollatorSupportedLocalesOf(Box::new(state))
}

fn take_intl_collator_supported_locales_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.Collator.supportedLocalesOf resumed without a completion",
    })
}

pub(super) fn begin_intl_collator_prototype(
    runtime: &mut Runtime,
    method: IntlCollatorPrototypeMethod,
    receiver: &StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(object) = receiver else {
        return intl_collator_brand_error(realm, origin);
    };
    let Some(state) = runtime.intl_collator_state(*object)?.cloned() else {
        return intl_collator_brand_error(realm, origin);
    };
    match method {
        IntlCollatorPrototypeMethod::Compare => {
            let function = match runtime.intl_collator_bound_compare(*object)? {
                Some(function) => function,
                None => runtime.allocate_intl_collator_bound_compare(realm, *object)?,
            };
            Ok(NativeDispatch::Immediate(StoredValue::Function(function)))
        }
        IntlCollatorPrototypeMethod::ResolvedOptions => {
            intl_collator_resolved_options(runtime, realm, &state)
        }
    }
}

fn intl_collator_resolved_options(
    runtime: &mut Runtime,
    realm: RealmId,
    state: &CollatorState,
) -> Result<NativeDispatch, NativeFailure> {
    let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    let properties = [
        (
            "locale",
            StoredValue::String(JsString::from_utf8(&state.locale)?),
        ),
        (
            "usage",
            StoredValue::String(JsString::from_utf8(state.usage.as_str())?),
        ),
        (
            "sensitivity",
            StoredValue::String(JsString::from_utf8(state.sensitivity.as_str())?),
        ),
        (
            "ignorePunctuation",
            StoredValue::Boolean(state.ignore_punctuation),
        ),
        (
            "collation",
            StoredValue::String(JsString::from_utf8(&state.collation)?),
        ),
        ("numeric", StoredValue::Boolean(state.numeric)),
        (
            "caseFirst",
            StoredValue::String(JsString::from_utf8(&state.case_first)?),
        ),
    ];
    for (name, value) in properties {
        let name = JsString::from_utf8(name)?;
        let key = runtime.property_key_from_string(&name)?;
        runtime.append_data_property(
            HeapReference::Object(object),
            key,
            PropertyLayout::data(true, true, true),
            value,
        )?;
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn intl_collator_brand_error<T>(realm: RealmId, origin: JsStackFrame) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        realm,
        origin,
        ExceptionKind::TypeError,
        "Intl.Collator method called on incompatible receiver",
    )
}

pub(super) fn begin_intl_collator_compare(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(collator) = receiver else {
        return intl_collator_brand_error(realm, origin);
    };
    if runtime.intl_collator_state(*collator)?.is_none() {
        return intl_collator_brand_error(realm, origin);
    }
    let first = arguments.take_first_or_undefined();
    let second = arguments.take_first_or_undefined();
    let state = IntlCollatorCompareContinuation {
        collator: *collator,
        second,
        first: None,
        realm,
        origin,
    };
    begin_intl_collator_compare_first(runtime, state, first, return_to, execution_budget)
}

fn begin_intl_collator_compare_first(
    runtime: &mut Runtime,
    mut state: IntlCollatorCompareContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
        let realm = state.realm;
        let origin = state.origin.clone();
        return begin_operator_primitive_conversion(
            runtime,
            value,
            OperatorPrimitiveHint::String,
            OperatorPrimitiveTarget::IntlCollatorCompareFirst(Box::new(state)),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    state.first = Some(operator_primitive_to_string(
        value,
        state.realm,
        &state.origin,
    )?);
    begin_intl_collator_compare_second(runtime, state, return_to, execution_budget)
}

pub(super) fn finish_intl_collator_compare_first(
    runtime: &mut Runtime,
    mut state: IntlCollatorCompareContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.first = Some(operator_primitive_to_string(
        value,
        state.realm,
        &state.origin,
    )?);
    begin_intl_collator_compare_second(runtime, state, return_to, execution_budget)
}

fn begin_intl_collator_compare_second(
    runtime: &mut Runtime,
    state: IntlCollatorCompareContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(
        state.second,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        let value = state.second.duplicate();
        let realm = state.realm;
        let origin = state.origin.clone();
        return begin_operator_primitive_conversion(
            runtime,
            value,
            OperatorPrimitiveHint::String,
            OperatorPrimitiveTarget::IntlCollatorCompareSecond(Box::new(state)),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    let value = state.second.duplicate();
    finish_intl_collator_compare_second(runtime, state, value)
}

pub(super) fn finish_intl_collator_compare_second(
    runtime: &Runtime,
    state: IntlCollatorCompareContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let first = state.first.ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.Collator comparison lost its first string",
    })?;
    let second = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let resolved =
        runtime
            .intl_collator_state(state.collator)?
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Intl.Collator comparison lost its branded receiver",
            })?;
    let ordering =
        compare_with_collator(resolved, &first.to_utf8_lossy()?, &second.to_utf8_lossy()?)
            .map_err(|_| EngineFault::RuntimeInvariant {
                message: "resolved Intl.Collator slots failed ICU comparison",
            })?;
    let result = match ordering {
        core::cmp::Ordering::Less => -1,
        core::cmp::Ordering::Equal => 0,
        core::cmp::Ordering::Greater => 1,
    };
    Ok(NativeDispatch::Immediate(StoredValue::Number(
        JsNumber::from_i32(result),
    )))
}

pub(super) fn begin_intl_get_canonical_locales(
    runtime: &mut Runtime,
    locales: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::ReturnArray,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_locale_list(
    runtime: &mut Runtime,
    locales: StoredValue,
    target: IntlLocaleListTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match locales {
        StoredValue::Undefined => finish_intl_locale_list(
            runtime,
            realm,
            Vec::new(),
            target,
            return_to,
            execution_budget,
        ),
        StoredValue::String(locale) => {
            let canonical = canonicalize_js_locale(&locale, realm, &origin, execution_budget)?;
            finish_intl_locale_list(
                runtime,
                realm,
                intl_one_value(StoredValue::String(canonical))?,
                target,
                return_to,
                execution_budget,
            )
        }
        StoredValue::Object(object) if runtime.intl_locale_value(object)?.is_some() => {
            let locale = runtime.intl_locale_value(object)?.cloned().ok_or(
                EngineFault::RuntimeInvariant {
                    message: "Intl.Locale slot disappeared during CanonicalizeLocaleList",
                },
            )?;
            finish_intl_locale_list(
                runtime,
                realm,
                intl_one_value(StoredValue::String(locale))?,
                target,
                return_to,
                execution_budget,
            )
        }
        value => {
            let source = match to_object_value(runtime, realm, value, origin.clone())? {
                Ok(source) => source,
                Err(exception) => return Err(NativeFailure::Abrupt(exception)),
            };
            let state = IntlLocaleListContinuation {
                source,
                seen: Vec::new(),
                index: 0,
                length: 0,
                realm,
                stage: IntlLocaleListStage::AwaitLength,
                target,
                origin,
            };
            read_intl_locale_property(
                runtime,
                state,
                runtime.predefined_property_key(PredefinedAtom::Length),
                "length",
                return_to,
                execution_budget,
            )
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the typed resumable CanonicalizeLocaleList state transitions stay together for ordering audits"
)]
pub(super) fn advance_intl_locale_list(
    runtime: &mut Runtime,
    mut state: IntlLocaleListContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            IntlLocaleListStage::AwaitLength => {
                let value = take_intl_completion(&mut completion)?;
                state.stage = IntlLocaleListStage::AwaitLengthConversion;
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::Number,
                        OperatorPrimitiveTarget::IntlLocaleListLength(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                return finish_intl_locale_list_length(
                    runtime,
                    state,
                    value,
                    return_to,
                    execution_budget,
                );
            }
            IntlLocaleListStage::AwaitLengthConversion => {
                return finish_intl_locale_list_length(
                    runtime,
                    state,
                    take_intl_completion(&mut completion)?,
                    return_to,
                    execution_budget,
                );
            }
            IntlLocaleListStage::Next => {
                if state.index >= state.length {
                    return finish_intl_locale_list(
                        runtime,
                        state.realm,
                        state.seen,
                        state.target,
                        return_to,
                        execution_budget,
                    );
                }
                execution_budget.charge_instructions(1)?;
                let key = array_static_index_key(runtime, state.index)?;
                state.stage = IntlLocaleListStage::AwaitHas;
                return has_intl_locale_property(runtime, state, key, return_to, execution_budget);
            }
            IntlLocaleListStage::AwaitHas => {
                let StoredValue::Boolean(present) = take_intl_completion(&mut completion)? else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "CanonicalizeLocaleList HasProperty returned a non-Boolean",
                    }
                    .into());
                };
                if !present {
                    state.index = state.index.saturating_add(1);
                    state.stage = IntlLocaleListStage::Next;
                    continue;
                }
                let key = array_static_index_key(runtime, state.index)?;
                state.stage = IntlLocaleListStage::AwaitElement;
                return read_intl_locale_property(
                    runtime,
                    state,
                    key,
                    "locale",
                    return_to,
                    execution_budget,
                );
            }
            IntlLocaleListStage::AwaitElement => {
                let value = take_intl_completion(&mut completion)?;
                if let StoredValue::Object(object) = &value
                    && let Some(locale) = runtime.intl_locale_value(*object)?.cloned()
                {
                    return finish_intl_locale_list_element(
                        runtime,
                        state,
                        &locale,
                        return_to,
                        execution_budget,
                    );
                }
                match value {
                    StoredValue::String(locale) => {
                        return finish_intl_locale_list_element(
                            runtime,
                            state,
                            &locale,
                            return_to,
                            execution_budget,
                        );
                    }
                    value @ (StoredValue::Function(_) | StoredValue::Object(_)) => {
                        state.stage = IntlLocaleListStage::AwaitElementString;
                        let realm = state.realm;
                        let origin = state.origin.clone();
                        return begin_operator_primitive_conversion(
                            runtime,
                            value,
                            OperatorPrimitiveHint::String,
                            OperatorPrimitiveTarget::IntlLocaleListElement(Box::new(state)),
                            realm,
                            return_to,
                            origin,
                            execution_budget,
                        );
                    }
                    StoredValue::Undefined
                    | StoredValue::Null
                    | StoredValue::Boolean(_)
                    | StoredValue::Number(_)
                    | StoredValue::BigInt(_)
                    | StoredValue::Symbol(_) => {
                        return intl_locale_list_error(
                            state.realm,
                            state.origin,
                            ExceptionKind::TypeError,
                            "locale list element is not a string or object",
                        );
                    }
                }
            }
            IntlLocaleListStage::AwaitElementString => {
                let value = take_intl_completion(&mut completion)?;
                let locale = operator_primitive_to_string(value, state.realm, &state.origin)?;
                return finish_intl_locale_list_element(
                    runtime,
                    state,
                    &locale,
                    return_to,
                    execution_budget,
                );
            }
        }
    }
}

pub(super) fn finish_intl_locale_list_length(
    runtime: &mut Runtime,
    mut state: IntlLocaleListContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.length = number_to_length(operator_to_number(value, state.realm, &state.origin)?);
    state.stage = IntlLocaleListStage::Next;
    advance_intl_locale_list(runtime, state, None, return_to, execution_budget)
}

pub(super) fn finish_intl_locale_list_element(
    runtime: &mut Runtime,
    mut state: IntlLocaleListContinuation,
    locale: &JsString,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let canonical = canonicalize_js_locale(locale, state.realm, &state.origin, execution_budget)?;
    if !state
        .seen
        .iter()
        .any(|candidate| matches!(candidate, StoredValue::String(value) if value == &canonical))
    {
        state
            .seen
            .try_reserve(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::Frames,
                additional: 1,
            })?;
        state.seen.push(StoredValue::String(canonical));
    }
    state.index = state.index.saturating_add(1);
    state.stage = IntlLocaleListStage::Next;
    advance_intl_locale_list(runtime, state, None, return_to, execution_budget)
}

fn canonicalize_js_locale(
    locale: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<JsString, NativeFailure> {
    execution_budget.charge_instructions(u64::from(locale.len()).saturating_add(1))?;
    let input = locale.to_utf8_lossy()?;
    let canonical = canonicalize_locale(&input).map_err(|_| {
        NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("invalid language tag")
                    .expect("static Intl error message is valid"),
            },
            origin: origin.clone(),
        })
    })?;
    Ok(JsString::from_utf8(&canonical)?)
}

fn finish_intl_locale_list(
    runtime: &mut Runtime,
    realm: RealmId,
    locales: Vec<StoredValue>,
    target: IntlLocaleListTarget,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match target {
        IntlLocaleListTarget::ReturnArray => Ok(NativeDispatch::Immediate(StoredValue::Object(
            runtime.allocate_array(realm, locales)?,
        ))),
        IntlLocaleListTarget::CollatorConstructor(mut state) => {
            state.requested_locales = intl_locale_strings(locales)?;
            begin_intl_collator_options(runtime, *state, return_to, execution_budget)
        }
        IntlLocaleListTarget::CollatorSupportedLocalesOf(mut state) => {
            state.requested_locales = intl_locale_strings(locales)?;
            begin_intl_collator_supported_locales_options(
                runtime,
                *state,
                return_to,
                execution_budget,
            )
        }
    }
}

fn intl_locale_strings(locales: Vec<StoredValue>) -> Result<Vec<String>, NativeFailure> {
    let mut strings = Vec::new();
    strings
        .try_reserve_exact(locales.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: locales.len(),
        })?;
    for locale in locales {
        let StoredValue::String(locale) = locale else {
            return Err(EngineFault::RuntimeInvariant {
                message: "CanonicalizeLocaleList retained a non-string locale",
            }
            .into());
        };
        strings.push(locale.to_utf8_lossy()?);
    }
    Ok(strings)
}

fn read_intl_locale_property(
    runtime: &mut Runtime,
    state: IntlLocaleListContinuation,
    key: PropertyKey,
    diagnostic_name: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = state.source.duplicate();
    charge_heap_property_lookup(runtime, &base, execution_budget)?;
    let name = JsString::from_utf8(diagnostic_name)?;
    let dispatch = begin_value_get(
        runtime,
        &base,
        key,
        Some(&name),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_intl_locale_after(dispatch, state, runtime, return_to, execution_budget)
}

fn has_intl_locale_property(
    runtime: &mut Runtime,
    state: IntlLocaleListContinuation,
    key: PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let base = state.source.duplicate();
    charge_heap_property_lookup(runtime, &base, execution_budget)?;
    let dispatch = begin_value_has(
        runtime,
        &base,
        key,
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_intl_locale_after(dispatch, state, runtime, return_to, execution_budget)
}

fn continue_intl_locale_after(
    dispatch: NativeDispatch,
    state: IntlLocaleListContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        intl_locale_list_continuation,
        |state, value| {
            advance_intl_locale_list(runtime, state, Some(value), return_to, execution_budget)
        },
        "CanonicalizeLocaleList property operation produced a structured result",
    )
}

fn intl_locale_list_continuation(state: IntlLocaleListContinuation) -> NativeContinuation {
    NativeContinuation::IntlLocaleList(Box::new(state))
}

fn take_intl_completion(completion: &mut Option<StoredValue>) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "CanonicalizeLocaleList resumed without a completion",
    })
}

fn intl_one_value(value: StoredValue) -> Result<Vec<StoredValue>, NativeFailure> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::Frames,
            additional: 1,
        })?;
    values.push(value);
    Ok(values)
}

fn intl_locale_list_error<T>(
    realm: RealmId,
    origin: JsStackFrame,
    kind: ExceptionKind,
    message: &str,
) -> Result<T, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}
