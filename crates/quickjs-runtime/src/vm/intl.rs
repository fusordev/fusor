//! Resumable ECMA-402 locale-list canonicalization.

use super::instanceof::begin_function_has_instance;
#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

use quickjs_intl::{
    CollatorRequestOptions, CollatorSensitivity, CollatorState, CollatorUsage,
    IntlMathematicalValue, LocaleComponents, LocaleOptionKind, LocaleOptions, LocaleWeekInfo,
    NumberFormatCompactDisplay, NumberFormatCurrencyDisplay, NumberFormatCurrencySign,
    NumberFormatError, NumberFormatNotation, NumberFormatRequestOptions, NumberFormatRoundingMode,
    NumberFormatRoundingPriority, NumberFormatSignDisplay, NumberFormatState, NumberFormatStyle,
    NumberFormatTrailingZeroDisplay, NumberFormatUnitDisplay, NumberFormatUseGrouping,
    apply_locale_options, calendars_of_locale, canonicalize_locale, canonicalize_locale_option,
    collations_of_locale, collator_supported_locales, compare_with_collator, format_number,
    format_number_to_parts, hour_cycles_of_locale, intl_mathematical_value_from_f64,
    is_well_formed_currency_code, is_well_formed_unit_identifier, locale_components,
    maximize_locale, minimize_locale, number_format_supported_locales, numbering_systems_of_locale,
    parse_intl_mathematical_value, resolve_collator, resolve_number_format, supported_values,
    text_direction_of_locale, time_zones_of_locale, week_info_of_locale,
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
    NumberFormatConstructor(Box<IntlNumberFormatConstructorContinuation>),
    NumberFormatSupportedLocalesOf(Box<IntlNumberFormatSupportedLocalesContinuation>),
}

