//! Resumable ECMA-402 locale-list canonicalization.

use super::instanceof::begin_function_has_instance;
#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

use quickjs_intl::{
    CollatorRequestOptions, CollatorSensitivity, CollatorState, CollatorUsage,
    DateTimeComponentStyle, DateTimeFormatError, DateTimeFormatInput, DateTimeFormatInputKind,
    DateTimeFormatMatcher, DateTimeFormatRequestOptions, DateTimeFormatState, DateTimeHourCycle,
    DateTimeStyle, DateTimeTimeZoneName, IntlMathematicalValue, LocaleComponents, LocaleOptionKind,
    LocaleOptions, LocaleWeekInfo, NumberFormatCompactDisplay, NumberFormatCurrencyDisplay,
    NumberFormatCurrencySign, NumberFormatError, NumberFormatNotation, NumberFormatRequestOptions,
    NumberFormatRoundingMode, NumberFormatRoundingPriority, NumberFormatSignDisplay,
    NumberFormatState, NumberFormatStyle, NumberFormatTrailingZeroDisplay, NumberFormatUnitDisplay,
    NumberFormatUseGrouping, PluralRuleType, PluralRulesRequestOptions, PluralRulesState,
    apply_locale_options, calendars_of_locale, canonicalize_locale, canonicalize_locale_option,
    canonicalize_time_zone, collations_of_locale, collator_supported_locales,
    compare_with_collator, date_time_format_supported_locales, format_datetime,
    format_datetime_to_parts, format_number, format_number_to_parts, hour_cycles_of_locale,
    intl_mathematical_value_from_f64, is_well_formed_currency_code, is_well_formed_unit_identifier,
    locale_components, maximize_locale, minimize_locale, number_format_supported_locales,
    numbering_systems_of_locale, parse_intl_mathematical_value, plural_rules_supported_locales,
    resolve_collator, resolve_date_time_format, resolve_number_format, resolve_plural_rules,
    select_plural, select_plural_range, supported_values, text_direction_of_locale,
    time_zones_of_locale, week_info_of_locale,
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
    DateTimeFormatConstructor(Box<IntlDateTimeFormatConstructorContinuation>),
    DateTimeFormatSupportedLocalesOf(Box<IntlDateTimeFormatSupportedLocalesContinuation>),
    PluralRulesConstructor(Box<IntlPluralRulesConstructorContinuation>),
    PluralRulesSupportedLocalesOf(Box<IntlPluralRulesSupportedLocalesContinuation>),
}

impl IntlLocaleListTarget {
    fn retained_values(&self) -> u64 {
        match self {
            Self::ReturnArray => 0,
            Self::CollatorConstructor(state) => state.retained_values(),
            Self::CollatorSupportedLocalesOf(state) => state.retained_values(),
            Self::NumberFormatConstructor(state) => state.retained_values(),
            Self::NumberFormatSupportedLocalesOf(state) => state.retained_values(),
            Self::DateTimeFormatConstructor(state) => state.retained_values(),
            Self::DateTimeFormatSupportedLocalesOf(state) => state.retained_values(),
            Self::PluralRulesConstructor(state) => state.retained_values(),
            Self::PluralRulesSupportedLocalesOf(state) => state.retained_values(),
        }
    }

    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::ReturnArray => {}
            Self::CollatorConstructor(state) => state.trace_roots(mark),
            Self::CollatorSupportedLocalesOf(state) => state.trace_roots(mark),
            Self::NumberFormatConstructor(state) => state.trace_roots(mark),
            Self::NumberFormatSupportedLocalesOf(state) => state.trace_roots(mark),
            Self::DateTimeFormatConstructor(state) => state.trace_roots(mark),
            Self::DateTimeFormatSupportedLocalesOf(state) => state.trace_roots(mark),
            Self::PluralRulesConstructor(state) => state.trace_roots(mark),
            Self::PluralRulesSupportedLocalesOf(state) => state.trace_roots(mark),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlPluralRulesConstructorStage {
    ReadOption,
    AwaitOption,
    AwaitOptionPrimitive,
    ConvertRawDigit,
    AwaitRawDigitPrimitive,
    AwaitPrototype,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlPluralRulesOption {
    LocaleMatcher,
    Type,
    Notation,
    CompactDisplay,
    MinimumIntegerDigits,
    MinimumFractionDigits,
    MaximumFractionDigits,
    MinimumSignificantDigits,
    MaximumSignificantDigits,
    RoundingIncrement,
    RoundingMode,
    RoundingPriority,
    TrailingZeroDisplay,
}

impl IntlPluralRulesOption {
    const ALL: [Self; 13] = [
        Self::LocaleMatcher,
        Self::Type,
        Self::Notation,
        Self::CompactDisplay,
        Self::MinimumIntegerDigits,
        Self::MinimumFractionDigits,
        Self::MaximumFractionDigits,
        Self::MinimumSignificantDigits,
        Self::MaximumSignificantDigits,
        Self::RoundingIncrement,
        Self::RoundingMode,
        Self::RoundingPriority,
        Self::TrailingZeroDisplay,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::LocaleMatcher => "localeMatcher",
            Self::Type => "type",
            Self::Notation => "notation",
            Self::CompactDisplay => "compactDisplay",
            Self::MinimumIntegerDigits => "minimumIntegerDigits",
            Self::MinimumFractionDigits => "minimumFractionDigits",
            Self::MaximumFractionDigits => "maximumFractionDigits",
            Self::MinimumSignificantDigits => "minimumSignificantDigits",
            Self::MaximumSignificantDigits => "maximumSignificantDigits",
            Self::RoundingIncrement => "roundingIncrement",
            Self::RoundingMode => "roundingMode",
            Self::RoundingPriority => "roundingPriority",
            Self::TrailingZeroDisplay => "trailingZeroDisplay",
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

pub(super) struct IntlPluralRulesConstructorContinuation {
    new_target: FunctionId,
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    options: PluralRulesRequestOptions,
    raw_digits: [Option<StoredValue>; 4],
    resolved: Option<PluralRulesState>,
    option_index: usize,
    raw_digit_index: usize,
    realm: RealmId,
    stage: IntlPluralRulesConstructorStage,
    origin: JsStackFrame,
}

impl IntlPluralRulesConstructorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.options_object.is_some()))
            .saturating_add(
                self.raw_digits
                    .iter()
                    .filter(|value| value.is_some())
                    .count() as u64,
            )
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
        trace_stored_value_root(&self.options_argument, mark);
        if let Some(options) = &self.options_object {
            trace_stored_value_root(options, mark);
        }
        for value in self.raw_digits.iter().flatten() {
            trace_stored_value_root(value, mark);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlPluralRulesSupportedLocalesStage {
    ReadLocaleMatcher,
    AwaitLocaleMatcher,
    AwaitLocaleMatcherPrimitive,
}

pub(super) struct IntlPluralRulesSupportedLocalesContinuation {
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    realm: RealmId,
    stage: IntlPluralRulesSupportedLocalesStage,
    origin: JsStackFrame,
}

impl IntlPluralRulesSupportedLocalesContinuation {
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
enum IntlPluralRulesOperation {
    Select,
    SelectRange,
}

pub(super) struct IntlPluralRulesValueContinuation {
    plural_rules: ObjectId,
    operation: IntlPluralRulesOperation,
    second: Option<StoredValue>,
    first: Option<IntlMathematicalValue>,
    realm: RealmId,
    origin: JsStackFrame,
}

impl IntlPluralRulesValueContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(u64::from(self.second.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(
            self.plural_rules,
        )));
        if let Some(second) = &self.second {
            trace_stored_value_root(second, mark);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlDateTimeFormatConstructorStage {
    ReadOption,
    AwaitOption,
    AwaitOptionPrimitive,
    AwaitPrototype,
    AwaitLegacyInstance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlDateTimeFormatRequired {
    Any,
    Date,
    Time,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlDateTimeFormatDefaults {
    Date,
    Time,
    All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlDateTimeFormatOption {
    LocaleMatcher,
    Calendar,
    NumberingSystem,
    Hour12,
    HourCycle,
    TimeZone,
    Weekday,
    Era,
    Year,
    Month,
    Day,
    DayPeriod,
    Hour,
    Minute,
    Second,
    FractionalSecondDigits,
    TimeZoneName,
    FormatMatcher,
    DateStyle,
    TimeStyle,
}

impl IntlDateTimeFormatOption {
    const ALL: [Self; 20] = [
        Self::LocaleMatcher,
        Self::Calendar,
        Self::NumberingSystem,
        Self::Hour12,
        Self::HourCycle,
        Self::TimeZone,
        Self::Weekday,
        Self::Era,
        Self::Year,
        Self::Month,
        Self::Day,
        Self::DayPeriod,
        Self::Hour,
        Self::Minute,
        Self::Second,
        Self::FractionalSecondDigits,
        Self::TimeZoneName,
        Self::FormatMatcher,
        Self::DateStyle,
        Self::TimeStyle,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::LocaleMatcher => "localeMatcher",
            Self::Calendar => "calendar",
            Self::NumberingSystem => "numberingSystem",
            Self::Hour12 => "hour12",
            Self::HourCycle => "hourCycle",
            Self::TimeZone => "timeZone",
            Self::Weekday => "weekday",
            Self::Era => "era",
            Self::Year => "year",
            Self::Month => "month",
            Self::Day => "day",
            Self::DayPeriod => "dayPeriod",
            Self::Hour => "hour",
            Self::Minute => "minute",
            Self::Second => "second",
            Self::FractionalSecondDigits => "fractionalSecondDigits",
            Self::TimeZoneName => "timeZoneName",
            Self::FormatMatcher => "formatMatcher",
            Self::DateStyle => "dateStyle",
            Self::TimeStyle => "timeStyle",
        }
    }

    const fn primitive_hint(self) -> OperatorPrimitiveHint {
        if matches!(self, Self::FractionalSecondDigits) {
            OperatorPrimitiveHint::Number
        } else {
            OperatorPrimitiveHint::String
        }
    }
}

pub(super) struct IntlDateTimeFormatConstructorContinuation {
    new_target: FunctionId,
    format_value: Option<temporal_rs::Instant>,
    required: IntlDateTimeFormatRequired,
    defaults: IntlDateTimeFormatDefaults,
    legacy_receiver: Option<StoredValue>,
    legacy_date_time_format: Option<ObjectId>,
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    options: DateTimeFormatRequestOptions,
    resolved: Option<DateTimeFormatState>,
    option_index: usize,
    realm: RealmId,
    stage: IntlDateTimeFormatConstructorStage,
    origin: JsStackFrame,
}

impl IntlDateTimeFormatConstructorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.options_object.is_some()))
            .saturating_add(u64::from(self.legacy_receiver.is_some()))
            .saturating_add(u64::from(self.legacy_date_time_format.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
        trace_stored_value_root(&self.options_argument, mark);
        if let Some(receiver) = &self.legacy_receiver {
            trace_stored_value_root(receiver, mark);
        }
        if let Some(formatter) = self.legacy_date_time_format {
            mark(CollectionRoot::Heap(HeapReference::Object(formatter)));
        }
        if let Some(options) = &self.options_object {
            trace_stored_value_root(options, mark);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlDateTimeFormatSupportedLocalesStage {
    ReadLocaleMatcher,
    AwaitLocaleMatcher,
    AwaitLocaleMatcherPrimitive,
}

pub(super) struct IntlDateTimeFormatSupportedLocalesContinuation {
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    realm: RealmId,
    stage: IntlDateTimeFormatSupportedLocalesStage,
    origin: JsStackFrame,
}

impl IntlDateTimeFormatSupportedLocalesContinuation {
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
pub(super) enum IntlDateTimeFormatOperation {
    Format,
    FormatToParts,
    FormatRange,
    FormatRangeToParts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlDateTimeInputIdentity {
    Number,
    Instant,
    PlainDateTime,
    PlainDate,
    PlainYearMonth,
    PlainMonthDay,
    PlainTime,
    ZonedDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedDateTimeInput {
    value: DateTimeFormatInput,
    identity: IntlDateTimeInputIdentity,
    calendar: Option<String>,
    valid: bool,
}

pub(super) struct IntlDateTimeFormatValueContinuation {
    formatter: ObjectId,
    operation: IntlDateTimeFormatOperation,
    second: Option<StoredValue>,
    first: Option<ResolvedDateTimeInput>,
    realm: RealmId,
    origin: JsStackFrame,
}

impl IntlDateTimeFormatValueContinuation {
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
enum IntlDateTimeFormatUnwrapStage {
    AwaitInstance,
    AwaitFallback,
}

pub(super) struct IntlDateTimeFormatUnwrapContinuation {
    receiver: StoredValue,
    method: IntlDateTimeFormatPrototypeMethod,
    realm: RealmId,
    stage: IntlDateTimeFormatUnwrapStage,
    origin: JsStackFrame,
}

impl IntlDateTimeFormatUnwrapContinuation {
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

pub(super) fn begin_intl_plural_rules_constructor(
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
            "Intl.PluralRules requires 'new'",
        );
    };
    let locales = inputs.arguments.take_first_or_undefined();
    let options_argument = inputs.arguments.take_first_or_undefined();
    let state = IntlPluralRulesConstructorContinuation {
        new_target,
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        options: PluralRulesRequestOptions::default(),
        raw_digits: core::array::from_fn(|_| None),
        resolved: None,
        option_index: 0,
        raw_digit_index: 0,
        realm,
        stage: IntlPluralRulesConstructorStage::ReadOption,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::PluralRulesConstructor(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_plural_rules_options(
    runtime: &mut Runtime,
    mut state: IntlPluralRulesConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        state.stage = IntlPluralRulesConstructorStage::ConvertRawDigit;
        return advance_intl_plural_rules_constructor(
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
    advance_intl_plural_rules_constructor(runtime, state, None, return_to, execution_budget)
}

#[allow(
    clippy::too_many_lines,
    reason = "PluralRules option Gets and delayed digit conversions remain in normative observable order"
)]
pub(super) fn advance_intl_plural_rules_constructor(
    runtime: &mut Runtime,
    mut state: IntlPluralRulesConstructorContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            IntlPluralRulesConstructorStage::ReadOption => {
                let Some(option) = IntlPluralRulesOption::ALL.get(state.option_index).copied()
                else {
                    state.stage = IntlPluralRulesConstructorStage::ConvertRawDigit;
                    continue;
                };
                let base = state
                    .options_object
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Intl.PluralRules option iteration lost its options object",
                    })?
                    .duplicate();
                charge_heap_property_lookup(runtime, &base, execution_budget)?;
                let name = JsString::from_utf8(option.name())?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = IntlPluralRulesConstructorStage::AwaitOption;
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
                return continue_intl_plural_rules_constructor_after(
                    dispatch,
                    state,
                    runtime,
                    return_to,
                    execution_budget,
                );
            }
            IntlPluralRulesConstructorStage::AwaitOption => {
                let value = take_intl_plural_rules_constructor_completion(&mut completion)?;
                let option = IntlPluralRulesOption::ALL[state.option_index];
                if matches!(value, StoredValue::Undefined) {
                    advance_intl_plural_rules_option(&mut state);
                    continue;
                }
                if let Some(index) = option.raw_digit_index() {
                    state.raw_digits[index] = Some(value);
                    advance_intl_plural_rules_option(&mut state);
                    continue;
                }
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    state.stage = IntlPluralRulesConstructorStage::AwaitOptionPrimitive;
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
                        OperatorPrimitiveTarget::IntlPluralRulesConstructor(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                store_intl_plural_rules_option(&mut state, option, value)?;
                advance_intl_plural_rules_option(&mut state);
            }
            IntlPluralRulesConstructorStage::AwaitOptionPrimitive => {
                let value = take_intl_plural_rules_constructor_completion(&mut completion)?;
                let option = IntlPluralRulesOption::ALL[state.option_index];
                store_intl_plural_rules_option(&mut state, option, value)?;
                advance_intl_plural_rules_option(&mut state);
            }
            IntlPluralRulesConstructorStage::ConvertRawDigit => {
                let Some(value) = state
                    .raw_digits
                    .get_mut(state.raw_digit_index)
                    .and_then(Option::take)
                else {
                    state.raw_digit_index = state.raw_digit_index.saturating_add(1);
                    if state.raw_digit_index >= state.raw_digits.len() {
                        return finish_intl_plural_rules_options(
                            runtime,
                            state,
                            return_to,
                            execution_budget,
                        );
                    }
                    continue;
                };
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    state.stage = IntlPluralRulesConstructorStage::AwaitRawDigitPrimitive;
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::Number,
                        OperatorPrimitiveTarget::IntlPluralRulesConstructor(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                store_intl_plural_rules_raw_digit(&mut state, value)?;
                state.raw_digit_index = state.raw_digit_index.saturating_add(1);
            }
            IntlPluralRulesConstructorStage::AwaitRawDigitPrimitive => {
                let value = take_intl_plural_rules_constructor_completion(&mut completion)?;
                store_intl_plural_rules_raw_digit(&mut state, value)?;
                state.raw_digit_index = state.raw_digit_index.saturating_add(1);
                state.stage = IntlPluralRulesConstructorStage::ConvertRawDigit;
            }
            IntlPluralRulesConstructorStage::AwaitPrototype => {
                let requested = take_intl_plural_rules_constructor_completion(&mut completion)?;
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
                        HeapReference::Object(
                            runtime.realm_intl_plural_rules_prototype(target_realm)?,
                        )
                    }
                };
                let resolved = state.resolved.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.PluralRules allocation lost its resolved slots",
                })?;
                let object = runtime.allocate_intl_plural_rules(prototype, resolved)?;
                return Ok(NativeDispatch::Immediate(StoredValue::Object(object)));
            }
        }
    }
}

fn advance_intl_plural_rules_option(state: &mut IntlPluralRulesConstructorContinuation) {
    state.option_index = state.option_index.saturating_add(1);
    state.stage = IntlPluralRulesConstructorStage::ReadOption;
}

fn store_intl_plural_rules_option(
    state: &mut IntlPluralRulesConstructorContinuation,
    option: IntlPluralRulesOption,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    if option.is_immediate_number() {
        let number = operator_to_number(value, state.realm, &state.origin)?.as_f64();
        return match option {
            IntlPluralRulesOption::MinimumIntegerDigits => {
                state.options.minimum_integer_digits =
                    Some(plural_rules_number_option_u8(state, option, number, 1, 21)?);
                Ok(())
            }
            IntlPluralRulesOption::RoundingIncrement => {
                let rounded = plural_rules_number_option_u16(state, option, number, 1, 5000)?;
                if !ALLOWED_NUMBER_FORMAT_ROUNDING_INCREMENTS.contains(&rounded) {
                    return invalid_intl_plural_rules_option(state, option);
                }
                state.options.rounding_increment = Some(rounded);
                Ok(())
            }
            _ => Err(EngineFault::RuntimeInvariant {
                message: "non-numeric PluralRules option reached numeric storage",
            }
            .into()),
        };
    }

    let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let text = text.to_utf8_lossy()?;
    match option {
        IntlPluralRulesOption::LocaleMatcher => {
            if !matches!(text.as_str(), "lookup" | "best fit") {
                return invalid_intl_plural_rules_option(state, option);
            }
        }
        IntlPluralRulesOption::Type => {
            state.options.rule_type = Some(match text.as_str() {
                "cardinal" => PluralRuleType::Cardinal,
                "ordinal" => PluralRuleType::Ordinal,
                _ => return invalid_intl_plural_rules_option(state, option),
            });
        }
        IntlPluralRulesOption::Notation => {
            state.options.notation = Some(match text.as_str() {
                "standard" => NumberFormatNotation::Standard,
                "scientific" => NumberFormatNotation::Scientific,
                "engineering" => NumberFormatNotation::Engineering,
                "compact" => NumberFormatNotation::Compact,
                _ => return invalid_intl_plural_rules_option(state, option),
            });
        }
        IntlPluralRulesOption::CompactDisplay => {
            state.options.compact_display = Some(match text.as_str() {
                "short" => NumberFormatCompactDisplay::Short,
                "long" => NumberFormatCompactDisplay::Long,
                _ => return invalid_intl_plural_rules_option(state, option),
            });
        }
        IntlPluralRulesOption::RoundingMode => {
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
                _ => return invalid_intl_plural_rules_option(state, option),
            });
        }
        IntlPluralRulesOption::RoundingPriority => {
            state.options.rounding_priority = Some(match text.as_str() {
                "auto" => NumberFormatRoundingPriority::Auto,
                "morePrecision" => NumberFormatRoundingPriority::MorePrecision,
                "lessPrecision" => NumberFormatRoundingPriority::LessPrecision,
                _ => return invalid_intl_plural_rules_option(state, option),
            });
        }
        IntlPluralRulesOption::TrailingZeroDisplay => {
            state.options.trailing_zero_display = Some(match text.as_str() {
                "auto" => NumberFormatTrailingZeroDisplay::Auto,
                "stripIfInteger" => NumberFormatTrailingZeroDisplay::StripIfInteger,
                _ => return invalid_intl_plural_rules_option(state, option),
            });
        }
        IntlPluralRulesOption::MinimumIntegerDigits
        | IntlPluralRulesOption::MinimumFractionDigits
        | IntlPluralRulesOption::MaximumFractionDigits
        | IntlPluralRulesOption::MinimumSignificantDigits
        | IntlPluralRulesOption::MaximumSignificantDigits
        | IntlPluralRulesOption::RoundingIncrement => {
            return Err(EngineFault::RuntimeInvariant {
                message: "a non-string PluralRules option reached string storage",
            }
            .into());
        }
    }
    Ok(())
}

fn store_intl_plural_rules_raw_digit(
    state: &mut IntlPluralRulesConstructorContinuation,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    let number = operator_to_number(value, state.realm, &state.origin)?.as_f64();
    let (minimum, maximum) = if state.raw_digit_index < 2 {
        (0, 100)
    } else {
        (1, 21)
    };
    let option = IntlPluralRulesOption::ALL[5 + state.raw_digit_index];
    let value = plural_rules_number_option_u8(state, option, number, minimum, maximum)?;
    match state.raw_digit_index {
        0 => state.options.minimum_fraction_digits = Some(value),
        1 => state.options.maximum_fraction_digits = Some(value),
        2 => state.options.minimum_significant_digits = Some(value),
        3 => state.options.maximum_significant_digits = Some(value),
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "PluralRules raw digit index is out of range",
            }
            .into());
        }
    }
    Ok(())
}

fn plural_rules_number_option_u8(
    state: &IntlPluralRulesConstructorContinuation,
    option: IntlPluralRulesOption,
    number: f64,
    minimum: u8,
    maximum: u8,
) -> Result<u8, NativeFailure> {
    if !number.is_finite() || number < f64::from(minimum) || number > f64::from(maximum) {
        return invalid_intl_plural_rules_option(state, option);
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the finite value is bounded to the inclusive u8 option range above"
    )]
    let integer = number.floor() as u8;
    Ok(integer)
}

fn plural_rules_number_option_u16(
    state: &IntlPluralRulesConstructorContinuation,
    option: IntlPluralRulesOption,
    number: f64,
    minimum: u16,
    maximum: u16,
) -> Result<u16, NativeFailure> {
    if !number.is_finite() || number < f64::from(minimum) || number > f64::from(maximum) {
        return invalid_intl_plural_rules_option(state, option);
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the finite value is bounded to the inclusive u16 option range above"
    )]
    let integer = number.floor() as u16;
    Ok(integer)
}