impl IntlLocaleListTarget {
    fn retained_values(&self) -> u64 {
        match self {
            Self::ReturnArray => 0,
            Self::CollatorConstructor(state) => state.retained_values(),
            Self::CollatorSupportedLocalesOf(state) => state.retained_values(),
            Self::NumberFormatConstructor(state) => state.retained_values(),
            Self::NumberFormatSupportedLocalesOf(state) => state.retained_values(),
        }
    }

    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::ReturnArray => {}
            Self::CollatorConstructor(state) => state.trace_roots(mark),
            Self::CollatorSupportedLocalesOf(state) => state.trace_roots(mark),
            Self::NumberFormatConstructor(state) => state.trace_roots(mark),
            Self::NumberFormatSupportedLocalesOf(state) => state.trace_roots(mark),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlNumberFormatConstructorStage {
    ReadOption,
    AwaitOption,
    AwaitOptionPrimitive,
    ConvertRawDigit,
    AwaitRawDigitPrimitive,
    AwaitPrototype,
    AwaitLegacyInstance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlNumberFormatOption {
    LocaleMatcher,
    NumberingSystem,
    Style,
    Currency,
    CurrencyDisplay,
    CurrencySign,
    Unit,
    UnitDisplay,
    Notation,
    MinimumIntegerDigits,
    MinimumFractionDigits,
    MaximumFractionDigits,
    MinimumSignificantDigits,
    MaximumSignificantDigits,
    RoundingIncrement,
    RoundingMode,
    RoundingPriority,
    TrailingZeroDisplay,
    CompactDisplay,
    UseGrouping,
    SignDisplay,
}

impl IntlNumberFormatOption {
    const ALL: [Self; 21] = [
        Self::LocaleMatcher,
        Self::NumberingSystem,
        Self::Style,
        Self::Currency,
        Self::CurrencyDisplay,
        Self::CurrencySign,
        Self::Unit,
        Self::UnitDisplay,
        Self::Notation,
        Self::MinimumIntegerDigits,
        Self::MinimumFractionDigits,
        Self::MaximumFractionDigits,
        Self::MinimumSignificantDigits,
        Self::MaximumSignificantDigits,
        Self::RoundingIncrement,
        Self::RoundingMode,
        Self::RoundingPriority,
        Self::TrailingZeroDisplay,
        Self::CompactDisplay,
        Self::UseGrouping,
        Self::SignDisplay,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::LocaleMatcher => "localeMatcher",
            Self::NumberingSystem => "numberingSystem",
            Self::Style => "style",
            Self::Currency => "currency",
            Self::CurrencyDisplay => "currencyDisplay",
            Self::CurrencySign => "currencySign",
            Self::Unit => "unit",
            Self::UnitDisplay => "unitDisplay",
            Self::Notation => "notation",
            Self::MinimumIntegerDigits => "minimumIntegerDigits",
            Self::MinimumFractionDigits => "minimumFractionDigits",
            Self::MaximumFractionDigits => "maximumFractionDigits",
            Self::MinimumSignificantDigits => "minimumSignificantDigits",
            Self::MaximumSignificantDigits => "maximumSignificantDigits",
            Self::RoundingIncrement => "roundingIncrement",
            Self::RoundingMode => "roundingMode",
            Self::RoundingPriority => "roundingPriority",
            Self::TrailingZeroDisplay => "trailingZeroDisplay",
            Self::CompactDisplay => "compactDisplay",
            Self::UseGrouping => "useGrouping",
            Self::SignDisplay => "signDisplay",
        }
    }

    const fn raw_digit_index(self) -> Option<usize> {
        match self {
            Self::MinimumFractionDigits => Some(0),
            Self::MaximumFractionDigits => Some(1),
            Self::MinimumSignificantDigits => Some(2),
            Self::MaximumSignificantDigits => Some(3),
            _ => None,
        }
    }

    const fn is_immediate_number(self) -> bool {
        matches!(self, Self::MinimumIntegerDigits | Self::RoundingIncrement)
    }
}

pub(super) struct IntlNumberFormatConstructorContinuation {
    new_target: Option<FunctionId>,
    format_value: Option<IntlMathematicalValue>,
    legacy_receiver: Option<StoredValue>,
    legacy_number_format: Option<ObjectId>,
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    options: NumberFormatRequestOptions,
    raw_digits: [Option<StoredValue>; 4],
    resolved: Option<NumberFormatState>,
    option_index: usize,
    raw_digit_index: usize,
    realm: RealmId,
    stage: IntlNumberFormatConstructorStage,
    origin: JsStackFrame,
}

impl IntlNumberFormatConstructorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.options_object.is_some()))
            .saturating_add(u64::from(self.legacy_receiver.is_some()))
            .saturating_add(u64::from(self.legacy_number_format.is_some()))
            .saturating_add(
                self.raw_digits
                    .iter()
                    .filter(|value| value.is_some())
                    .count() as u64,
            )
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        if let Some(new_target) = self.new_target {
            mark(CollectionRoot::Heap(HeapReference::Function(new_target)));
        }
        trace_stored_value_root(&self.options_argument, mark);
        if let Some(receiver) = &self.legacy_receiver {
            trace_stored_value_root(receiver, mark);
        }
        if let Some(number_format) = self.legacy_number_format {
            mark(CollectionRoot::Heap(HeapReference::Object(number_format)));
        }
        if let Some(options) = &self.options_object {
            trace_stored_value_root(options, mark);
        }
        for value in self.raw_digits.iter().flatten() {
            trace_stored_value_root(value, mark);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlNumberFormatSupportedLocalesStage {
    ReadLocaleMatcher,
    AwaitLocaleMatcher,
    AwaitLocaleMatcherPrimitive,
}

pub(super) struct IntlNumberFormatSupportedLocalesContinuation {
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    realm: RealmId,
    stage: IntlNumberFormatSupportedLocalesStage,
    origin: JsStackFrame,
}

impl IntlNumberFormatSupportedLocalesContinuation {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IntlNumberFormatOperation {
    Format,
    FormatToParts,
    FormatRange,
    FormatRangeToParts,
}

pub(super) struct IntlNumberFormatValueContinuation {
    formatter: ObjectId,
    operation: IntlNumberFormatOperation,
    second: Option<StoredValue>,
    first: Option<IntlMathematicalValue>,
    realm: RealmId,
    origin: JsStackFrame,
}

impl IntlNumberFormatValueContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(u64::from(self.second.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.formatter)));
        if let Some(second) = &self.second {
            trace_stored_value_root(second, mark);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlNumberFormatUnwrapStage {
    AwaitInstance,
    AwaitFallback,
}

pub(super) struct IntlNumberFormatUnwrapContinuation {
    receiver: StoredValue,
    method: IntlNumberFormatPrototypeMethod,
    realm: RealmId,
    stage: IntlNumberFormatUnwrapStage,
    origin: JsStackFrame,
}

impl IntlNumberFormatUnwrapContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.receiver, mark);
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

pub(super) fn begin_intl_number_format_constructor(
    runtime: &mut Runtime,
    function: FunctionId,
    mut inputs: CallInputs,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let new_target = inputs.new_target.unwrap_or(function);
    let legacy_receiver = inputs
        .new_target
        .is_none()
        .then(|| inputs.receiver.duplicate());
    let locales = inputs.arguments.take_first_or_undefined();
    let options_argument = inputs.arguments.take_first_or_undefined();
    let state = IntlNumberFormatConstructorContinuation {
        new_target: Some(new_target),
        format_value: None,
        legacy_receiver,
        legacy_number_format: None,
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        options: NumberFormatRequestOptions::default(),
        raw_digits: core::array::from_fn(|_| None),
        resolved: None,
        option_index: 0,
        raw_digit_index: 0,
        realm,
        stage: IntlNumberFormatConstructorStage::ReadOption,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::NumberFormatConstructor(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn begin_intl_number_to_locale_string(
    runtime: &mut Runtime,
    mut arguments: CallArguments,
    value: IntlMathematicalValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let locales = arguments.take_first_or_undefined();
    let options_argument = arguments.take_first_or_undefined();
    let state = IntlNumberFormatConstructorContinuation {
        new_target: None,
        format_value: Some(value),
        legacy_receiver: None,
        legacy_number_format: None,
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        options: NumberFormatRequestOptions::default(),
        raw_digits: core::array::from_fn(|_| None),
        resolved: None,
        option_index: 0,
        raw_digit_index: 0,
        realm,
        stage: IntlNumberFormatConstructorStage::ReadOption,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::NumberFormatConstructor(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_number_format_options(
    runtime: &mut Runtime,
    mut state: IntlNumberFormatConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        state.stage = IntlNumberFormatConstructorStage::ConvertRawDigit;
        return advance_intl_number_format_constructor(
            runtime,
            state,
            None,
            return_to,
            execution_budget,
        );
    }
    let options_argument = state.options_argument.duplicate();
    state.options_object = Some(
        match to_object_value(runtime, state.realm, options_argument, state.origin.clone())? {
            Ok(options) => options,
            Err(exception) => return Err(NativeFailure::Abrupt(exception)),
        },
    );
    state.stage = IntlNumberFormatConstructorStage::ReadOption;
    advance_intl_number_format_constructor(runtime, state, None, return_to, execution_budget)
}

#[allow(
    clippy::too_many_lines,
    reason = "NumberFormat option Gets and delayed digit conversions remain in normative observable order"
)]
pub(super) fn advance_intl_number_format_constructor(
    runtime: &mut Runtime,
    mut state: IntlNumberFormatConstructorContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            IntlNumberFormatConstructorStage::ReadOption => {
                let Some(option) = IntlNumberFormatOption::ALL.get(state.option_index).copied()
                else {
                    state.stage = IntlNumberFormatConstructorStage::ConvertRawDigit;
                    continue;
                };
                let base = state
                    .options_object
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Intl.NumberFormat option iteration lost its options object",
                    })?
                    .duplicate();
                charge_heap_property_lookup(runtime, &base, execution_budget)?;
                let name = JsString::from_utf8(option.name())?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = IntlNumberFormatConstructorStage::AwaitOption;
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
                return continue_intl_number_format_constructor_after(
                    dispatch,
                    state,
                    runtime,
                    return_to,
                    execution_budget,
                );
            }
            IntlNumberFormatConstructorStage::AwaitOption => {
                let value = take_intl_number_format_constructor_completion(&mut completion)?;
                let option = IntlNumberFormatOption::ALL[state.option_index];
                if matches!(value, StoredValue::Undefined) {
                    validate_required_number_format_option(&state, option)?;
                    advance_intl_number_format_option(&mut state);
                    continue;
                }
                if let Some(index) = option.raw_digit_index() {
                    state.raw_digits[index] = Some(value);
                    advance_intl_number_format_option(&mut state);
                    continue;
                }
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    state.stage = IntlNumberFormatConstructorStage::AwaitOptionPrimitive;
                    let hint = if option.is_immediate_number() {
                        OperatorPrimitiveHint::Number
                    } else {
                        OperatorPrimitiveHint::String
                    };
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        hint,
                        OperatorPrimitiveTarget::IntlNumberFormatConstructor(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                store_intl_number_format_option(&mut state, option, value)?;
                advance_intl_number_format_option(&mut state);
            }
            IntlNumberFormatConstructorStage::AwaitOptionPrimitive => {
                let primitive = take_intl_number_format_constructor_completion(&mut completion)?;
                let option = IntlNumberFormatOption::ALL[state.option_index];
                store_intl_number_format_option(&mut state, option, primitive)?;
                advance_intl_number_format_option(&mut state);
            }
            IntlNumberFormatConstructorStage::ConvertRawDigit => {
                let Some(value) = state
                    .raw_digits
                    .get_mut(state.raw_digit_index)
                    .and_then(Option::take)
                else {
                    state.raw_digit_index = state.raw_digit_index.saturating_add(1);
                    if state.raw_digit_index >= state.raw_digits.len() {
                        return finish_intl_number_format_options(
                            runtime,
                            state,
                            return_to,
                            execution_budget,
                        );
                    }
                    continue;
                };
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    state.stage = IntlNumberFormatConstructorStage::AwaitRawDigitPrimitive;
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::Number,
                        OperatorPrimitiveTarget::IntlNumberFormatConstructor(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                store_intl_number_format_raw_digit(&mut state, value)?;
                state.raw_digit_index = state.raw_digit_index.saturating_add(1);
            }
            IntlNumberFormatConstructorStage::AwaitRawDigitPrimitive => {
                let primitive = take_intl_number_format_constructor_completion(&mut completion)?;
                store_intl_number_format_raw_digit(&mut state, primitive)?;
                state.raw_digit_index = state.raw_digit_index.saturating_add(1);
                state.stage = IntlNumberFormatConstructorStage::ConvertRawDigit;
            }
            IntlNumberFormatConstructorStage::AwaitPrototype => {
                let new_target = state.new_target.ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.NumberFormat allocation lost its new target",
                })?;
                let requested = take_intl_number_format_constructor_completion(&mut completion)?;
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
                        let target_realm = runtime.function_realm(new_target)?;
                        HeapReference::Object(
                            runtime.realm_intl_number_format_prototype(target_realm)?,
                        )
                    }
                };
                let resolved = state.resolved.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.NumberFormat allocation lost its resolved slots",
                })?;
                let object = runtime.allocate_intl_number_format(prototype, resolved)?;
                if let Some(receiver) = state.legacy_receiver.as_ref().map(StoredValue::duplicate) {
                    state.legacy_number_format = Some(object);
                    state.stage = IntlNumberFormatConstructorStage::AwaitLegacyInstance;
                    let constructor = runtime.realm_intl_number_format_constructor(state.realm)?;
                    let dispatch = begin_function_has_instance(
                        runtime,
                        state.realm,
                        receiver,
                        StoredValue::Function(constructor),
                        return_to,
                        state.origin.clone(),
                        execution_budget,
                    )?;
                    return continue_intl_number_format_constructor_after(
                        dispatch,
                        state,
                        runtime,
                        return_to,
                        execution_budget,
                    );
                }
                return Ok(NativeDispatch::Immediate(StoredValue::Object(object)));
            }
            IntlNumberFormatConstructorStage::AwaitLegacyInstance => {
                let completion = take_intl_number_format_constructor_completion(&mut completion)?;
                let number_format =
                    state
                        .legacy_number_format
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "Intl.NumberFormat legacy chain lost its initialized object",
                        })?;
                if !completion.is_truthy() {
                    return Ok(NativeDispatch::Immediate(StoredValue::Object(
                        number_format,
                    )));
                }
                let receiver = state.legacy_receiver.ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.NumberFormat legacy chain lost its receiver",
                })?;
                let reference = receiver.heap_reference().ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.NumberFormat legacy receiver passed instanceof as a primitive",
                })?;
                let symbol = runtime.intl_number_format_fallback_symbol();
                let key = runtime.property_key_from_symbol(&symbol)?;
                let definition = PropertyDefinition::data(
                    Requested::Present(StoredValue::Object(number_format)),
                    Requested::Present(false),
                )
                .with_enumerable(Requested::Present(false))
                .with_configurable(Requested::Present(false));
                return begin_internal_define_own_property(
                    runtime,
                    reference,
                    key,
                    definition,
                    state.realm,
                    return_to,
                    state.origin,
                    execution_budget,
                    DefinePropertyResult::Target,
                );
            }
        }
    }
}