fn invalid_intl_plural_rules_option<T>(
    state: &IntlPluralRulesConstructorContinuation,
    option: IntlPluralRulesOption,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        state.realm,
        state.origin.clone(),
        ExceptionKind::RangeError,
        &format!("invalid Intl.PluralRules {} option", option.name()),
    )
}

fn finish_intl_plural_rules_options(
    runtime: &mut Runtime,
    mut state: IntlPluralRulesConstructorContinuation,
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
            state.origin,
            ExceptionKind::TypeError,
            "roundingIncrement is incompatible with significant-digit rounding",
        );
    }
    execution_budget
        .charge_instructions(usize_to_u64(state.requested_locales.len()).saturating_add(1))?;
    let resolved = resolve_plural_rules(&state.requested_locales, state.options).map_err(|_| {
        NativeFailure::Abrupt(PendingException {
            realm: state.realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("invalid Intl.PluralRules options")
                    .expect("static Intl error message is valid"),
            },
            origin: state.origin.clone(),
        })
    })?;
    state.resolved = Some(resolved);
    state.stage = IntlPluralRulesConstructorStage::AwaitPrototype;
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
    continue_intl_plural_rules_constructor_after(
        dispatch,
        state,
        runtime,
        return_to,
        execution_budget,
    )
}

fn continue_intl_plural_rules_constructor_after(
    dispatch: NativeDispatch,
    state: IntlPluralRulesConstructorContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        |state| NativeContinuation::IntlPluralRulesConstructor(Box::new(state)),
        |state, value| {
            advance_intl_plural_rules_constructor(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.PluralRules property Get produced a structured result",
    )
}

fn take_intl_plural_rules_constructor_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.PluralRules constructor resumed without a completion",
    })
}

pub(super) fn begin_intl_plural_rules_supported_locales_of(
    runtime: &mut Runtime,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let locales = arguments.take_first_or_undefined();
    let options_argument = arguments.take_first_or_undefined();
    let state = IntlPluralRulesSupportedLocalesContinuation {
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        realm,
        stage: IntlPluralRulesSupportedLocalesStage::ReadLocaleMatcher,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::PluralRulesSupportedLocalesOf(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_plural_rules_supported_locales_options(
    runtime: &mut Runtime,
    mut state: IntlPluralRulesSupportedLocalesContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_plural_rules_supported_locales(runtime, &state);
    }
    let options_argument = state.options_argument.duplicate();
    state.options_object = Some(
        match to_object_value(runtime, state.realm, options_argument, state.origin.clone())? {
            Ok(options) => options,
            Err(exception) => return Err(NativeFailure::Abrupt(exception)),
        },
    );
    advance_intl_plural_rules_supported_locales(runtime, state, None, return_to, execution_budget)
}

pub(super) fn advance_intl_plural_rules_supported_locales(
    runtime: &mut Runtime,
    mut state: IntlPluralRulesSupportedLocalesContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    match state.stage {
        IntlPluralRulesSupportedLocalesStage::ReadLocaleMatcher => {
            let base = state
                .options_object
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.PluralRules.supportedLocalesOf lost its options object",
                })?
                .duplicate();
            charge_heap_property_lookup(runtime, &base, execution_budget)?;
            let name = JsString::from_utf8("localeMatcher")?;
            let key = runtime.property_key_from_string(&name)?;
            state.stage = IntlPluralRulesSupportedLocalesStage::AwaitLocaleMatcher;
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
            continue_intl_plural_rules_supported_locales_after(
                dispatch,
                state,
                runtime,
                return_to,
                execution_budget,
            )
        }
        IntlPluralRulesSupportedLocalesStage::AwaitLocaleMatcher => {
            let value = take_intl_plural_rules_supported_locales_completion(&mut completion)?;
            if matches!(value, StoredValue::Undefined) {
                return finish_intl_plural_rules_supported_locales(runtime, &state);
            }
            if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                state.stage = IntlPluralRulesSupportedLocalesStage::AwaitLocaleMatcherPrimitive;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::IntlPluralRulesSupportedLocalesOf(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            validate_intl_plural_rules_locale_matcher(&state, value)?;
            finish_intl_plural_rules_supported_locales(runtime, &state)
        }
        IntlPluralRulesSupportedLocalesStage::AwaitLocaleMatcherPrimitive => {
            let value = take_intl_plural_rules_supported_locales_completion(&mut completion)?;
            validate_intl_plural_rules_locale_matcher(&state, value)?;
            finish_intl_plural_rules_supported_locales(runtime, &state)
        }
    }
}

fn validate_intl_plural_rules_locale_matcher(
    state: &IntlPluralRulesSupportedLocalesContinuation,
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
        "invalid Intl.PluralRules localeMatcher option",
    )
}

fn finish_intl_plural_rules_supported_locales(
    runtime: &mut Runtime,
    state: &IntlPluralRulesSupportedLocalesContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    intl_locale_string_array(
        runtime,
        state.realm,
        plural_rules_supported_locales(&state.requested_locales),
    )
}

fn continue_intl_plural_rules_supported_locales_after(
    dispatch: NativeDispatch,
    state: IntlPluralRulesSupportedLocalesContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        |state| NativeContinuation::IntlPluralRulesSupportedLocalesOf(Box::new(state)),
        |state, value| {
            advance_intl_plural_rules_supported_locales(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.PluralRules.supportedLocalesOf Get produced a structured result",
    )
}

fn take_intl_plural_rules_supported_locales_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.PluralRules.supportedLocalesOf resumed without a completion",
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch keeps receiver, arguments, realm, return target, origin, and budget explicit"
)]
pub(super) fn begin_intl_plural_rules_prototype(
    runtime: &mut Runtime,
    method: IntlPluralRulesPrototypeMethod,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(plural_rules) = receiver else {
        return intl_plural_rules_brand_error(realm, origin);
    };
    let Some(resolved) = runtime.intl_plural_rules_state(*plural_rules)?.cloned() else {
        return intl_plural_rules_brand_error(realm, origin);
    };
    match method {
        IntlPluralRulesPrototypeMethod::ResolvedOptions => {
            intl_plural_rules_resolved_options(runtime, realm, &resolved)
        }
        IntlPluralRulesPrototypeMethod::Select => {
            let value = arguments.take_first_or_undefined();
            begin_intl_plural_rules_value(
                runtime,
                IntlPluralRulesValueContinuation {
                    plural_rules: *plural_rules,
                    operation: IntlPluralRulesOperation::Select,
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
        IntlPluralRulesPrototypeMethod::SelectRange => {
            let first = arguments.take_first_or_undefined();
            let second = arguments.take_first_or_undefined();
            if matches!(first, StoredValue::Undefined) || matches!(second, StoredValue::Undefined) {
                return intl_locale_list_error(
                    realm,
                    origin,
                    ExceptionKind::TypeError,
                    "Intl.PluralRules range arguments must not be undefined",
                );
            }
            begin_intl_plural_rules_value(
                runtime,
                IntlPluralRulesValueContinuation {
                    plural_rules: *plural_rules,
                    operation: IntlPluralRulesOperation::SelectRange,
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

fn begin_intl_plural_rules_value(
    runtime: &mut Runtime,
    state: IntlPluralRulesValueContinuation,
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
            OperatorPrimitiveTarget::IntlPluralRulesValue(Box::new(state)),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    finish_intl_plural_rules_value_primitive(runtime, state, value, return_to, execution_budget)
}

pub(super) fn finish_intl_plural_rules_value_primitive(
    runtime: &mut Runtime,
    mut state: IntlPluralRulesValueContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let value = to_intl_mathematical_value(value, state.realm, &state.origin)?;
    if state.operation == IntlPluralRulesOperation::SelectRange && state.first.is_none() {
        state.first = Some(value);
        let second = state.second.take().ok_or(EngineFault::RuntimeInvariant {
            message: "Intl.PluralRules range conversion lost its second operand",
        })?;
        return begin_intl_plural_rules_value(runtime, state, second, return_to, execution_budget);
    }
    finish_intl_plural_rules_operation(runtime, state, &value)
}

fn finish_intl_plural_rules_operation(
    runtime: &Runtime,
    state: IntlPluralRulesValueContinuation,
    value: &IntlMathematicalValue,
) -> Result<NativeDispatch, NativeFailure> {
    let resolved = runtime.intl_plural_rules_state(state.plural_rules)?.ok_or(
        EngineFault::RuntimeInvariant {
            message: "Intl.PluralRules operation lost its branded receiver",
        },
    )?;
    let category = match state.operation {
        IntlPluralRulesOperation::Select => select_plural(resolved, value),
        IntlPluralRulesOperation::SelectRange => {
            let first = state.first.ok_or(EngineFault::RuntimeInvariant {
                message: "Intl.PluralRules range operation lost its first operand",
            })?;
            if matches!(first, IntlMathematicalValue::NaN)
                || matches!(value, IntlMathematicalValue::NaN)
            {
                return intl_locale_list_error(
                    state.realm,
                    state.origin,
                    ExceptionKind::RangeError,
                    "Intl.PluralRules range arguments must not be NaN",
                );
            }
            select_plural_range(resolved, &first, value)
        }
    }
    .map_err(|_| EngineFault::RuntimeInvariant {
        message: "resolved Intl.PluralRules slots failed plural selection",
    })?;
    Ok(NativeDispatch::Immediate(StoredValue::String(
        JsString::from_utf8(category.as_str())?,
    )))
}

#[allow(
    clippy::too_many_lines,
    reason = "resolvedOptions property order mirrors the ECMA-402 PluralRules algorithm"
)]
fn intl_plural_rules_resolved_options(
    runtime: &mut Runtime,
    realm: RealmId,
    state: &PluralRulesState,
) -> Result<NativeDispatch, NativeFailure> {
    let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    let mut properties = Vec::new();
    properties.push((
        "locale",
        StoredValue::String(JsString::from_utf8(&state.locale)?),
    ));
    properties.push((
        "type",
        StoredValue::String(JsString::from_utf8(state.rule_type.as_str())?),
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
    let categories = state
        .plural_categories
        .iter()
        .map(|category| {
            JsString::from_utf8(category.as_str())
                .map(StoredValue::String)
                .map_err(NativeFailure::from)
        })
        .collect::<Result<Vec<_>, _>>()?;
    properties.push((
        "pluralCategories",
        StoredValue::Object(runtime.allocate_array(realm, categories)?),
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

fn intl_plural_rules_brand_error<T>(
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        realm,
        origin,
        ExceptionKind::TypeError,
        "Intl.PluralRules method called on incompatible receiver",
    )
}

pub(super) fn begin_intl_date_time_format_constructor(
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
    let state = IntlDateTimeFormatConstructorContinuation {
        new_target,
        format_value: None,
        required: IntlDateTimeFormatRequired::Any,
        defaults: IntlDateTimeFormatDefaults::Date,
        legacy_receiver,
        legacy_date_time_format: None,
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        options: DateTimeFormatRequestOptions::default(),
        resolved: None,
        option_index: 0,
        realm,
        stage: IntlDateTimeFormatConstructorStage::ReadOption,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::DateTimeFormatConstructor(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch keeps the Date service, arguments, realm, return target, origin, and budget explicit"
)]
pub(super) fn begin_intl_date_to_locale_string(
    runtime: &mut Runtime,
    method: DatePrototypeMethod,
    mut arguments: CallArguments,
    value: JsNumber,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if !value.as_f64().is_finite() {
        return Ok(NativeDispatch::Immediate(StoredValue::String(
            JsString::from_utf8("Invalid Date")?,
        )));
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a finite Date slot is TimeClip-bounded to an integral i64 millisecond value"
    )]
    let milliseconds = value.as_f64() as i64;
    let instant = temporal_rs::Instant::from_epoch_milliseconds(milliseconds).map_err(|_| {
        EngineFault::RuntimeInvariant {
            message: "a valid Date slot did not produce a Temporal instant",
        }
    })?;
    let (required, defaults) = match method {
        DatePrototypeMethod::ToLocaleString => (
            IntlDateTimeFormatRequired::Any,
            IntlDateTimeFormatDefaults::All,
        ),
        DatePrototypeMethod::ToLocaleDateString => (
            IntlDateTimeFormatRequired::Date,
            IntlDateTimeFormatDefaults::Date,
        ),
        DatePrototypeMethod::ToLocaleTimeString => (
            IntlDateTimeFormatRequired::Time,
            IntlDateTimeFormatDefaults::Time,
        ),
        _ => {
            return Err(EngineFault::RuntimeInvariant {
                message: "a non-locale Date method reached Intl.DateTimeFormat",
            }
            .into());
        }
    };
    let locales = arguments.take_first_or_undefined();
    let options_argument = arguments.take_first_or_undefined();
    let new_target = runtime.realm_intl_date_time_format_constructor(realm)?;
    let state = IntlDateTimeFormatConstructorContinuation {
        new_target,
        format_value: Some(instant),
        required,
        defaults,
        legacy_receiver: None,
        legacy_date_time_format: None,
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        options: DateTimeFormatRequestOptions::default(),
        resolved: None,
        option_index: 0,
        realm,
        stage: IntlDateTimeFormatConstructorStage::ReadOption,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::DateTimeFormatConstructor(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_date_time_format_options(
    runtime: &mut Runtime,
    mut state: IntlDateTimeFormatConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_date_time_format_options(runtime, state, return_to, execution_budget);
    }
    let options_argument = state.options_argument.duplicate();
    state.options_object = Some(
        match to_object_value(runtime, state.realm, options_argument, state.origin.clone())? {
            Ok(options) => options,
            Err(exception) => return Err(NativeFailure::Abrupt(exception)),
        },
    );
    state.stage = IntlDateTimeFormatConstructorStage::ReadOption;
    advance_intl_date_time_format_constructor(runtime, state, None, return_to, execution_budget)
}

#[allow(
    clippy::too_many_lines,
    reason = "DateTimeFormat option Gets and coercions remain in normative observable order"
)]
pub(super) fn advance_intl_date_time_format_constructor(
    runtime: &mut Runtime,
    mut state: IntlDateTimeFormatConstructorContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            IntlDateTimeFormatConstructorStage::ReadOption => {
                let Some(option) = IntlDateTimeFormatOption::ALL
                    .get(state.option_index)
                    .copied()
                else {
                    return finish_intl_date_time_format_options(
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
                        message: "Intl.DateTimeFormat option iteration lost its options object",
                    })?
                    .duplicate();
                charge_heap_property_lookup(runtime, &base, execution_budget)?;
                let name = JsString::from_utf8(option.name())?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = IntlDateTimeFormatConstructorStage::AwaitOption;
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
                return continue_intl_date_time_format_constructor_after(
                    dispatch,
                    state,
                    runtime,
                    return_to,
                    execution_budget,
                );
            }
            IntlDateTimeFormatConstructorStage::AwaitOption => {
                let value = take_intl_date_time_format_constructor_completion(&mut completion)?;
                let option = IntlDateTimeFormatOption::ALL[state.option_index];
                if matches!(value, StoredValue::Undefined) {
                    advance_intl_date_time_format_option(&mut state);
                    continue;
                }
                if option == IntlDateTimeFormatOption::Hour12 {
                    state.options.hour12 = Some(value.is_truthy());
                    advance_intl_date_time_format_option(&mut state);
                    continue;
                }
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    state.stage = IntlDateTimeFormatConstructorStage::AwaitOptionPrimitive;
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        option.primitive_hint(),
                        OperatorPrimitiveTarget::IntlDateTimeFormatConstructor(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                store_intl_date_time_format_option(&mut state, option, value)?;
                advance_intl_date_time_format_option(&mut state);
            }
            IntlDateTimeFormatConstructorStage::AwaitOptionPrimitive => {
                let primitive = take_intl_date_time_format_constructor_completion(&mut completion)?;
                let option = IntlDateTimeFormatOption::ALL[state.option_index];
                store_intl_date_time_format_option(&mut state, option, primitive)?;
                advance_intl_date_time_format_option(&mut state);
            }
            IntlDateTimeFormatConstructorStage::AwaitPrototype => {
                let requested = take_intl_date_time_format_constructor_completion(&mut completion)?;
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
                        HeapReference::Object(
                            runtime.realm_intl_date_time_format_prototype(target_realm)?,
                        )
                    }
                };
                let resolved = state.resolved.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.DateTimeFormat allocation lost its resolved slots",
                })?;
                let object = runtime.allocate_intl_date_time_format(prototype, resolved)?;
                if let Some(receiver) = state.legacy_receiver.as_ref().map(StoredValue::duplicate) {
                    state.legacy_date_time_format = Some(object);
                    state.stage = IntlDateTimeFormatConstructorStage::AwaitLegacyInstance;
                    let constructor =
                        runtime.realm_intl_date_time_format_constructor(state.realm)?;
                    let dispatch = begin_function_has_instance(
                        runtime,
                        state.realm,
                        receiver,
                        StoredValue::Function(constructor),
                        return_to,
                        state.origin.clone(),
                        execution_budget,
                    )?;
                    return continue_intl_date_time_format_constructor_after(
                        dispatch,
                        state,
                        runtime,
                        return_to,
                        execution_budget,
                    );
                }
                return Ok(NativeDispatch::Immediate(StoredValue::Object(object)));
            }
            IntlDateTimeFormatConstructorStage::AwaitLegacyInstance => {
                let completion =
                    take_intl_date_time_format_constructor_completion(&mut completion)?;
                let date_time_format =
                    state
                        .legacy_date_time_format
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "Intl.DateTimeFormat legacy chain lost its initialized object",
                        })?;
                if !completion.is_truthy() {
                    return Ok(NativeDispatch::Immediate(StoredValue::Object(
                        date_time_format,
                    )));
                }
                let receiver = state.legacy_receiver.ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.DateTimeFormat legacy chain lost its receiver",
                })?;
                let reference = receiver.heap_reference().ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.DateTimeFormat legacy receiver passed instanceof as a primitive",
                })?;
                let symbol = runtime.intl_number_format_fallback_symbol();
                let key = runtime.property_key_from_symbol(&symbol)?;
                let definition = PropertyDefinition::data(
                    Requested::Present(StoredValue::Object(date_time_format)),
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

fn advance_intl_date_time_format_option(state: &mut IntlDateTimeFormatConstructorContinuation) {
    state.option_index = state.option_index.saturating_add(1);
    state.stage = IntlDateTimeFormatConstructorStage::ReadOption;
}

#[allow(
    clippy::too_many_lines,
    reason = "the closed ECMA-402 DateTimeFormat option vocabulary is audited in one match"
)]
fn store_intl_date_time_format_option(
    state: &mut IntlDateTimeFormatConstructorContinuation,
    option: IntlDateTimeFormatOption,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    if option == IntlDateTimeFormatOption::FractionalSecondDigits {
        let number = operator_to_number(value, state.realm, &state.origin)?.as_f64();
        if !number.is_finite() || !(1.0..=3.0).contains(&number) {
            return invalid_intl_date_time_format_option(state, option);
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the finite value is bounded to the inclusive 1 through 3 option range"
        )]
        let digits = number.floor() as u8;
        state.options.fractional_second_digits = Some(digits);
        return Ok(());
    }
    if option == IntlDateTimeFormatOption::Hour12 {
        return Err(EngineFault::RuntimeInvariant {
            message: "Intl.DateTimeFormat hour12 reached string option storage",
        }
        .into());
    }

    let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let text = text.to_utf8_lossy()?;
    match option {
        IntlDateTimeFormatOption::LocaleMatcher => {
            if !matches!(text.as_str(), "lookup" | "best fit") {
                return invalid_intl_date_time_format_option(state, option);
            }
        }
        IntlDateTimeFormatOption::Calendar => {
            state.options.calendar = Some(
                canonicalize_locale_option(LocaleOptionKind::Calendar, &text)
                    .map_err(|_| invalid_intl_date_time_format_option_failure(state, option))?,
            );
        }
        IntlDateTimeFormatOption::NumberingSystem => {
            state.options.numbering_system = Some(
                canonicalize_locale_option(LocaleOptionKind::NumberingSystem, &text)
                    .map_err(|_| invalid_intl_date_time_format_option_failure(state, option))?,
            );
        }
        IntlDateTimeFormatOption::HourCycle => {
            state.options.hour_cycle = Some(match text.as_str() {
                "h11" => DateTimeHourCycle::H11,
                "h12" => DateTimeHourCycle::H12,
                "h23" => DateTimeHourCycle::H23,
                "h24" => DateTimeHourCycle::H24,
                _ => return invalid_intl_date_time_format_option(state, option),
            });
        }
        IntlDateTimeFormatOption::TimeZone => {
            state.options.time_zone = Some(
                canonicalize_time_zone(&text)
                    .map_err(|_| invalid_intl_date_time_format_option_failure(state, option))?,
            );
        }
        IntlDateTimeFormatOption::Weekday
        | IntlDateTimeFormatOption::Era
        | IntlDateTimeFormatOption::DayPeriod => {
            let style = match text.as_str() {
                "narrow" => DateTimeComponentStyle::Narrow,
                "short" => DateTimeComponentStyle::Short,
                "long" => DateTimeComponentStyle::Long,
                _ => return invalid_intl_date_time_format_option(state, option),
            };
            match option {
                IntlDateTimeFormatOption::Weekday => state.options.weekday = Some(style),
                IntlDateTimeFormatOption::Era => state.options.era = Some(style),
                IntlDateTimeFormatOption::DayPeriod => state.options.day_period = Some(style),
                _ => unreachable!("component option group is closed"),
            }
        }
        IntlDateTimeFormatOption::Year
        | IntlDateTimeFormatOption::Day
        | IntlDateTimeFormatOption::Hour
        | IntlDateTimeFormatOption::Minute
        | IntlDateTimeFormatOption::Second => {
            let style = numeric_date_time_component(state, option, &text)?;
            match option {
                IntlDateTimeFormatOption::Year => state.options.year = Some(style),
                IntlDateTimeFormatOption::Day => state.options.day = Some(style),
                IntlDateTimeFormatOption::Hour => state.options.hour = Some(style),
                IntlDateTimeFormatOption::Minute => state.options.minute = Some(style),
                IntlDateTimeFormatOption::Second => state.options.second = Some(style),
                _ => unreachable!("numeric component option group is closed"),
            }
        }
        IntlDateTimeFormatOption::Month => {
            state.options.month = Some(match text.as_str() {
                "numeric" => DateTimeComponentStyle::Numeric,
                "2-digit" => DateTimeComponentStyle::TwoDigit,
                "narrow" => DateTimeComponentStyle::Narrow,
                "short" => DateTimeComponentStyle::Short,
                "long" => DateTimeComponentStyle::Long,
                _ => return invalid_intl_date_time_format_option(state, option),
            });
        }
        IntlDateTimeFormatOption::TimeZoneName => {
            state.options.time_zone_name = Some(match text.as_str() {
                "short" => DateTimeTimeZoneName::Short,
                "long" => DateTimeTimeZoneName::Long,
                "shortOffset" => DateTimeTimeZoneName::ShortOffset,
                "longOffset" => DateTimeTimeZoneName::LongOffset,
                "shortGeneric" => DateTimeTimeZoneName::ShortGeneric,
                "longGeneric" => DateTimeTimeZoneName::LongGeneric,
                _ => return invalid_intl_date_time_format_option(state, option),
            });
        }
        IntlDateTimeFormatOption::FormatMatcher => {
            state.options.format_matcher = Some(match text.as_str() {
                "basic" => DateTimeFormatMatcher::Basic,
                "best fit" => DateTimeFormatMatcher::BestFit,
                _ => return invalid_intl_date_time_format_option(state, option),
            });
        }
        IntlDateTimeFormatOption::DateStyle | IntlDateTimeFormatOption::TimeStyle => {
            let style = match text.as_str() {
                "full" => DateTimeStyle::Full,
                "long" => DateTimeStyle::Long,
                "medium" => DateTimeStyle::Medium,
                "short" => DateTimeStyle::Short,
                _ => return invalid_intl_date_time_format_option(state, option),
            };
            if option == IntlDateTimeFormatOption::DateStyle {
                state.options.date_style = Some(style);
            } else {
                state.options.time_style = Some(style);
            }
        }
        IntlDateTimeFormatOption::Hour12 | IntlDateTimeFormatOption::FractionalSecondDigits => {
            return Err(EngineFault::RuntimeInvariant {
                message: "a non-string Intl.DateTimeFormat option reached string storage",
            }
            .into());
        }
    }
    Ok(())
}

fn numeric_date_time_component(
    state: &IntlDateTimeFormatConstructorContinuation,
    option: IntlDateTimeFormatOption,
    text: &str,
) -> Result<DateTimeComponentStyle, NativeFailure> {
    match text {
        "numeric" => Ok(DateTimeComponentStyle::Numeric),
        "2-digit" => Ok(DateTimeComponentStyle::TwoDigit),
        _ => invalid_intl_date_time_format_option(state, option),
    }
}

fn invalid_intl_date_time_format_option_failure(
    state: &IntlDateTimeFormatConstructorContinuation,
    option: IntlDateTimeFormatOption,
) -> NativeFailure {
    NativeFailure::Abrupt(PendingException {
        realm: state.realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::RangeError,
            message: JsString::from_utf8(&format!(
                "invalid Intl.DateTimeFormat {} option",
                option.name()
            ))
            .expect("static Intl error prefix and ASCII option are valid UTF-8"),
        },
        origin: state.origin.clone(),
    })
}