fn advance_intl_number_format_option(state: &mut IntlNumberFormatConstructorContinuation) {
    state.option_index = state.option_index.saturating_add(1);
    state.stage = IntlNumberFormatConstructorStage::ReadOption;
}

fn validate_required_number_format_option(
    state: &IntlNumberFormatConstructorContinuation,
    option: IntlNumberFormatOption,
) -> Result<(), NativeFailure> {
    let style = state.options.style.unwrap_or_default();
    if (option == IntlNumberFormatOption::Currency && style == NumberFormatStyle::Currency)
        || (option == IntlNumberFormatOption::Unit && style == NumberFormatStyle::Unit)
    {
        return intl_locale_list_error(
            state.realm,
            state.origin.clone(),
            ExceptionKind::TypeError,
            if option == IntlNumberFormatOption::Currency {
                "currency is required with currency style"
            } else {
                "unit is required with unit style"
            },
        );
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the closed ECMA-402 NumberFormat option vocabulary is audited in one match"
)]
fn store_intl_number_format_option(
    state: &mut IntlNumberFormatConstructorContinuation,
    option: IntlNumberFormatOption,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    if option.is_immediate_number() {
        let number = operator_to_number(value, state.realm, &state.origin)?.as_f64();
        return match option {
            IntlNumberFormatOption::MinimumIntegerDigits => {
                state.options.minimum_integer_digits =
                    Some(number_option_u8(state, option, number, 1, 21)?);
                Ok(())
            }
            IntlNumberFormatOption::RoundingIncrement => {
                let rounded = number_option_u16(state, option, number, 1, 5000)?;
                if !ALLOWED_NUMBER_FORMAT_ROUNDING_INCREMENTS.contains(&rounded) {
                    return invalid_intl_number_format_option(state, option);
                }
                state.options.rounding_increment = Some(rounded);
                Ok(())
            }
            _ => Err(EngineFault::RuntimeInvariant {
                message: "non-numeric NumberFormat option reached numeric storage",
            }
            .into()),
        };
    }

    if option == IntlNumberFormatOption::UseGrouping {
        state.options.use_grouping = Some(if value.is_truthy() {
            match value {
                StoredValue::Boolean(true) => NumberFormatUseGrouping::Always,
                StoredValue::Boolean(false) => unreachable!("falsy Boolean handled below"),
                value => {
                    let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
                    let text = text.to_utf8_lossy()?;
                    match text.as_str() {
                        "min2" => NumberFormatUseGrouping::Min2,
                        "auto" => NumberFormatUseGrouping::Auto,
                        "always" => NumberFormatUseGrouping::Always,
                        "true" | "false" => {
                            if state.options.notation == Some(NumberFormatNotation::Compact) {
                                NumberFormatUseGrouping::Min2
                            } else {
                                NumberFormatUseGrouping::Auto
                            }
                        }
                        _ => return invalid_intl_number_format_option(state, option),
                    }
                }
            }
        } else {
            NumberFormatUseGrouping::Never
        });
        return Ok(());
    }

    let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let text = text.to_utf8_lossy()?;
    match option {
        IntlNumberFormatOption::LocaleMatcher => {
            if !matches!(text.as_str(), "lookup" | "best fit") {
                return invalid_intl_number_format_option(state, option);
            }
        }
        IntlNumberFormatOption::NumberingSystem => {
            let Some(value) = canonical_unicode_locale_type(&text) else {
                return invalid_intl_number_format_option(state, option);
            };
            state.options.numbering_system = Some(value);
        }
        IntlNumberFormatOption::Style => {
            state.options.style = Some(match text.as_str() {
                "decimal" => NumberFormatStyle::Decimal,
                "percent" => NumberFormatStyle::Percent,
                "currency" => NumberFormatStyle::Currency,
                "unit" => NumberFormatStyle::Unit,
                _ => return invalid_intl_number_format_option(state, option),
            });
        }
        IntlNumberFormatOption::Currency => {
            if !is_well_formed_currency_code(&text) {
                return invalid_intl_number_format_option(state, option);
            }
            state.options.currency = Some(text.to_ascii_uppercase());
        }
        IntlNumberFormatOption::CurrencyDisplay => {
            state.options.currency_display = Some(match text.as_str() {
                "code" => NumberFormatCurrencyDisplay::Code,
                "symbol" => NumberFormatCurrencyDisplay::Symbol,
                "narrowSymbol" => NumberFormatCurrencyDisplay::NarrowSymbol,
                "name" => NumberFormatCurrencyDisplay::Name,
                _ => return invalid_intl_number_format_option(state, option),
            });
        }
        IntlNumberFormatOption::CurrencySign => {
            state.options.currency_sign = Some(match text.as_str() {
                "standard" => NumberFormatCurrencySign::Standard,
                "accounting" => NumberFormatCurrencySign::Accounting,
                _ => return invalid_intl_number_format_option(state, option),
            });
        }
        IntlNumberFormatOption::Unit => {
            if !is_well_formed_unit_identifier(&text) {
                return invalid_intl_number_format_option(state, option);
            }
            state.options.unit = Some(text);
        }
        IntlNumberFormatOption::UnitDisplay => {
            state.options.unit_display = Some(match text.as_str() {
                "short" => NumberFormatUnitDisplay::Short,
                "narrow" => NumberFormatUnitDisplay::Narrow,
                "long" => NumberFormatUnitDisplay::Long,
                _ => return invalid_intl_number_format_option(state, option),
            });
        }
        IntlNumberFormatOption::Notation => {
            state.options.notation = Some(match text.as_str() {
                "standard" => NumberFormatNotation::Standard,
                "scientific" => NumberFormatNotation::Scientific,
                "engineering" => NumberFormatNotation::Engineering,
                "compact" => NumberFormatNotation::Compact,
                _ => return invalid_intl_number_format_option(state, option),
            });
        }
        IntlNumberFormatOption::RoundingMode => {
            state.options.rounding_mode = Some(match text.as_str() {
                "ceil" => NumberFormatRoundingMode::Ceil,
                "floor" => NumberFormatRoundingMode::Floor,
                "expand" => NumberFormatRoundingMode::Expand,
                "trunc" => NumberFormatRoundingMode::Trunc,
                "halfCeil" => NumberFormatRoundingMode::HalfCeil,
                "halfFloor" => NumberFormatRoundingMode::HalfFloor,
                "halfExpand" => NumberFormatRoundingMode::HalfExpand,
                "halfTrunc" => NumberFormatRoundingMode::HalfTrunc,
                "halfEven" => NumberFormatRoundingMode::HalfEven,
                _ => return invalid_intl_number_format_option(state, option),
            });
        }
        IntlNumberFormatOption::RoundingPriority => {
            state.options.rounding_priority = Some(match text.as_str() {
                "auto" => NumberFormatRoundingPriority::Auto,
                "morePrecision" => NumberFormatRoundingPriority::MorePrecision,
                "lessPrecision" => NumberFormatRoundingPriority::LessPrecision,
                _ => return invalid_intl_number_format_option(state, option),
            });
        }
        IntlNumberFormatOption::TrailingZeroDisplay => {
            state.options.trailing_zero_display = Some(match text.as_str() {
                "auto" => NumberFormatTrailingZeroDisplay::Auto,
                "stripIfInteger" => NumberFormatTrailingZeroDisplay::StripIfInteger,
                _ => return invalid_intl_number_format_option(state, option),
            });
        }
        IntlNumberFormatOption::CompactDisplay => {
            state.options.compact_display = Some(match text.as_str() {
                "short" => NumberFormatCompactDisplay::Short,
                "long" => NumberFormatCompactDisplay::Long,
                _ => return invalid_intl_number_format_option(state, option),
            });
        }
        IntlNumberFormatOption::SignDisplay => {
            state.options.sign_display = Some(match text.as_str() {
                "auto" => NumberFormatSignDisplay::Auto,
                "never" => NumberFormatSignDisplay::Never,
                "always" => NumberFormatSignDisplay::Always,
                "exceptZero" => NumberFormatSignDisplay::ExceptZero,
                "negative" => NumberFormatSignDisplay::Negative,
                _ => return invalid_intl_number_format_option(state, option),
            });
        }
        IntlNumberFormatOption::MinimumIntegerDigits
        | IntlNumberFormatOption::MinimumFractionDigits
        | IntlNumberFormatOption::MaximumFractionDigits
        | IntlNumberFormatOption::MinimumSignificantDigits
        | IntlNumberFormatOption::MaximumSignificantDigits
        | IntlNumberFormatOption::RoundingIncrement
        | IntlNumberFormatOption::UseGrouping => {
            return Err(EngineFault::RuntimeInvariant {
                message: "a non-string NumberFormat option reached string storage",
            }
            .into());
        }
    }
    Ok(())
}

const ALLOWED_NUMBER_FORMAT_ROUNDING_INCREMENTS: &[u16] = &[
    1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000, 2500, 5000,
];

fn store_intl_number_format_raw_digit(
    state: &mut IntlNumberFormatConstructorContinuation,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    let number = operator_to_number(value, state.realm, &state.origin)?.as_f64();
    let (minimum, maximum) = if state.raw_digit_index < 2 {
        (0, 100)
    } else {
        (1, 21)
    };
    let value = number_option_u8(
        state,
        IntlNumberFormatOption::ALL[10 + state.raw_digit_index],
        number,
        minimum,
        maximum,
    )?;
    match state.raw_digit_index {
        0 => state.options.minimum_fraction_digits = Some(value),
        1 => state.options.maximum_fraction_digits = Some(value),
        2 => state.options.minimum_significant_digits = Some(value),
        3 => state.options.maximum_significant_digits = Some(value),
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "NumberFormat raw digit index is out of range",
            }
            .into());
        }
    }
    Ok(())
}

fn number_option_u8(
    state: &IntlNumberFormatConstructorContinuation,
    option: IntlNumberFormatOption,
    number: f64,
    minimum: u8,
    maximum: u8,
) -> Result<u8, NativeFailure> {
    if !number.is_finite() || number < f64::from(minimum) || number > f64::from(maximum) {
        return invalid_intl_number_format_option(state, option);
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the finite value is bounded to the inclusive u8 option range above"
    )]
    let integer = number.floor() as u8;
    Ok(integer)
}

fn number_option_u16(
    state: &IntlNumberFormatConstructorContinuation,
    option: IntlNumberFormatOption,
    number: f64,
    minimum: u16,
    maximum: u16,
) -> Result<u16, NativeFailure> {
    if !number.is_finite() || number < f64::from(minimum) || number > f64::from(maximum) {
        return invalid_intl_number_format_option(state, option);
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the finite value is bounded to the inclusive u16 option range above"
    )]
    let integer = number.floor() as u16;
    Ok(integer)
}