fn invalid_intl_date_time_format_option<T>(
    state: &IntlDateTimeFormatConstructorContinuation,
    option: IntlDateTimeFormatOption,
) -> Result<T, NativeFailure> {
    Err(invalid_intl_date_time_format_option_failure(state, option))
}

fn finish_intl_date_time_format_options(
    runtime: &mut Runtime,
    mut state: IntlDateTimeFormatConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    apply_intl_date_time_format_defaults(&mut state)?;
    if state.options.time_zone.is_none() {
        let time_zone = temporal_rs::Temporal::local_now()
            .time_zone()
            .and_then(|time_zone| time_zone.identifier())
            .map_err(|_| EngineFault::RuntimeInvariant {
                message: "the host did not provide a valid system time-zone identifier",
            })?;
        state.options.time_zone = Some(time_zone);
    }
    execution_budget
        .charge_instructions(usize_to_u64(state.requested_locales.len()).saturating_add(1))?;
    let resolved = resolve_date_time_format(&state.requested_locales, state.options.clone())
        .map_err(|error| {
            let kind = if matches!(
                error,
                DateTimeFormatError::NoFields | DateTimeFormatError::InvalidOption
            ) {
                ExceptionKind::TypeError
            } else {
                ExceptionKind::RangeError
            };
            NativeFailure::Abrupt(PendingException {
                realm: state.realm,
                payload: PendingExceptionPayload::EngineError {
                    kind,
                    message: JsString::from_utf8("invalid Intl.DateTimeFormat options")
                        .expect("static Intl error message is valid"),
                },
                origin: state.origin.clone(),
            })
        })?;
    if let Some(instant) = state.format_value.take() {
        let input = project_epoch_date_time_input(
            &resolved.time_zone,
            instant,
            DateTimeFormatInputKind::Epoch,
            IntlDateTimeInputIdentity::Number,
        )?;
        let formatted = format_datetime(&resolved, &input.value).map_err(|error| match error {
            DateTimeFormatError::InvalidDateTime => NativeFailure::Abrupt(PendingException {
                realm: state.realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::RangeError,
                    message: JsString::from_utf8("Intl.DateTimeFormat value is outside its range")
                        .expect("static Intl error is valid UTF-8"),
                },
                origin: state.origin.clone(),
            }),
            _ => EngineFault::RuntimeInvariant {
                message: "resolved Date locale-string slots failed formatting",
            }
            .into(),
        })?;
        return Ok(NativeDispatch::Immediate(StoredValue::String(
            JsString::from_utf8(&formatted)?,
        )));
    }
    state.resolved = Some(resolved);
    state.stage = IntlDateTimeFormatConstructorStage::AwaitPrototype;
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
    continue_intl_date_time_format_constructor_after(
        dispatch,
        state,
        runtime,
        return_to,
        execution_budget,
    )
}

fn apply_intl_date_time_format_defaults(
    state: &mut IntlDateTimeFormatConstructorContinuation,
) -> Result<(), NativeFailure> {
    if state.format_value.is_none() {
        return Ok(());
    }
    if (state.required == IntlDateTimeFormatRequired::Date && state.options.time_style.is_some())
        || (state.required == IntlDateTimeFormatRequired::Time
            && state.options.date_style.is_some())
    {
        return intl_locale_list_error(
            state.realm,
            state.origin.clone(),
            ExceptionKind::TypeError,
            "Intl.DateTimeFormat style is incompatible with the locale-string service",
        );
    }

    let has_date = state.options.weekday.is_some()
        || state.options.year.is_some()
        || state.options.month.is_some()
        || state.options.day.is_some();
    let has_time = state.options.day_period.is_some()
        || state.options.hour.is_some()
        || state.options.minute.is_some()
        || state.options.second.is_some()
        || state.options.fractional_second_digits.is_some();
    let need_defaults = match state.required {
        IntlDateTimeFormatRequired::Any => !has_date && !has_time,
        IntlDateTimeFormatRequired::Date => !has_date,
        IntlDateTimeFormatRequired::Time => !has_time,
    };
    if !need_defaults {
        return Ok(());
    }
    if matches!(
        state.defaults,
        IntlDateTimeFormatDefaults::Date | IntlDateTimeFormatDefaults::All
    ) {
        state.options.year = Some(DateTimeComponentStyle::Numeric);
        state.options.month = Some(DateTimeComponentStyle::Numeric);
        state.options.day = Some(DateTimeComponentStyle::Numeric);
    }
    if matches!(
        state.defaults,
        IntlDateTimeFormatDefaults::Time | IntlDateTimeFormatDefaults::All
    ) {
        state.options.hour = Some(DateTimeComponentStyle::Numeric);
        state.options.minute = Some(DateTimeComponentStyle::Numeric);
        state.options.second = Some(DateTimeComponentStyle::Numeric);
    }
    Ok(())
}

fn continue_intl_date_time_format_constructor_after(
    dispatch: NativeDispatch,
    state: IntlDateTimeFormatConstructorContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        intl_date_time_format_constructor_continuation,
        |state, value| {
            advance_intl_date_time_format_constructor(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.DateTimeFormat property Get produced a structured result",
    )
}

fn intl_date_time_format_constructor_continuation(
    state: IntlDateTimeFormatConstructorContinuation,
) -> NativeContinuation {
    NativeContinuation::IntlDateTimeFormatConstructor(Box::new(state))
}

fn take_intl_date_time_format_constructor_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.DateTimeFormat constructor resumed without a completion",
    })
}

pub(super) fn begin_intl_date_time_format_supported_locales_of(
    runtime: &mut Runtime,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let locales = arguments.take_first_or_undefined();
    let options_argument = arguments.take_first_or_undefined();
    let state = IntlDateTimeFormatSupportedLocalesContinuation {
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        realm,
        stage: IntlDateTimeFormatSupportedLocalesStage::ReadLocaleMatcher,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::DateTimeFormatSupportedLocalesOf(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_date_time_format_supported_locales_options(
    runtime: &mut Runtime,
    mut state: IntlDateTimeFormatSupportedLocalesContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_date_time_format_supported_locales(runtime, &state);
    }
    let options_argument = state.options_argument.duplicate();
    state.options_object = Some(
        match to_object_value(runtime, state.realm, options_argument, state.origin.clone())? {
            Ok(options) => options,
            Err(exception) => return Err(NativeFailure::Abrupt(exception)),
        },
    );
    advance_intl_date_time_format_supported_locales(
        runtime,
        state,
        None,
        return_to,
        execution_budget,
    )
}

pub(super) fn advance_intl_date_time_format_supported_locales(
    runtime: &mut Runtime,
    mut state: IntlDateTimeFormatSupportedLocalesContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    match state.stage {
        IntlDateTimeFormatSupportedLocalesStage::ReadLocaleMatcher => {
            let base = state
                .options_object
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.DateTimeFormat.supportedLocalesOf lost its options object",
                })?
                .duplicate();
            charge_heap_property_lookup(runtime, &base, execution_budget)?;
            let name = JsString::from_utf8("localeMatcher")?;
            let key = runtime.property_key_from_string(&name)?;
            state.stage = IntlDateTimeFormatSupportedLocalesStage::AwaitLocaleMatcher;
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
            continue_intl_date_time_format_supported_locales_after(
                dispatch,
                state,
                runtime,
                return_to,
                execution_budget,
            )
        }
        IntlDateTimeFormatSupportedLocalesStage::AwaitLocaleMatcher => {
            let value = take_intl_date_time_format_supported_locales_completion(&mut completion)?;
            if matches!(value, StoredValue::Undefined) {
                return finish_intl_date_time_format_supported_locales(runtime, &state);
            }
            if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                state.stage = IntlDateTimeFormatSupportedLocalesStage::AwaitLocaleMatcherPrimitive;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::IntlDateTimeFormatSupportedLocalesOf(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            validate_intl_date_time_format_locale_matcher(&state, value)?;
            finish_intl_date_time_format_supported_locales(runtime, &state)
        }
        IntlDateTimeFormatSupportedLocalesStage::AwaitLocaleMatcherPrimitive => {
            let value = take_intl_date_time_format_supported_locales_completion(&mut completion)?;
            validate_intl_date_time_format_locale_matcher(&state, value)?;
            finish_intl_date_time_format_supported_locales(runtime, &state)
        }
    }
}

fn validate_intl_date_time_format_locale_matcher(
    state: &IntlDateTimeFormatSupportedLocalesContinuation,
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
        "invalid Intl.DateTimeFormat localeMatcher option",
    )
}

fn finish_intl_date_time_format_supported_locales(
    runtime: &mut Runtime,
    state: &IntlDateTimeFormatSupportedLocalesContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    intl_locale_string_array(
        runtime,
        state.realm,
        date_time_format_supported_locales(&state.requested_locales),
    )
}

fn continue_intl_date_time_format_supported_locales_after(
    dispatch: NativeDispatch,
    state: IntlDateTimeFormatSupportedLocalesContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        intl_date_time_format_supported_locales_continuation,
        |state, value| {
            advance_intl_date_time_format_supported_locales(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.DateTimeFormat.supportedLocalesOf Get produced a structured result",
    )
}

fn intl_date_time_format_supported_locales_continuation(
    state: IntlDateTimeFormatSupportedLocalesContinuation,
) -> NativeContinuation {
    NativeContinuation::IntlDateTimeFormatSupportedLocalesOf(Box::new(state))
}

fn take_intl_date_time_format_supported_locales_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.DateTimeFormat.supportedLocalesOf resumed without a completion",
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch keeps receiver, arguments, realm, return target, origin, and budget explicit"
)]
pub(super) fn begin_intl_date_time_format_prototype(
    runtime: &mut Runtime,
    method: IntlDateTimeFormatPrototypeMethod,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(
        method,
        IntlDateTimeFormatPrototypeMethod::Format
            | IntlDateTimeFormatPrototypeMethod::ResolvedOptions
    ) {
        return begin_intl_date_time_format_unwrap(
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
        return intl_date_time_format_brand_error(realm, origin);
    };
    if runtime.intl_date_time_format_state(*formatter)?.is_none() {
        return intl_date_time_format_brand_error(realm, origin);
    }
    match method {
        IntlDateTimeFormatPrototypeMethod::Format
        | IntlDateTimeFormatPrototypeMethod::ResolvedOptions => {
            Err(EngineFault::RuntimeInvariant {
                message: "unwrap-capable Intl.DateTimeFormat method bypassed UnwrapDateTimeFormat",
            }
            .into())
        }
        IntlDateTimeFormatPrototypeMethod::FormatToParts => {
            let value = arguments.take_first_or_undefined();
            begin_intl_date_time_format_value(
                runtime,
                IntlDateTimeFormatValueContinuation {
                    formatter: *formatter,
                    operation: IntlDateTimeFormatOperation::FormatToParts,
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
        IntlDateTimeFormatPrototypeMethod::FormatRange
        | IntlDateTimeFormatPrototypeMethod::FormatRangeToParts => {
            let first = arguments.take_first_or_undefined();
            let second = arguments.take_first_or_undefined();
            if matches!(first, StoredValue::Undefined) || matches!(second, StoredValue::Undefined) {
                return intl_locale_list_error(
                    realm,
                    origin,
                    ExceptionKind::TypeError,
                    "Intl.DateTimeFormat range arguments must not be undefined",
                );
            }
            begin_intl_date_time_format_value(
                runtime,
                IntlDateTimeFormatValueContinuation {
                    formatter: *formatter,
                    operation: if method == IntlDateTimeFormatPrototypeMethod::FormatRange {
                        IntlDateTimeFormatOperation::FormatRange
                    } else {
                        IntlDateTimeFormatOperation::FormatRangeToParts
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

fn begin_intl_date_time_format_unwrap(
    runtime: &mut Runtime,
    method: IntlDateTimeFormatPrototypeMethod,
    receiver: &StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(formatter) = receiver
        && runtime.intl_date_time_format_state(*formatter)?.is_some()
    {
        return finish_intl_date_time_format_unwrap(runtime, method, *formatter, realm, origin);
    }
    if !matches!(receiver, StoredValue::Function(_) | StoredValue::Object(_)) {
        return intl_date_time_format_brand_error(realm, origin);
    }
    let state = IntlDateTimeFormatUnwrapContinuation {
        receiver: receiver.duplicate(),
        method,
        realm,
        stage: IntlDateTimeFormatUnwrapStage::AwaitInstance,
        origin: origin.clone(),
    };
    let constructor = runtime.realm_intl_date_time_format_constructor(realm)?;
    let dispatch = begin_function_has_instance(
        runtime,
        realm,
        receiver.duplicate(),
        StoredValue::Function(constructor),
        return_to,
        origin,
        execution_budget,
    )?;
    continue_intl_date_time_format_unwrap_after(
        dispatch,
        state,
        runtime,
        return_to,
        execution_budget,
    )
}

pub(super) fn advance_intl_date_time_format_unwrap(
    runtime: &mut Runtime,
    mut state: IntlDateTimeFormatUnwrapContinuation,
    completion: &StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IntlDateTimeFormatUnwrapStage::AwaitInstance => {
            if !completion.is_truthy() {
                return intl_date_time_format_brand_error(state.realm, state.origin);
            }
            let symbol = runtime.intl_number_format_fallback_symbol();
            let key = runtime.property_key_from_symbol(&symbol)?;
            charge_heap_property_lookup(runtime, &state.receiver, execution_budget)?;
            state.stage = IntlDateTimeFormatUnwrapStage::AwaitFallback;
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
            continue_intl_date_time_format_unwrap_after(
                dispatch,
                state,
                runtime,
                return_to,
                execution_budget,
            )
        }
        IntlDateTimeFormatUnwrapStage::AwaitFallback => {
            let StoredValue::Object(formatter) = completion else {
                return intl_date_time_format_brand_error(state.realm, state.origin);
            };
            if runtime.intl_date_time_format_state(*formatter)?.is_none() {
                return intl_date_time_format_brand_error(state.realm, state.origin);
            }
            finish_intl_date_time_format_unwrap(
                runtime,
                state.method,
                *formatter,
                state.realm,
                state.origin,
            )
        }
    }
}

fn finish_intl_date_time_format_unwrap(
    runtime: &mut Runtime,
    method: IntlDateTimeFormatPrototypeMethod,
    formatter: ObjectId,
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        IntlDateTimeFormatPrototypeMethod::Format => {
            let function = match runtime.intl_date_time_format_bound_format(formatter)? {
                Some(function) => function,
                None => runtime.allocate_intl_date_time_format_bound_format(realm, formatter)?,
            };
            Ok(NativeDispatch::Immediate(StoredValue::Function(function)))
        }
        IntlDateTimeFormatPrototypeMethod::ResolvedOptions => {
            let state = runtime
                .intl_date_time_format_state(formatter)?
                .cloned()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "unwrapped Intl.DateTimeFormat lost its internal slots",
                })?;
            intl_date_time_format_resolved_options(runtime, realm, &state)
        }
        IntlDateTimeFormatPrototypeMethod::FormatToParts
        | IntlDateTimeFormatPrototypeMethod::FormatRange
        | IntlDateTimeFormatPrototypeMethod::FormatRangeToParts => {
            intl_date_time_format_brand_error(realm, origin)
        }
    }
}

fn continue_intl_date_time_format_unwrap_after(
    dispatch: NativeDispatch,
    state: IntlDateTimeFormatUnwrapContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match dispatch {
        NativeDispatch::Immediate(value) => advance_intl_date_time_format_unwrap(
            runtime,
            state,
            &value,
            return_to,
            execution_budget,
        ),
        NativeDispatch::Call(mut call) => {
            prepend_native_continuations(
                &mut call,
                vec![NativeContinuation::IntlDateTimeFormatUnwrap(Box::new(
                    state,
                ))],
            )?;
            Ok(NativeDispatch::Call(call))
        }
        NativeDispatch::Frame(mut frame) => {
            attach_native_continuations(
                &mut frame,
                vec![NativeContinuation::IntlDateTimeFormatUnwrap(Box::new(
                    state,
                ))],
            )?;
            Ok(NativeDispatch::Frame(frame))
        }
        NativeDispatch::Pair(_, _)
        | NativeDispatch::ForOfRecord { .. }
        | NativeDispatch::ForOfStep { .. }
        | NativeDispatch::ForOfClosed
        | NativeDispatch::CopyDataPropertiesDone
        | NativeDispatch::AsyncAwait { .. } => Err(EngineFault::RuntimeInvariant {
            message: "UnwrapDateTimeFormat produced a structured result",
        }
        .into()),
    }
}

pub(super) fn begin_intl_date_time_format_format(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(formatter) = receiver else {
        return intl_date_time_format_brand_error(realm, origin);
    };
    if runtime.intl_date_time_format_state(*formatter)?.is_none() {
        return intl_date_time_format_brand_error(realm, origin);
    }
    let value = arguments.take_first_or_undefined();
    begin_intl_date_time_format_value(
        runtime,
        IntlDateTimeFormatValueContinuation {
            formatter: *formatter,
            operation: IntlDateTimeFormatOperation::Format,
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

#[allow(
    clippy::too_many_lines,
    reason = "resolvedOptions property order mirrors the ECMA-402 DateTimeFormat algorithm"
)]
fn intl_date_time_format_resolved_options(
    runtime: &mut Runtime,
    realm: RealmId,
    state: &DateTimeFormatState,
) -> Result<NativeDispatch, NativeFailure> {
    let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    let mut properties = vec![
        (
            "locale",
            StoredValue::String(JsString::from_utf8(&state.locale)?),
        ),
        (
            "calendar",
            StoredValue::String(JsString::from_utf8(&state.calendar)?),
        ),
        (
            "numberingSystem",
            StoredValue::String(JsString::from_utf8(&state.numbering_system)?),
        ),
        (
            "timeZone",
            StoredValue::String(JsString::from_utf8(&state.time_zone)?),
        ),
    ];
    if state.has_hour() {
        properties.push((
            "hourCycle",
            StoredValue::String(JsString::from_utf8(state.hour_cycle.as_str())?),
        ));
        properties.push(("hour12", StoredValue::Boolean(state.hour12())));
    }
    for (name, style) in [
        ("weekday", state.weekday),
        ("era", state.era),
        ("year", state.year),
        ("month", state.month),
        ("day", state.day),
        ("dayPeriod", state.day_period),
        ("hour", state.hour),
        ("minute", state.minute),
        ("second", state.second),
    ] {
        if let Some(style) = style {
            properties.push((
                name,
                StoredValue::String(JsString::from_utf8(style.as_str())?),
            ));
        }
    }
    if let Some(digits) = state.fractional_second_digits {
        properties.push((
            "fractionalSecondDigits",
            StoredValue::Number(JsNumber::from_i32(i32::from(digits))),
        ));
    }
    if let Some(style) = state.time_zone_name {
        properties.push((
            "timeZoneName",
            StoredValue::String(JsString::from_utf8(style.as_str())?),
        ));
    }
    if let Some(style) = state.date_style {
        properties.push((
            "dateStyle",
            StoredValue::String(JsString::from_utf8(style.as_str())?),
        ));
    }
    if let Some(style) = state.time_style {
        properties.push((
            "timeStyle",
            StoredValue::String(JsString::from_utf8(style.as_str())?),
        ));
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
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn intl_date_time_format_brand_error<T>(
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        realm,
        origin,
        ExceptionKind::TypeError,
        "Intl.DateTimeFormat method called on incompatible receiver",
    )
}

fn begin_intl_date_time_format_value(
    runtime: &mut Runtime,
    state: IntlDateTimeFormatValueContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(object) = value {
        if let Some(value) = resolve_temporal_date_time_input(runtime, object, &state)? {
            return finish_intl_date_time_format_value_resolved(
                runtime,
                state,
                value,
                return_to,
                execution_budget,
            );
        }
        let realm = state.realm;
        let origin = state.origin.clone();
        return begin_operator_primitive_conversion(
            runtime,
            StoredValue::Object(object),
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::IntlDateTimeFormatValue(Box::new(state)),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if let StoredValue::Function(function) = value {
        let realm = state.realm;
        let origin = state.origin.clone();
        return begin_operator_primitive_conversion(
            runtime,
            StoredValue::Function(function),
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::IntlDateTimeFormatValue(Box::new(state)),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    if matches!(value, StoredValue::Undefined) {
        let instant = temporal_rs::Temporal::utc_now().instant().map_err(|_| {
            EngineFault::RuntimeInvariant {
                message: "the host clock did not produce a Temporal instant",
            }
        })?;
        let resolved = resolve_epoch_date_time_input(runtime, state.formatter, instant)?;
        return finish_intl_date_time_format_value_resolved(
            runtime,
            state,
            resolved,
            return_to,
            execution_budget,
        );
    }
    finish_intl_date_time_format_value_primitive(runtime, state, value, return_to, execution_budget)
}

pub(super) fn finish_intl_date_time_format_value_primitive(
    runtime: &mut Runtime,
    state: IntlDateTimeFormatValueContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let number = operator_to_number(value, state.realm, &state.origin)?.as_f64();
    let clipped = time_clip(number).as_f64();
    if !clipped.is_finite() {
        if matches!(
            state.operation,
            IntlDateTimeFormatOperation::FormatRange
                | IntlDateTimeFormatOperation::FormatRangeToParts
        ) {
            return finish_intl_date_time_format_value_resolved(
                runtime,
                state,
                invalid_resolved_date_time_input(IntlDateTimeInputIdentity::Number),
                return_to,
                execution_budget,
            );
        }
        return intl_locale_list_error(
            state.realm,
            state.origin,
            ExceptionKind::RangeError,
            "Intl.DateTimeFormat value is not a valid time",
        );
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "TimeClip bounds the integral millisecond value well inside i64"
    )]
    let milliseconds = clipped as i64;
    let instant = temporal_rs::Instant::from_epoch_milliseconds(milliseconds).map_err(|_| {
        EngineFault::RuntimeInvariant {
            message: "TimeClip produced an invalid Temporal instant",
        }
    })?;
    let resolved = resolve_epoch_date_time_input(runtime, state.formatter, instant)?;
    finish_intl_date_time_format_value_resolved(
        runtime,
        state,
        resolved,
        return_to,
        execution_budget,
    )
}

fn finish_intl_date_time_format_value_resolved(
    runtime: &mut Runtime,
    mut state: IntlDateTimeFormatValueContinuation,
    value: ResolvedDateTimeInput,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(
        state.operation,
        IntlDateTimeFormatOperation::FormatRange | IntlDateTimeFormatOperation::FormatRangeToParts
    ) && state.first.is_none()
    {
        state.first = Some(value);
        let second = state.second.take().ok_or(EngineFault::RuntimeInvariant {
            message: "Intl.DateTimeFormat range conversion lost its second operand",
        })?;
        return begin_intl_date_time_format_value(
            runtime,
            state,
            second,
            return_to,
            execution_budget,
        );
    }
    finish_intl_date_time_format_operation(runtime, state, &value)
}

fn resolve_temporal_date_time_input(
    runtime: &Runtime,
    object: ObjectId,
    state: &IntlDateTimeFormatValueContinuation,
) -> Result<Option<ResolvedDateTimeInput>, NativeFailure> {
    if runtime.temporal_zoned_date_time(object)?.is_some() {
        if matches!(
            state.operation,
            IntlDateTimeFormatOperation::FormatRange
                | IntlDateTimeFormatOperation::FormatRangeToParts
        ) {
            return Ok(Some(invalid_resolved_date_time_input(
                IntlDateTimeInputIdentity::ZonedDateTime,
            )));
        }
        return intl_locale_list_error(
            state.realm,
            state.origin.clone(),
            ExceptionKind::TypeError,
            "Intl.DateTimeFormat does not accept Temporal.ZonedDateTime",
        );
    }
    if let Some(instant) = runtime.temporal_instant(object)? {
        let mut resolved = resolve_epoch_date_time_input(runtime, state.formatter, instant)?;
        resolved.identity = IntlDateTimeInputIdentity::Instant;
        resolved.value.kind = DateTimeFormatInputKind::Instant;
        return Ok(Some(resolved));
    }
    if let Some(date_time) = runtime.temporal_plain_date_time(object)? {
        let calendar = date_time.calendar().identifier().to_owned();
        return Ok(Some(resolve_plain_date_time_input(
            &date_time,
            DateTimeFormatInputKind::PlainDateTime,
            IntlDateTimeInputIdentity::PlainDateTime,
            Some(calendar),
        )));
    }
    if let Some(date) = runtime.temporal_plain_date(object)? {
        let calendar = date.calendar().identifier().to_owned();
        let instant = temporal_rs::Instant::from(date.epoch_ns_for_utc());
        return Ok(Some(resolve_anchored_plain_date_time_input(
            instant,
            DateTimeFormatInputKind::PlainDate,
            IntlDateTimeInputIdentity::PlainDate,
            Some(calendar),
        )?));
    }
    if let Some(year_month) = runtime.temporal_plain_year_month(object)? {
        let calendar = year_month.calendar_id().to_owned();
        let instant = temporal_rs::Instant::from(year_month.epoch_ns_for_utc());
        return Ok(Some(resolve_anchored_plain_date_time_input(
            instant,
            DateTimeFormatInputKind::PlainYearMonth,
            IntlDateTimeInputIdentity::PlainYearMonth,
            Some(calendar),
        )?));
    }
    if let Some(month_day) = runtime.temporal_plain_month_day(object)? {
        let calendar = month_day.calendar_id().to_owned();
        let instant = temporal_rs::Instant::from(month_day.epoch_ns_for_utc());
        return Ok(Some(resolve_anchored_plain_date_time_input(
            instant,
            DateTimeFormatInputKind::PlainMonthDay,
            IntlDateTimeInputIdentity::PlainMonthDay,
            Some(calendar),
        )?));
    }
    if let Some(time) = runtime.temporal_plain_time(object)? {
        let nanosecond = u32::from(time.millisecond()) * 1_000_000
            + u32::from(time.microsecond()) * 1_000
            + u32::from(time.nanosecond());
        return Ok(Some(ResolvedDateTimeInput {
            value: DateTimeFormatInput {
                kind: DateTimeFormatInputKind::PlainTime,
                year: 1970,
                month: 1,
                day: 1,
                hour: time.hour(),
                minute: time.minute(),
                second: time.second(),
                nanosecond,
                offset_seconds: 0,
                epoch_seconds: i64::from(time.hour()) * 3_600
                    + i64::from(time.minute()) * 60
                    + i64::from(time.second()),
            },
            identity: IntlDateTimeInputIdentity::PlainTime,
            calendar: None,
            valid: true,
        }));
    }
    Ok(None)
}

fn invalid_resolved_date_time_input(identity: IntlDateTimeInputIdentity) -> ResolvedDateTimeInput {
    ResolvedDateTimeInput {
        value: DateTimeFormatInput {
            kind: DateTimeFormatInputKind::Epoch,
            year: 1970,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            nanosecond: 0,
            offset_seconds: 0,
            epoch_seconds: 0,
        },
        identity,
        calendar: None,
        valid: false,
    }
}

fn resolve_epoch_date_time_input(
    runtime: &Runtime,
    formatter: ObjectId,
    instant: temporal_rs::Instant,
) -> Result<ResolvedDateTimeInput, NativeFailure> {
    let time_zone = runtime
        .intl_date_time_format_state(formatter)?
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Intl.DateTimeFormat operation lost its branded receiver",
        })?
        .time_zone
        .clone();
    project_epoch_date_time_input(
        &time_zone,
        instant,
        DateTimeFormatInputKind::Epoch,
        IntlDateTimeInputIdentity::Number,
    )
}

fn project_epoch_date_time_input(
    time_zone: &str,
    instant: temporal_rs::Instant,
    kind: DateTimeFormatInputKind,
    identity: IntlDateTimeInputIdentity,
) -> Result<ResolvedDateTimeInput, NativeFailure> {
    let time_zone = temporal_rs::TimeZone::try_from_identifier_str(time_zone).map_err(|_| {
        EngineFault::RuntimeInvariant {
            message: "resolved Intl.DateTimeFormat time zone was rejected by Temporal",
        }
    })?;
    let zoned =
        instant
            .to_zoned_date_time_iso(time_zone)
            .map_err(|_| EngineFault::RuntimeInvariant {
                message: "Temporal failed to project a valid Intl.DateTimeFormat instant",
            })?;
    let plain = zoned.to_plain_date_time();
    let epoch_seconds = instant.as_i128().div_euclid(1_000_000_000);
    let epoch_seconds =
        i64::try_from(epoch_seconds).map_err(|_| EngineFault::RuntimeInvariant {
            message: "a valid Temporal instant exceeded i64 epoch seconds",
        })?;
    let offset_seconds = zoned.offset_nanoseconds().div_euclid(1_000_000_000);
    let offset_seconds =
        i32::try_from(offset_seconds).map_err(|_| EngineFault::RuntimeInvariant {
            message: "a valid Temporal time-zone offset exceeded i32 seconds",
        })?;
    Ok(ResolvedDateTimeInput {
        value: DateTimeFormatInput {
            kind,
            year: plain.iso_year(),
            month: plain.iso_month(),
            day: plain.iso_day(),
            hour: plain.hour(),
            minute: plain.minute(),
            second: plain.second(),
            nanosecond: date_time_nanosecond(
                plain.millisecond(),
                plain.microsecond(),
                plain.nanosecond(),
            ),
            offset_seconds,
            epoch_seconds,
        },
        identity,
        calendar: None,
        valid: true,
    })
}

fn resolve_plain_date_time_input(
    date_time: &temporal_rs::PlainDateTime,
    kind: DateTimeFormatInputKind,
    identity: IntlDateTimeInputIdentity,
    calendar: Option<String>,
) -> ResolvedDateTimeInput {
    let instant = temporal_rs::Instant::from(date_time.epoch_ns_for_utc());
    let epoch_seconds = i64::try_from(instant.as_i128().div_euclid(1_000_000_000))
        .expect("a valid plain date-time is inside Temporal's i64 epoch-second range");
    ResolvedDateTimeInput {
        value: DateTimeFormatInput {
            kind,
            year: date_time.iso_year(),
            month: date_time.iso_month(),
            day: date_time.iso_day(),
            hour: date_time.hour(),
            minute: date_time.minute(),
            second: date_time.second(),
            nanosecond: date_time_nanosecond(
                date_time.millisecond(),
                date_time.microsecond(),
                date_time.nanosecond(),
            ),
            offset_seconds: 0,
            epoch_seconds,
        },
        identity,
        calendar,
        valid: true,
    }
}

fn resolve_anchored_plain_date_time_input(
    instant: temporal_rs::Instant,
    kind: DateTimeFormatInputKind,
    identity: IntlDateTimeInputIdentity,
    calendar: Option<String>,
) -> Result<ResolvedDateTimeInput, EngineFault> {
    const NANOSECONDS_PER_DAY: i128 = 86_400_000_000_000;
    let epoch_nanoseconds = instant.as_i128();
    let epoch_days = epoch_nanoseconds.div_euclid(NANOSECONDS_PER_DAY);
    let epoch_days = i64::try_from(epoch_days).map_err(|_| EngineFault::RuntimeInvariant {
        message: "a Temporal plain calendar anchor exceeded i64 epoch days",
    })?;
    let (year, month, day) = civil_date_from_epoch_days(epoch_days);
    Ok(ResolvedDateTimeInput {
        value: DateTimeFormatInput {
            kind,
            year,
            month,
            day,
            hour: 0,
            minute: 0,
            second: 0,
            nanosecond: 0,
            offset_seconds: 0,
            epoch_seconds: i64::try_from(epoch_nanoseconds.div_euclid(1_000_000_000)).map_err(
                |_| EngineFault::RuntimeInvariant {
                    message: "a Temporal plain calendar anchor exceeded i64 epoch seconds",
                },
            )?,
        },
        identity,
        calendar,
        valid: true,
    })
}

fn civil_date_from_epoch_days(epoch_days: i64) -> (i32, u8, u8) {
    // Proleptic-Gregorian inverse of days-from-civil. The 400-year era
    // decomposition is exact across Temporal's full plain-date range.
    let shifted = epoch_days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (
        i32::try_from(year).expect("Temporal years fit i32"),
        u8::try_from(month).expect("civil month is in range"),
        u8::try_from(day).expect("civil day is in range"),
    )
}

fn date_time_nanosecond(millisecond: u16, microsecond: u16, nanosecond: u16) -> u32 {
    u32::from(millisecond) * 1_000_000 + u32::from(microsecond) * 1_000 + u32::from(nanosecond)
}

fn finish_intl_date_time_format_operation(
    runtime: &mut Runtime,
    state: IntlDateTimeFormatValueContinuation,
    value: &ResolvedDateTimeInput,
) -> Result<NativeDispatch, NativeFailure> {
    let resolved = runtime
        .intl_date_time_format_state(state.formatter)?
        .cloned()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Intl.DateTimeFormat operation lost its branded receiver",
        })?;
    match state.operation {
        IntlDateTimeFormatOperation::Format => {
            let formatted = format_datetime(&resolved, &value.value)
                .map_err(|error| intl_date_time_format_value_failure(&state, error))?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&formatted)?,
            )))
        }
        IntlDateTimeFormatOperation::FormatToParts => {
            let parts = format_datetime_to_parts(&resolved, &value.value)
                .map_err(|error| intl_date_time_format_value_failure(&state, error))?;
            intl_date_time_format_parts_array(runtime, state.realm, parts, None)
        }
        IntlDateTimeFormatOperation::FormatRange
        | IntlDateTimeFormatOperation::FormatRangeToParts => {
            let first = state.first.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "Intl.DateTimeFormat range operation lost its first operand",
            })?;
            if matches!(first.identity, IntlDateTimeInputIdentity::ZonedDateTime)
                || matches!(value.identity, IntlDateTimeInputIdentity::ZonedDateTime)
            {
                return intl_locale_list_error(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "Intl.DateTimeFormat does not accept Temporal.ZonedDateTime",
                );
            }
            if first.identity != value.identity {
                return intl_locale_list_error(
                    state.realm,
                    state.origin,
                    ExceptionKind::TypeError,
                    "Intl.DateTimeFormat range arguments have different types",
                );
            }
            if first.calendar != value.calendar {
                return intl_locale_list_error(
                    state.realm,
                    state.origin,
                    ExceptionKind::RangeError,
                    "Intl.DateTimeFormat range arguments have different calendars",
                );
            }
            if !first.valid || !value.valid {
                return intl_locale_list_error(
                    state.realm,
                    state.origin,
                    ExceptionKind::RangeError,
                    "Intl.DateTimeFormat range value is not a valid time",
                );
            }
            let parts = intl_date_time_format_range_parts(&resolved, first, value, &state)?;
            if state.operation == IntlDateTimeFormatOperation::FormatRange {
                let formatted = parts
                    .iter()
                    .map(|part| part.part.value.as_str())
                    .collect::<String>();
                Ok(NativeDispatch::Immediate(StoredValue::String(
                    JsString::from_utf8(&formatted)?,
                )))
            } else {
                intl_date_time_format_sourced_parts_array(runtime, state.realm, parts)
            }
        }
    }
}

fn intl_date_time_format_value_failure(
    state: &IntlDateTimeFormatValueContinuation,
    error: DateTimeFormatError,
) -> NativeFailure {
    match error {
        DateTimeFormatError::NoFields => NativeFailure::Abrupt(PendingException {
            realm: state.realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8(
                    "Intl.DateTimeFormat has no fields for this Temporal value",
                )
                .expect("static Intl error is valid UTF-8"),
            },
            origin: state.origin.clone(),
        }),
        DateTimeFormatError::InvalidDateTime => NativeFailure::Abrupt(PendingException {
            realm: state.realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("Intl.DateTimeFormat value is outside its range")
                    .expect("static Intl error is valid UTF-8"),
            },
            origin: state.origin.clone(),
        }),
        DateTimeFormatError::InvalidLocale
        | DateTimeFormatError::InvalidOption
        | DateTimeFormatError::InvalidTimeZone
        | DateTimeFormatError::Data => EngineFault::RuntimeInvariant {
            message: "resolved Intl.DateTimeFormat slots failed locale formatting",
        }
        .into(),
    }
}