fn canonical_unicode_locale_type(value: &str) -> Option<String> {
    if value.is_empty()
        || value.split('-').any(|subtag| {
            !(3..=8).contains(&subtag.len())
                || !subtag.is_ascii()
                || !subtag.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

fn invalid_intl_number_format_option<T>(
    state: &IntlNumberFormatConstructorContinuation,
    option: IntlNumberFormatOption,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        state.realm,
        state.origin.clone(),
        ExceptionKind::RangeError,
        &format!("invalid Intl.NumberFormat {} option", option.name()),
    )
}

fn finish_intl_number_format_options(
    runtime: &mut Runtime,
    mut state: IntlNumberFormatConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if state.options.rounding_increment.unwrap_or(1) != 1
        && (matches!(
            state.options.rounding_priority,
            Some(
                NumberFormatRoundingPriority::MorePrecision
                    | NumberFormatRoundingPriority::LessPrecision
            )
        ) || state.options.minimum_significant_digits.is_some()
            || state.options.maximum_significant_digits.is_some())
    {
        return intl_locale_list_error(
            state.realm,
            state.origin.clone(),
            ExceptionKind::TypeError,
            "roundingIncrement is incompatible with significant-digit rounding",
        );
    }
    execution_budget
        .charge_instructions(usize_to_u64(state.requested_locales.len()).saturating_add(1))?;
    let resolved = resolve_number_format(&state.requested_locales, state.options.clone()).map_err(
        |error| {
            let kind = match error {
                NumberFormatError::InvalidCurrency | NumberFormatError::InvalidUnit => {
                    ExceptionKind::TypeError
                }
                _ => ExceptionKind::RangeError,
            };
            NativeFailure::Abrupt(PendingException {
                realm: state.realm,
                payload: PendingExceptionPayload::EngineError {
                    kind,
                    message: JsString::from_utf8("invalid Intl.NumberFormat options")
                        .expect("static Intl error message is valid"),
                },
                origin: state.origin.clone(),
            })
        },
    )?;
    if let Some(value) = state.format_value.take() {
        let formatted =
            format_number(&resolved, &value).map_err(|_| EngineFault::RuntimeInvariant {
                message: "resolved Intl.NumberFormat slots failed locale formatting",
            })?;
        return Ok(NativeDispatch::Immediate(StoredValue::String(
            JsString::from_utf8(&formatted)?,
        )));
    }
    state.resolved = Some(resolved);
    state.stage = IntlNumberFormatConstructorStage::AwaitPrototype;
    let new_target = state.new_target.ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.NumberFormat allocation lost its new target",
    })?;
    let base = StoredValue::Function(new_target);
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
    continue_intl_number_format_constructor_after(
        dispatch,
        state,
        runtime,
        return_to,
        execution_budget,
    )
}

fn continue_intl_number_format_constructor_after(
    dispatch: NativeDispatch,
    state: IntlNumberFormatConstructorContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        intl_number_format_constructor_continuation,
        |state, value| {
            advance_intl_number_format_constructor(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.NumberFormat property Get produced a structured result",
    )
}

fn intl_number_format_constructor_continuation(
    state: IntlNumberFormatConstructorContinuation,
) -> NativeContinuation {
    NativeContinuation::IntlNumberFormatConstructor(Box::new(state))
}

fn take_intl_number_format_constructor_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.NumberFormat constructor resumed without a completion",
    })
}