struct SourcedDateTimeFormatPart {
    part: quickjs_intl::DateTimeFormatPart,
    source: &'static str,
}

fn intl_date_time_format_range_parts(
    resolved: &DateTimeFormatState,
    first: &ResolvedDateTimeInput,
    second: &ResolvedDateTimeInput,
    continuation: &IntlDateTimeFormatValueContinuation,
) -> Result<Vec<SourcedDateTimeFormatPart>, NativeFailure> {
    let first_parts = format_datetime_to_parts(resolved, &first.value)
        .map_err(|error| intl_date_time_format_value_failure(continuation, error))?;
    let second_parts = format_datetime_to_parts(resolved, &second.value)
        .map_err(|error| intl_date_time_format_value_failure(continuation, error))?;
    let first_text = first_parts
        .iter()
        .map(|part| part.value.as_str())
        .collect::<String>();
    let second_text = second_parts
        .iter()
        .map(|part| part.value.as_str())
        .collect::<String>();
    if first_text == second_text {
        return Ok(first_parts
            .into_iter()
            .map(|part| SourcedDateTimeFormatPart {
                part,
                source: "shared",
            })
            .collect());
    }

    let english = resolved.locale == "en" || resolved.locale.starts_with("en-");
    if english
        && matches!(
            resolved.month,
            Some(
                DateTimeComponentStyle::Narrow
                    | DateTimeComponentStyle::Short
                    | DateTimeComponentStyle::Long
            )
        )
        && first.value.year == second.value.year
    {
        let prefix = common_date_time_part_prefix(&first_parts, &second_parts);
        let suffix = common_date_time_part_suffix(&first_parts, &second_parts, prefix);
        return Ok(collapse_date_time_range_parts(
            &first_parts,
            &second_parts,
            prefix,
            suffix,
        ));
    }
    if resolved.default_components
        && first.value.kind == DateTimeFormatInputKind::PlainDateTime
        && second.value.kind == DateTimeFormatInputKind::PlainDateTime
        && (first.value.year, first.value.month, first.value.day)
            == (second.value.year, second.value.month, second.value.day)
    {
        let prefix = common_date_time_part_prefix(&first_parts, &second_parts);
        return Ok(collapse_date_time_range_parts(
            &first_parts,
            &second_parts,
            prefix,
            0,
        ));
    }

    let mut result = Vec::new();
    result.extend(
        first_parts
            .into_iter()
            .map(|part| SourcedDateTimeFormatPart {
                part,
                source: "startRange",
            }),
    );
    result.push(SourcedDateTimeFormatPart {
        part: quickjs_intl::DateTimeFormatPart {
            kind: "literal",
            value: "\u{2009}–\u{2009}".to_owned(),
        },
        source: "shared",
    });
    result.extend(
        second_parts
            .into_iter()
            .map(|part| SourcedDateTimeFormatPart {
                part,
                source: "endRange",
            }),
    );
    Ok(result)
}