pub(super) fn begin_intl_number_format_supported_locales_of(
    runtime: &mut Runtime,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let locales = arguments.take_first_or_undefined();
    let options_argument = arguments.take_first_or_undefined();
    let state = IntlNumberFormatSupportedLocalesContinuation {
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        realm,
        stage: IntlNumberFormatSupportedLocalesStage::ReadLocaleMatcher,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::NumberFormatSupportedLocalesOf(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_number_format_supported_locales_options(
    runtime: &mut Runtime,
    mut state: IntlNumberFormatSupportedLocalesContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_number_format_supported_locales(runtime, &state);
    }
    let options_argument = state.options_argument.duplicate();
    state.options_object = Some(
        match to_object_value(runtime, state.realm, options_argument, state.origin.clone())? {
            Ok(options) => options,
            Err(exception) => return Err(NativeFailure::Abrupt(exception)),
        },
    );
    advance_intl_number_format_supported_locales(runtime, state, None, return_to, execution_budget)
}

pub(super) fn advance_intl_number_format_supported_locales(
    runtime: &mut Runtime,
    mut state: IntlNumberFormatSupportedLocalesContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    match state.stage {
        IntlNumberFormatSupportedLocalesStage::ReadLocaleMatcher => {
            let base = state
                .options_object
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.NumberFormat.supportedLocalesOf lost its options object",
                })?
                .duplicate();
            charge_heap_property_lookup(runtime, &base, execution_budget)?;
            let name = JsString::from_utf8("localeMatcher")?;
            let key = runtime.property_key_from_string(&name)?;
            state.stage = IntlNumberFormatSupportedLocalesStage::AwaitLocaleMatcher;
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
            continue_intl_number_format_supported_locales_after(
                dispatch,
                state,
                runtime,
                return_to,
                execution_budget,
            )
        }
        IntlNumberFormatSupportedLocalesStage::AwaitLocaleMatcher => {
            let value = take_intl_number_format_supported_locales_completion(&mut completion)?;
            if matches!(value, StoredValue::Undefined) {
                return finish_intl_number_format_supported_locales(runtime, &state);
            }
            if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                state.stage = IntlNumberFormatSupportedLocalesStage::AwaitLocaleMatcherPrimitive;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::IntlNumberFormatSupportedLocalesOf(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            validate_intl_number_format_locale_matcher(&state, value)?;
            finish_intl_number_format_supported_locales(runtime, &state)
        }
        IntlNumberFormatSupportedLocalesStage::AwaitLocaleMatcherPrimitive => {
            let value = take_intl_number_format_supported_locales_completion(&mut completion)?;
            validate_intl_number_format_locale_matcher(&state, value)?;
            finish_intl_number_format_supported_locales(runtime, &state)
        }
    }
}

fn validate_intl_number_format_locale_matcher(
    state: &IntlNumberFormatSupportedLocalesContinuation,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
    if matches!(text.to_utf8_lossy()?.as_str(), "lookup" | "best fit") {
        return Ok(());
    }
    intl_locale_list_error(
        state.realm,
        state.origin.clone(),
        ExceptionKind::RangeError,
        "invalid Intl.NumberFormat localeMatcher option",
    )
}

fn finish_intl_number_format_supported_locales(
    runtime: &mut Runtime,
    state: &IntlNumberFormatSupportedLocalesContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    intl_locale_string_array(
        runtime,
        state.realm,
        number_format_supported_locales(&state.requested_locales),
    )
}

fn continue_intl_number_format_supported_locales_after(
    dispatch: NativeDispatch,
    state: IntlNumberFormatSupportedLocalesContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        intl_number_format_supported_locales_continuation,
        |state, value| {
            advance_intl_number_format_supported_locales(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.NumberFormat.supportedLocalesOf Get produced a structured result",
    )
}

fn intl_number_format_supported_locales_continuation(
    state: IntlNumberFormatSupportedLocalesContinuation,
) -> NativeContinuation {
    NativeContinuation::IntlNumberFormatSupportedLocalesOf(Box::new(state))
}

fn take_intl_number_format_supported_locales_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.NumberFormat.supportedLocalesOf resumed without a completion",
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch keeps receiver, arguments, realm, return target, origin, and budget explicit"
)]
pub(super) fn begin_intl_number_format_prototype(
    runtime: &mut Runtime,
    method: IntlNumberFormatPrototypeMethod,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(
        method,
        IntlNumberFormatPrototypeMethod::Format | IntlNumberFormatPrototypeMethod::ResolvedOptions
    ) {
        return begin_intl_number_format_unwrap(
            runtime,
            method,
            receiver,
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    let StoredValue::Object(formatter) = receiver else {
        return intl_number_format_brand_error(realm, origin);
    };
    if runtime.intl_number_format_state(*formatter)?.is_none() {
        return intl_number_format_brand_error(realm, origin);
    }
    match method {
        IntlNumberFormatPrototypeMethod::Format
        | IntlNumberFormatPrototypeMethod::ResolvedOptions => Err(EngineFault::RuntimeInvariant {
            message: "unwrap-capable Intl.NumberFormat method bypassed UnwrapNumberFormat",
        }
        .into()),
        IntlNumberFormatPrototypeMethod::FormatToParts => {
            let value = arguments.take_first_or_undefined();
            begin_intl_number_format_value(
                runtime,
                IntlNumberFormatValueContinuation {
                    formatter: *formatter,
                    operation: IntlNumberFormatOperation::FormatToParts,
                    second: None,
                    first: None,
                    realm,
                    origin,
                },
                value,
                return_to,
                execution_budget,
            )
        }
        IntlNumberFormatPrototypeMethod::FormatRange
        | IntlNumberFormatPrototypeMethod::FormatRangeToParts => {
            let first = arguments.take_first_or_undefined();
            let second = arguments.take_first_or_undefined();
            if matches!(first, StoredValue::Undefined) || matches!(second, StoredValue::Undefined) {
                return intl_locale_list_error(
                    realm,
                    origin,
                    ExceptionKind::TypeError,
                    "Intl.NumberFormat range arguments must not be undefined",
                );
            }
            begin_intl_number_format_value(
                runtime,
                IntlNumberFormatValueContinuation {
                    formatter: *formatter,
                    operation: if method == IntlNumberFormatPrototypeMethod::FormatRange {
                        IntlNumberFormatOperation::FormatRange
                    } else {
                        IntlNumberFormatOperation::FormatRangeToParts
                    },
                    second: Some(second),
                    first: None,
                    realm,
                    origin,
                },
                first,
                return_to,
                execution_budget,
            )
        }
    }
}

fn begin_intl_number_format_unwrap(
    runtime: &mut Runtime,
    method: IntlNumberFormatPrototypeMethod,
    receiver: &StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(formatter) = receiver
        && runtime.intl_number_format_state(*formatter)?.is_some()
    {
        return finish_intl_number_format_unwrap(runtime, method, *formatter, realm, origin);
    }
    if !matches!(receiver, StoredValue::Function(_) | StoredValue::Object(_)) {
        return intl_number_format_brand_error(realm, origin);
    }
    let state = IntlNumberFormatUnwrapContinuation {
        receiver: receiver.duplicate(),
        method,
        realm,
        stage: IntlNumberFormatUnwrapStage::AwaitInstance,
        origin: origin.clone(),
    };
    let constructor = runtime.realm_intl_number_format_constructor(realm)?;
    let dispatch = begin_function_has_instance(
        runtime,
        realm,
        receiver.duplicate(),
        StoredValue::Function(constructor),
        return_to,
        origin,
        execution_budget,
    )?;
    continue_intl_number_format_unwrap_after(dispatch, state, runtime, return_to, execution_budget)
}

pub(super) fn advance_intl_number_format_unwrap(
    runtime: &mut Runtime,
    mut state: IntlNumberFormatUnwrapContinuation,
    completion: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IntlNumberFormatUnwrapStage::AwaitInstance => {
            if !completion.is_truthy() {
                return intl_number_format_brand_error(state.realm, state.origin);
            }
            let symbol = runtime.intl_number_format_fallback_symbol();
            let key = runtime.property_key_from_symbol(&symbol)?;
            charge_heap_property_lookup(runtime, &state.receiver, execution_budget)?;
            state.stage = IntlNumberFormatUnwrapStage::AwaitFallback;
            let dispatch = begin_value_get(
                runtime,
                &state.receiver,
                key,
                None,
                state.realm,
                return_to,
                state.origin.clone(),
                execution_budget,
            )?;
            continue_intl_number_format_unwrap_after(
                dispatch,
                state,
                runtime,
                return_to,
                execution_budget,
            )
        }
        IntlNumberFormatUnwrapStage::AwaitFallback => {
            let StoredValue::Object(formatter) = completion else {
                return intl_number_format_brand_error(state.realm, state.origin);
            };
            if runtime.intl_number_format_state(*formatter)?.is_none() {
                return intl_number_format_brand_error(state.realm, state.origin);
            }
            finish_intl_number_format_unwrap(
                runtime,
                state.method,
                *formatter,
                state.realm,
                state.origin,
            )
        }
    }
}

fn finish_intl_number_format_unwrap(
    runtime: &mut Runtime,
    method: IntlNumberFormatPrototypeMethod,
    formatter: ObjectId,
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        IntlNumberFormatPrototypeMethod::Format => {
            let function = match runtime.intl_number_format_bound_format(formatter)? {
                Some(function) => function,
                None => runtime.allocate_intl_number_format_bound_format(realm, formatter)?,
            };
            Ok(NativeDispatch::Immediate(StoredValue::Function(function)))
        }
        IntlNumberFormatPrototypeMethod::ResolvedOptions => {
            let state = runtime
                .intl_number_format_state(formatter)?
                .cloned()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "unwrapped Intl.NumberFormat lost its internal slots",
                })?;
            intl_number_format_resolved_options(runtime, realm, &state)
        }
        IntlNumberFormatPrototypeMethod::FormatToParts
        | IntlNumberFormatPrototypeMethod::FormatRange
        | IntlNumberFormatPrototypeMethod::FormatRangeToParts => {
            intl_number_format_brand_error(realm, origin)
        }
    }
}

fn continue_intl_number_format_unwrap_after(
    dispatch: NativeDispatch,
    state: IntlNumberFormatUnwrapContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => {
            advance_intl_number_format_unwrap(runtime, state, &value, return_to, execution_budget)
        }
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::IntlNumberFormatUnwrap(Box::new(state))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::IntlNumberFormatUnwrap(Box::new(state))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "UnwrapNumberFormat produced a structured result",
        }
        .into()),
    }
}