fn common_date_time_part_prefix(
    first: &[quickjs_intl::DateTimeFormatPart],
    second: &[quickjs_intl::DateTimeFormatPart],
) -> usize {
    first
        .iter()
        .zip(second)
        .take_while(|(first, second)| first == second)
        .count()
}

fn common_date_time_part_suffix(
    first: &[quickjs_intl::DateTimeFormatPart],
    second: &[quickjs_intl::DateTimeFormatPart],
    prefix: usize,
) -> usize {
    first
        .iter()
        .rev()
        .zip(second.iter().rev())
        .take(first.len().min(second.len()).saturating_sub(prefix))
        .take_while(|(first, second)| first == second)
        .count()
}

fn collapse_date_time_range_parts(
    first: &[quickjs_intl::DateTimeFormatPart],
    second: &[quickjs_intl::DateTimeFormatPart],
    prefix: usize,
    suffix: usize,
) -> Vec<SourcedDateTimeFormatPart> {
    let mut result = Vec::new();
    result.extend(
        first[..prefix]
            .iter()
            .cloned()
            .map(|part| SourcedDateTimeFormatPart {
                part,
                source: "shared",
            }),
    );
    result.extend(
        first[prefix..first.len() - suffix]
            .iter()
            .cloned()
            .map(|part| SourcedDateTimeFormatPart {
                part,
                source: "startRange",
            }),
    );
    result.push(SourcedDateTimeFormatPart {
        part: quickjs_intl::DateTimeFormatPart {
            kind: "literal",
            value: "\u{2009}–\u{2009}".to_owned(),
        },
        source: "shared",
    });
    result.extend(
        second[prefix..second.len() - suffix]
            .iter()
            .cloned()
            .map(|part| SourcedDateTimeFormatPart {
                part,
                source: "endRange",
            }),
    );
    result.extend(first[first.len() - suffix..].iter().cloned().map(|part| {
        SourcedDateTimeFormatPart {
            part,
            source: "shared",
        }
    }));
    result
}

fn intl_date_time_format_parts_array(
    runtime: &mut Runtime,
    realm: RealmId,
    parts: Vec<quickjs_intl::DateTimeFormatPart>,
    source: Option<&'static str>,
) -> Result<NativeDispatch, NativeFailure> {
    intl_date_time_format_part_entries_array(
        runtime,
        realm,
        parts.into_iter().map(|part| (part, source)).collect(),
    )
}

fn intl_date_time_format_sourced_parts_array(
    runtime: &mut Runtime,
    realm: RealmId,
    parts: Vec<SourcedDateTimeFormatPart>,
) -> Result<NativeDispatch, NativeFailure> {
    intl_date_time_format_part_entries_array(
        runtime,
        realm,
        parts
            .into_iter()
            .map(|part| (part.part, Some(part.source)))
            .collect(),
    )
}

fn intl_date_time_format_part_entries_array(
    runtime: &mut Runtime,
    realm: RealmId,
    parts: Vec<(quickjs_intl::DateTimeFormatPart, Option<&'static str>)>,
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
        IntlLocaleListTarget::DateTimeFormatConstructor(mut state) => {
            state.requested_locales = intl_locale_strings(locales)?;
            begin_intl_date_time_format_options(runtime, *state, return_to, execution_budget)
        }
        IntlLocaleListTarget::DateTimeFormatSupportedLocalesOf(mut state) => {
            state.requested_locales = intl_locale_strings(locales)?;
            begin_intl_date_time_format_supported_locales_options(
                runtime,
                *state,
                return_to,
                execution_budget,
            )
        }
        IntlLocaleListTarget::PluralRulesConstructor(mut state) => {
            state.requested_locales = intl_locale_strings(locales)?;
            begin_intl_plural_rules_options(runtime, *state, return_to, execution_budget)
        }
        IntlLocaleListTarget::PluralRulesSupportedLocalesOf(mut state) => {
            state.requested_locales = intl_locale_strings(locales)?;
            begin_intl_plural_rules_supported_locales_options(
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