pub(super) fn begin_intl_number_format_format(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(formatter) = receiver else {
        return intl_number_format_brand_error(realm, origin);
    };
    if runtime.intl_number_format_state(*formatter)?.is_none() {
        return intl_number_format_brand_error(realm, origin);
    }
    let value = arguments.take_first_or_undefined();
    begin_intl_number_format_value(
        runtime,
        IntlNumberFormatValueContinuation {
            formatter: *formatter,
            operation: IntlNumberFormatOperation::Format,
            second: None,
            first: None,
            realm,
            origin,
        },
        value,
        return_to,
        execution_budget,
    )
}

fn begin_intl_number_format_value(
    runtime: &mut Runtime,
    state: IntlNumberFormatValueContinuation,
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
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::IntlNumberFormatValue(Box::new(state)),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    finish_intl_number_format_value_primitive(runtime, state, value, return_to, execution_budget)
}

pub(super) fn finish_intl_number_format_value_primitive(
    runtime: &mut Runtime,
    mut state: IntlNumberFormatValueContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let value = to_intl_mathematical_value(value, state.realm, &state.origin)?;
    if matches!(
        state.operation,
        IntlNumberFormatOperation::FormatRange | IntlNumberFormatOperation::FormatRangeToParts
    ) && state.first.is_none()
    {
        state.first = Some(value);
        let second = state.second.take().ok_or(EngineFault::RuntimeInvariant {
            message: "Intl.NumberFormat range conversion lost its second operand",
        })?;
        return begin_intl_number_format_value(runtime, state, second, return_to, execution_budget);
    }
    finish_intl_number_format_operation(runtime, state, &value)
}

pub(super) fn to_intl_mathematical_value(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<IntlMathematicalValue, NativeFailure> {
    match value {
        StoredValue::String(value) => Ok(parse_intl_mathematical_value(&value.to_utf8_lossy()?)),
        StoredValue::BigInt(value) => Ok(parse_intl_mathematical_value(
            &bigint_decimal_string(&value)?.to_utf8_lossy()?,
        )),
        value => Ok(intl_mathematical_value_from_f64(
            operator_to_number(value, realm, origin)?.as_f64(),
        )),
    }
}

fn finish_intl_number_format_operation(
    runtime: &mut Runtime,
    state: IntlNumberFormatValueContinuation,
    value: &IntlMathematicalValue,
) -> Result<NativeDispatch, NativeFailure> {
    let resolved = runtime.intl_number_format_state(state.formatter)?.ok_or(
        EngineFault::RuntimeInvariant {
            message: "Intl.NumberFormat operation lost its branded receiver",
        },
    )?;
    match state.operation {
        IntlNumberFormatOperation::Format => {
            let formatted =
                format_number(resolved, value).map_err(|_| EngineFault::RuntimeInvariant {
                    message: "resolved Intl.NumberFormat slots failed formatting",
                })?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&formatted)?,
            )))
        }
        IntlNumberFormatOperation::FormatToParts => {
            let parts = format_number_to_parts(resolved, value).map_err(|_| {
                EngineFault::RuntimeInvariant {
                    message: "resolved Intl.NumberFormat slots failed parts formatting",
                }
            })?;
            intl_number_format_parts_array(runtime, state.realm, parts, None)
        }
        IntlNumberFormatOperation::FormatRange | IntlNumberFormatOperation::FormatRangeToParts => {
            let first = state.first.ok_or(EngineFault::RuntimeInvariant {
                message: "Intl.NumberFormat range operation lost its first operand",
            })?;
            if matches!(first, IntlMathematicalValue::NaN)
                || matches!(value, IntlMathematicalValue::NaN)
            {
                return intl_locale_list_error(
                    state.realm,
                    state.origin,
                    ExceptionKind::RangeError,
                    "Intl.NumberFormat range arguments must not be NaN",
                );
            }
            let parts = intl_number_format_range_parts(resolved, &first, value)?;
            let formatted = parts
                .iter()
                .map(|part| part.part.value.as_str())
                .collect::<String>();
            if state.operation == IntlNumberFormatOperation::FormatRange {
                Ok(NativeDispatch::Immediate(StoredValue::String(
                    JsString::from_utf8(&formatted)?,
                )))
            } else {
                intl_number_format_sourced_parts_array(runtime, state.realm, parts)
            }
        }
    }
}

struct SourcedNumberFormatPart {
    part: quickjs_intl::NumberFormatPart,
    source: &'static str,
}

#[allow(
    clippy::too_many_lines,
    reason = "range partitioning keeps prefix, suffix, separator, and source attribution together"
)]
fn intl_number_format_range_parts(
    state: &NumberFormatState,
    first: &IntlMathematicalValue,
    second: &IntlMathematicalValue,
) -> Result<Vec<SourcedNumberFormatPart>, NativeFailure> {
    let first_parts =
        format_number_to_parts(state, first).map_err(|_| EngineFault::RuntimeInvariant {
            message: "resolved Intl.NumberFormat slots failed range formatting",
        })?;
    let second_parts =
        format_number_to_parts(state, second).map_err(|_| EngineFault::RuntimeInvariant {
            message: "resolved Intl.NumberFormat slots failed range formatting",
        })?;
    let first_text = first_parts
        .iter()
        .map(|part| part.value.as_str())
        .collect::<String>();
    let second_text = second_parts
        .iter()
        .map(|part| part.value.as_str())
        .collect::<String>();
    if first_text == second_text {
        let mut result = Vec::new();
        result.push(SourcedNumberFormatPart {
            part: quickjs_intl::NumberFormatPart {
                kind: "approximatelySign",
                value: "~".to_owned(),
            },
            source: "shared",
        });
        result.extend(first_parts.into_iter().map(|part| SourcedNumberFormatPart {
            part,
            source: "shared",
        }));
        return Ok(result);
    }

    let mut shared_prefix = 0;
    if matches!(
        first_parts.first().map(|part| part.kind),
        Some("plusSign" | "minusSign")
    ) {
        while shared_prefix < first_parts.len()
            && shared_prefix < second_parts.len()
            && first_parts[shared_prefix] == second_parts[shared_prefix]
            && !is_number_format_numeric_part(first_parts[shared_prefix].kind)
        {
            shared_prefix += 1;
        }
    }

    let mut shared_suffix = 0;
    if state.locale.starts_with("pt") {
        while shared_suffix + shared_prefix < first_parts.len()
            && shared_suffix + shared_prefix < second_parts.len()
        {
            let first_index = first_parts.len() - shared_suffix - 1;
            let second_index = second_parts.len() - shared_suffix - 1;
            if first_parts[first_index] != second_parts[second_index]
                || is_number_format_numeric_part(first_parts[first_index].kind)
            {
                break;
            }
            shared_suffix += 1;
        }
    }

    let separator = if state.locale.starts_with("pt") {
        " - "
    } else if state.style == NumberFormatStyle::Currency && shared_prefix == 0 {
        " – "
    } else {
        "–"
    };
    let mut result = Vec::new();
    result.extend(first_parts[..shared_prefix].iter().cloned().map(|part| {
        SourcedNumberFormatPart {
            part,
            source: "shared",
        }
    }));
    result.extend(
        first_parts[shared_prefix..first_parts.len() - shared_suffix]
            .iter()
            .cloned()
            .map(|part| SourcedNumberFormatPart {
                part,
                source: "startRange",
            }),
    );
    result.push(SourcedNumberFormatPart {
        part: quickjs_intl::NumberFormatPart {
            kind: "literal",
            value: separator.to_owned(),
        },
        source: "shared",
    });
    result.extend(
        second_parts[shared_prefix..second_parts.len() - shared_suffix]
            .iter()
            .cloned()
            .map(|part| SourcedNumberFormatPart {
                part,
                source: "endRange",
            }),
    );
    result.extend(
        second_parts[second_parts.len() - shared_suffix..]
            .iter()
            .cloned()
            .map(|part| SourcedNumberFormatPart {
                part,
                source: "shared",
            }),
    );
    Ok(result)
}

fn is_number_format_numeric_part(kind: &str) -> bool {
    matches!(
        kind,
        "integer"
            | "group"
            | "decimal"
            | "fraction"
            | "compact"
            | "exponentInteger"
            | "exponentMinusSign"
            | "exponentSeparator"
            | "infinity"
            | "nan"
    )
}

fn intl_number_format_parts_array(
    runtime: &mut Runtime,
    realm: RealmId,
    parts: Vec<quickjs_intl::NumberFormatPart>,
    source: Option<&'static str>,
) -> Result<NativeDispatch, NativeFailure> {
    intl_number_format_part_entries_array(
        runtime,
        realm,
        parts.into_iter().map(|part| (part, source)).collect(),
    )
}

fn intl_number_format_sourced_parts_array(
    runtime: &mut Runtime,
    realm: RealmId,
    parts: Vec<SourcedNumberFormatPart>,
) -> Result<NativeDispatch, NativeFailure> {
    intl_number_format_part_entries_array(
        runtime,
        realm,
        parts
            .into_iter()
            .map(|part| (part.part, Some(part.source)))
            .collect(),
    )
}

fn intl_number_format_part_entries_array(
    runtime: &mut Runtime,
    realm: RealmId,
    parts: Vec<(quickjs_intl::NumberFormatPart, Option<&'static str>)>,
) -> Result<NativeDispatch, NativeFailure> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(parts.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: parts.len(),
        })?;
    for (part, source) in parts {
        let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
        let mut properties = vec![
            ("type", StoredValue::String(JsString::from_utf8(part.kind)?)),
            (
                "value",
                StoredValue::String(JsString::from_utf8(&part.value)?),
            ),
        ];
        if let Some(source) = source {
            properties.push(("source", StoredValue::String(JsString::from_utf8(source)?)));
        }
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
        values.push(StoredValue::Object(object));
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(
        runtime.allocate_array(realm, values)?,
    )))
}

#[allow(
    clippy::too_many_lines,
    reason = "resolvedOptions property order mirrors the ECMA-402 algorithm"
)]
fn intl_number_format_resolved_options(
    runtime: &mut Runtime,
    realm: RealmId,
    state: &NumberFormatState,
) -> Result<NativeDispatch, NativeFailure> {
    let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    let mut properties = Vec::new();
    properties.push((
        "locale",
        StoredValue::String(JsString::from_utf8(&state.locale)?),
    ));
    properties.push((
        "numberingSystem",
        StoredValue::String(JsString::from_utf8(&state.numbering_system)?),
    ));
    properties.push((
        "style",
        StoredValue::String(JsString::from_utf8(state.style.as_str())?),
    ));
    if state.style == NumberFormatStyle::Currency {
        properties.push((
            "currency",
            StoredValue::String(JsString::from_utf8(
                state.currency.as_deref().unwrap_or(""),
            )?),
        ));
        properties.push((
            "currencyDisplay",
            StoredValue::String(JsString::from_utf8(state.currency_display.as_str())?),
        ));
        properties.push((
            "currencySign",
            StoredValue::String(JsString::from_utf8(state.currency_sign.as_str())?),
        ));
    }
    if state.style == NumberFormatStyle::Unit {
        properties.push((
            "unit",
            StoredValue::String(JsString::from_utf8(state.unit.as_deref().unwrap_or(""))?),
        ));
        properties.push((
            "unitDisplay",
            StoredValue::String(JsString::from_utf8(state.unit_display.as_str())?),
        ));
    }
    properties.push((
        "minimumIntegerDigits",
        StoredValue::Number(JsNumber::from_i32(i32::from(state.minimum_integer_digits))),
    ));
    if let (Some(minimum), Some(maximum)) =
        (state.minimum_fraction_digits, state.maximum_fraction_digits)
    {
        properties.push((
            "minimumFractionDigits",
            StoredValue::Number(JsNumber::from_i32(i32::from(minimum))),
        ));
        properties.push((
            "maximumFractionDigits",
            StoredValue::Number(JsNumber::from_i32(i32::from(maximum))),
        ));
    }
    if let (Some(minimum), Some(maximum)) = (
        state.minimum_significant_digits,
        state.maximum_significant_digits,
    ) {
        properties.push((
            "minimumSignificantDigits",
            StoredValue::Number(JsNumber::from_i32(i32::from(minimum))),
        ));
        properties.push((
            "maximumSignificantDigits",
            StoredValue::Number(JsNumber::from_i32(i32::from(maximum))),
        ));
    }
    properties.push((
        "useGrouping",
        if state.use_grouping == NumberFormatUseGrouping::Never {
            StoredValue::Boolean(false)
        } else {
            StoredValue::String(JsString::from_utf8(state.use_grouping.as_str())?)
        },
    ));
    properties.push((
        "notation",
        StoredValue::String(JsString::from_utf8(state.notation.as_str())?),
    ));
    if state.notation == NumberFormatNotation::Compact {
        properties.push((
            "compactDisplay",
            StoredValue::String(JsString::from_utf8(state.compact_display.as_str())?),
        ));
    }
    properties.push((
        "signDisplay",
        StoredValue::String(JsString::from_utf8(state.sign_display.as_str())?),
    ));
    properties.push((
        "roundingIncrement",
        StoredValue::Number(JsNumber::from_i32(i32::from(state.rounding_increment))),
    ));
    properties.push((
        "roundingMode",
        StoredValue::String(JsString::from_utf8(state.rounding_mode.as_str())?),
    ));
    properties.push((
        "roundingPriority",
        StoredValue::String(JsString::from_utf8(state.rounding_priority.as_str())?),
    ));
    properties.push((
        "trailingZeroDisplay",
        StoredValue::String(JsString::from_utf8(state.trailing_zero_display.as_str())?),
    ));
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

fn intl_number_format_brand_error<T>(
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        realm,
        origin,
        ExceptionKind::TypeError,
        "Intl.NumberFormat method called on incompatible receiver",
    )
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
        IntlLocaleListTarget::NumberFormatConstructor(mut state) => {
            state.requested_locales = intl_locale_strings(locales)?;
            begin_intl_number_format_options(runtime, *state, return_to, execution_budget)
        }
        IntlLocaleListTarget::NumberFormatSupportedLocalesOf(mut state) => {
            state.requested_locales = intl_locale_strings(locales)?;
            begin_intl_number_format_supported_locales_options(
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
