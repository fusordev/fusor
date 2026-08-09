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
    DateTimeStyle, DateTimeTimeZoneName, DisplayNamesError, DisplayNamesFallback,
    DisplayNamesLanguageDisplay, DisplayNamesRequestOptions, DisplayNamesState, DisplayNamesStyle,
    DisplayNamesType, DurationDisplay, DurationFormatError, DurationFormatRequestOptions,
    DurationFormatState, DurationFormatStyle, DurationRecord, DurationUnit, DurationUnitStyle,
    IntlMathematicalValue, ListFormatError, ListFormatRequestOptions, ListFormatState,
    ListFormatStyle, ListFormatType, LocaleComponents, LocaleOptionKind, LocaleOptions,
    LocaleWeekInfo, NumberFormatCompactDisplay, NumberFormatCurrencyDisplay,
    NumberFormatCurrencySign, NumberFormatError, NumberFormatNotation, NumberFormatRequestOptions,
    NumberFormatRoundingMode, NumberFormatRoundingPriority, NumberFormatSignDisplay,
    NumberFormatState, NumberFormatStyle, NumberFormatTrailingZeroDisplay, NumberFormatUnitDisplay,
    NumberFormatUseGrouping, PluralRuleType, PluralRulesRequestOptions, PluralRulesState,
    RelativeTimeFormatError, RelativeTimeFormatNumeric, RelativeTimeFormatRequestOptions,
    RelativeTimeFormatState, RelativeTimeFormatStyle, RelativeTimeUnit, SegmentBoundary,
    SegmenterError, SegmenterGranularity, SegmenterRequestOptions, SegmenterState,
    apply_locale_options, calendars_of_locale, canonicalize_locale, canonicalize_locale_option,
    canonicalize_time_zone, collations_of_locale, collator_supported_locales,
    compare_with_collator, date_time_format_supported_locales, display_name,
    display_names_supported_locales, duration_format_supported_locales, format_datetime,
    format_datetime_to_parts, format_duration, format_duration_to_parts, format_list,
    format_list_to_parts, format_number, format_number_to_parts, format_relative_time,
    format_relative_time_to_parts, hour_cycles_of_locale, intl_mathematical_value_from_f64,
    is_well_formed_currency_code, is_well_formed_unit_identifier, list_format_supported_locales,
    locale_components, maximize_locale, minimize_locale, number_format_supported_locales,
    numbering_systems_of_locale, parse_intl_mathematical_value, plural_rules_supported_locales,
    relative_time_format_supported_locales, resolve_collator, resolve_date_time_format,
    resolve_display_names, resolve_duration_format, resolve_list_format, resolve_number_format,
    resolve_plural_rules, resolve_relative_time_format, resolve_segmenter, segment_boundaries,
    segmenter_supported_locales, select_plural, select_plural_range, supported_values,
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
    StringCase(Box<IntlStringCaseContinuation>),
    CollatorConstructor(Box<IntlCollatorConstructorContinuation>),
    CollatorSupportedLocalesOf(Box<IntlCollatorSupportedLocalesContinuation>),
    NumberFormatConstructor(Box<IntlNumberFormatConstructorContinuation>),
    NumberFormatSupportedLocalesOf(Box<IntlNumberFormatSupportedLocalesContinuation>),
    DateTimeFormatConstructor(Box<IntlDateTimeFormatConstructorContinuation>),
    DateTimeFormatSupportedLocalesOf(Box<IntlDateTimeFormatSupportedLocalesContinuation>),
    PluralRulesConstructor(Box<IntlPluralRulesConstructorContinuation>),
    PluralRulesSupportedLocalesOf(Box<IntlPluralRulesSupportedLocalesContinuation>),
    RelativeTimeFormatConstructor(Box<IntlRelativeTimeFormatConstructorContinuation>),
    RelativeTimeFormatSupportedLocalesOf(Box<IntlRelativeTimeFormatSupportedLocalesContinuation>),
    ListFormatConstructor(Box<IntlListFormatConstructorContinuation>),
    ListFormatSupportedLocalesOf(Box<IntlListFormatSupportedLocalesContinuation>),
    DisplayNamesConstructor(Box<IntlDisplayNamesConstructorContinuation>),
    DisplayNamesSupportedLocalesOf(Box<IntlDisplayNamesSupportedLocalesContinuation>),
    DurationFormatConstructor(Box<IntlDurationFormatConstructorContinuation>),
    DurationFormatSupportedLocalesOf(Box<IntlDurationFormatSupportedLocalesContinuation>),
    SegmenterConstructor(Box<IntlSegmenterConstructorContinuation>),
    SegmenterSupportedLocalesOf(Box<IntlSegmenterSupportedLocalesContinuation>),
}

impl IntlLocaleListTarget {
    fn retained_values(&self) -> u64 {
        match self {
            Self::ReturnArray => 0,
            Self::StringCase(_) => IntlStringCaseContinuation::retained_values(),
            Self::CollatorConstructor(state) => state.retained_values(),
            Self::CollatorSupportedLocalesOf(state) => state.retained_values(),
            Self::NumberFormatConstructor(state) => state.retained_values(),
            Self::NumberFormatSupportedLocalesOf(state) => state.retained_values(),
            Self::DateTimeFormatConstructor(state) => state.retained_values(),
            Self::DateTimeFormatSupportedLocalesOf(state) => state.retained_values(),
            Self::PluralRulesConstructor(state) => state.retained_values(),
            Self::PluralRulesSupportedLocalesOf(state) => state.retained_values(),
            Self::RelativeTimeFormatConstructor(state) => state.retained_values(),
            Self::RelativeTimeFormatSupportedLocalesOf(state) => state.retained_values(),
            Self::ListFormatConstructor(state) => state.retained_values(),
            Self::ListFormatSupportedLocalesOf(state) => state.retained_values(),
            Self::DisplayNamesConstructor(state) => state.retained_values(),
            Self::DisplayNamesSupportedLocalesOf(state) => state.retained_values(),
            Self::DurationFormatConstructor(state) => state.retained_values(),
            Self::DurationFormatSupportedLocalesOf(state) => state.retained_values(),
            Self::SegmenterConstructor(state) => state.retained_values(),
            Self::SegmenterSupportedLocalesOf(state) => state.retained_values(),
        }
    }

    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::ReturnArray => {}
            Self::StringCase(_) => IntlStringCaseContinuation::trace_roots(mark),
            Self::CollatorConstructor(state) => state.trace_roots(mark),
            Self::CollatorSupportedLocalesOf(state) => state.trace_roots(mark),
            Self::NumberFormatConstructor(state) => state.trace_roots(mark),
            Self::NumberFormatSupportedLocalesOf(state) => state.trace_roots(mark),
            Self::DateTimeFormatConstructor(state) => state.trace_roots(mark),
            Self::DateTimeFormatSupportedLocalesOf(state) => state.trace_roots(mark),
            Self::PluralRulesConstructor(state) => state.trace_roots(mark),
            Self::PluralRulesSupportedLocalesOf(state) => state.trace_roots(mark),
            Self::RelativeTimeFormatConstructor(state) => state.trace_roots(mark),
            Self::RelativeTimeFormatSupportedLocalesOf(state) => state.trace_roots(mark),
            Self::ListFormatConstructor(state) => state.trace_roots(mark),
            Self::ListFormatSupportedLocalesOf(state) => state.trace_roots(mark),
            Self::DisplayNamesConstructor(state) => state.trace_roots(mark),
            Self::DisplayNamesSupportedLocalesOf(state) => state.trace_roots(mark),
            Self::DurationFormatConstructor(state) => state.trace_roots(mark),
            Self::DurationFormatSupportedLocalesOf(state) => state.trace_roots(mark),
            Self::SegmenterConstructor(state) => state.trace_roots(mark),
            Self::SegmenterSupportedLocalesOf(state) => state.trace_roots(mark),
        }
    }
}

struct IntlStringCaseContinuation {
    subject: JsString,
    uppercase: bool,
}

impl IntlStringCaseContinuation {
    const fn retained_values() -> u64 {
        1
    }

    fn trace_roots(_mark: &mut dyn FnMut(CollectionRoot)) {}
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

enum IntlCollatorTarget {
    Constructor { new_target: FunctionId },
    LocaleCompare { first: JsString, second: JsString },
}

impl IntlCollatorTarget {
    const fn retained_values(&self) -> u64 {
        match self {
            Self::Constructor { .. } => 1,
            Self::LocaleCompare { .. } => 2,
        }
    }

    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        if let Self::Constructor { new_target } = self {
            mark(CollectionRoot::Heap(HeapReference::Function(*new_target)));
        }
    }
}

pub(super) struct IntlCollatorConstructorContinuation {
    target: IntlCollatorTarget,
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
        1_u64
            .saturating_add(self.target.retained_values())
            .saturating_add(u64::from(self.options_object.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        self.target.trace_roots(mark);
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
enum IntlRelativeTimeFormatConstructorStage {
    ReadOption,
    AwaitOption,
    AwaitOptionPrimitive,
    AwaitPrototype,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlRelativeTimeFormatOption {
    LocaleMatcher,
    NumberingSystem,
    Style,
    Numeric,
}

impl IntlRelativeTimeFormatOption {
    const ALL: [Self; 4] = [
        Self::LocaleMatcher,
        Self::NumberingSystem,
        Self::Style,
        Self::Numeric,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::LocaleMatcher => "localeMatcher",
            Self::NumberingSystem => "numberingSystem",
            Self::Style => "style",
            Self::Numeric => "numeric",
        }
    }
}

pub(super) struct IntlRelativeTimeFormatConstructorContinuation {
    new_target: FunctionId,
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    options: RelativeTimeFormatRequestOptions,
    resolved: Option<RelativeTimeFormatState>,
    option_index: usize,
    realm: RealmId,
    stage: IntlRelativeTimeFormatConstructorStage,
    origin: JsStackFrame,
}

impl IntlRelativeTimeFormatConstructorContinuation {
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
enum IntlRelativeTimeFormatSupportedLocalesStage {
    ReadLocaleMatcher,
    AwaitLocaleMatcher,
    AwaitLocaleMatcherPrimitive,
}

pub(super) struct IntlRelativeTimeFormatSupportedLocalesContinuation {
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    realm: RealmId,
    stage: IntlRelativeTimeFormatSupportedLocalesStage,
    origin: JsStackFrame,
}

impl IntlRelativeTimeFormatSupportedLocalesContinuation {
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
enum IntlRelativeTimeFormatOperation {
    Format,
    FormatToParts,
}

pub(super) struct IntlRelativeTimeFormatValueContinuation {
    formatter: ObjectId,
    operation: IntlRelativeTimeFormatOperation,
    unit: Option<StoredValue>,
    value: Option<f64>,
    realm: RealmId,
    origin: JsStackFrame,
}

impl IntlRelativeTimeFormatValueContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(u64::from(self.unit.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.formatter)));
        if let Some(unit) = &self.unit {
            trace_stored_value_root(unit, mark);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlListFormatConstructorStage {
    ReadOption,
    AwaitOption,
    AwaitOptionPrimitive,
    AwaitPrototype,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlListFormatOption {
    LocaleMatcher,
    Type,
    Style,
}

impl IntlListFormatOption {
    const ALL: [Self; 3] = [Self::LocaleMatcher, Self::Type, Self::Style];

    const fn name(self) -> &'static str {
        match self {
            Self::LocaleMatcher => "localeMatcher",
            Self::Type => "type",
            Self::Style => "style",
        }
    }
}

pub(super) struct IntlListFormatConstructorContinuation {
    new_target: FunctionId,
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    options: ListFormatRequestOptions,
    resolved: Option<ListFormatState>,
    option_index: usize,
    realm: RealmId,
    stage: IntlListFormatConstructorStage,
    origin: JsStackFrame,
}

impl IntlListFormatConstructorContinuation {
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
enum IntlListFormatSupportedLocalesStage {
    ReadLocaleMatcher,
    AwaitLocaleMatcher,
    AwaitLocaleMatcherPrimitive,
}

pub(super) struct IntlListFormatSupportedLocalesContinuation {
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    realm: RealmId,
    stage: IntlListFormatSupportedLocalesStage,
    origin: JsStackFrame,
}

impl IntlListFormatSupportedLocalesContinuation {
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
enum IntlDisplayNamesConstructorStage {
    AwaitPrototype,
    ReadOption,
    AwaitOption,
    AwaitOptionPrimitive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlDisplayNamesOption {
    LocaleMatcher,
    Style,
    Type,
    Fallback,
    LanguageDisplay,
}

impl IntlDisplayNamesOption {
    const ALL: [Self; 5] = [
        Self::LocaleMatcher,
        Self::Style,
        Self::Type,
        Self::Fallback,
        Self::LanguageDisplay,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::LocaleMatcher => "localeMatcher",
            Self::Style => "style",
            Self::Type => "type",
            Self::Fallback => "fallback",
            Self::LanguageDisplay => "languageDisplay",
        }
    }
}

pub(super) struct IntlDisplayNamesConstructorContinuation {
    new_target: FunctionId,
    locales_argument: Option<StoredValue>,
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    prototype: Option<HeapReference>,
    requested_locales: Vec<String>,
    options: DisplayNamesRequestOptions,
    option_index: usize,
    realm: RealmId,
    stage: IntlDisplayNamesConstructorStage,
    origin: JsStackFrame,
}

impl IntlDisplayNamesConstructorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.locales_argument.is_some()))
            .saturating_add(u64::from(self.options_object.is_some()))
            .saturating_add(u64::from(self.prototype.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
        if let Some(locales) = &self.locales_argument {
            trace_stored_value_root(locales, mark);
        }
        trace_stored_value_root(&self.options_argument, mark);
        if let Some(options) = &self.options_object {
            trace_stored_value_root(options, mark);
        }
        if let Some(prototype) = self.prototype {
            mark(CollectionRoot::Heap(prototype));
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlDisplayNamesSupportedLocalesStage {
    ReadLocaleMatcher,
    AwaitLocaleMatcher,
    AwaitLocaleMatcherPrimitive,
}

pub(super) struct IntlDisplayNamesSupportedLocalesContinuation {
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    realm: RealmId,
    stage: IntlDisplayNamesSupportedLocalesStage,
    origin: JsStackFrame,
}

impl IntlDisplayNamesSupportedLocalesContinuation {
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

pub(super) struct IntlDisplayNamesOfContinuation {
    display_names: ObjectId,
    realm: RealmId,
    origin: JsStackFrame,
}

impl IntlDisplayNamesOfContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(
            self.display_names,
        )));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlDurationFormatConstructorStage {
    AwaitPrototype,
    ReadOption,
    AwaitOption,
    AwaitOptionPrimitive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlDurationFormatOption {
    LocaleMatcher,
    NumberingSystem,
    Style,
    UnitStyle(DurationUnit),
    UnitDisplay(DurationUnit),
    FractionalDigits,
}

impl IntlDurationFormatOption {
    const ALL: [Self; 24] = [
        Self::LocaleMatcher,
        Self::NumberingSystem,
        Self::Style,
        Self::UnitStyle(DurationUnit::Years),
        Self::UnitDisplay(DurationUnit::Years),
        Self::UnitStyle(DurationUnit::Months),
        Self::UnitDisplay(DurationUnit::Months),
        Self::UnitStyle(DurationUnit::Weeks),
        Self::UnitDisplay(DurationUnit::Weeks),
        Self::UnitStyle(DurationUnit::Days),
        Self::UnitDisplay(DurationUnit::Days),
        Self::UnitStyle(DurationUnit::Hours),
        Self::UnitDisplay(DurationUnit::Hours),
        Self::UnitStyle(DurationUnit::Minutes),
        Self::UnitDisplay(DurationUnit::Minutes),
        Self::UnitStyle(DurationUnit::Seconds),
        Self::UnitDisplay(DurationUnit::Seconds),
        Self::UnitStyle(DurationUnit::Milliseconds),
        Self::UnitDisplay(DurationUnit::Milliseconds),
        Self::UnitStyle(DurationUnit::Microseconds),
        Self::UnitDisplay(DurationUnit::Microseconds),
        Self::UnitStyle(DurationUnit::Nanoseconds),
        Self::UnitDisplay(DurationUnit::Nanoseconds),
        Self::FractionalDigits,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::LocaleMatcher => "localeMatcher",
            Self::NumberingSystem => "numberingSystem",
            Self::Style => "style",
            Self::UnitStyle(unit) => unit.plural_name(),
            Self::UnitDisplay(unit) => unit.display_name(),
            Self::FractionalDigits => "fractionalDigits",
        }
    }

    const fn is_number(self) -> bool {
        matches!(self, Self::FractionalDigits)
    }
}

pub(super) struct IntlDurationFormatConstructorContinuation {
    new_target: FunctionId,
    format_value: Option<temporal_rs::Duration>,
    locales_argument: Option<StoredValue>,
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    prototype: Option<HeapReference>,
    requested_locales: Vec<String>,
    options: DurationFormatRequestOptions,
    option_index: usize,
    realm: RealmId,
    stage: IntlDurationFormatConstructorStage,
    origin: JsStackFrame,
}

impl IntlDurationFormatConstructorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.locales_argument.is_some()))
            .saturating_add(u64::from(self.options_object.is_some()))
            .saturating_add(u64::from(self.prototype.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
        if let Some(locales) = &self.locales_argument {
            trace_stored_value_root(locales, mark);
        }
        trace_stored_value_root(&self.options_argument, mark);
        if let Some(options) = &self.options_object {
            trace_stored_value_root(options, mark);
        }
        if let Some(prototype) = self.prototype {
            mark(CollectionRoot::Heap(prototype));
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlDurationFormatSupportedLocalesStage {
    ReadLocaleMatcher,
    AwaitLocaleMatcher,
    AwaitLocaleMatcherPrimitive,
}

pub(super) struct IntlDurationFormatSupportedLocalesContinuation {
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    realm: RealmId,
    stage: IntlDurationFormatSupportedLocalesStage,
    origin: JsStackFrame,
}

impl IntlDurationFormatSupportedLocalesContinuation {
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
enum IntlDurationFormatOperation {
    Format,
    FormatToParts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlDurationFormatValueStage {
    ReadProperty,
    AwaitProperty,
    AwaitPropertyPrimitive,
}

const INTL_DURATION_RECORD_PROPERTY_ORDER: [DurationUnit; 10] = [
    DurationUnit::Days,
    DurationUnit::Hours,
    DurationUnit::Microseconds,
    DurationUnit::Milliseconds,
    DurationUnit::Minutes,
    DurationUnit::Months,
    DurationUnit::Nanoseconds,
    DurationUnit::Seconds,
    DurationUnit::Weeks,
    DurationUnit::Years,
];

pub(super) struct IntlDurationFormatValueContinuation {
    formatter: ObjectId,
    input: StoredValue,
    values: [i128; 10],
    found: bool,
    unit_index: usize,
    operation: IntlDurationFormatOperation,
    realm: RealmId,
    stage: IntlDurationFormatValueStage,
    origin: JsStackFrame,
}

impl IntlDurationFormatValueContinuation {
    pub(super) const fn retained_values() -> u64 {
        2
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.formatter)));
        trace_stored_value_root(&self.input, mark);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlSegmenterConstructorStage {
    AwaitPrototype,
    ReadOption,
    AwaitOption,
    AwaitOptionPrimitive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlSegmenterOption {
    LocaleMatcher,
    Granularity,
}

impl IntlSegmenterOption {
    const ALL: [Self; 2] = [Self::LocaleMatcher, Self::Granularity];

    const fn name(self) -> &'static str {
        match self {
            Self::LocaleMatcher => "localeMatcher",
            Self::Granularity => "granularity",
        }
    }
}

pub(super) struct IntlSegmenterConstructorContinuation {
    new_target: FunctionId,
    locales_argument: Option<StoredValue>,
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    prototype: Option<HeapReference>,
    requested_locales: Vec<String>,
    options: SegmenterRequestOptions,
    option_index: usize,
    realm: RealmId,
    stage: IntlSegmenterConstructorStage,
    origin: JsStackFrame,
}

impl IntlSegmenterConstructorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.locales_argument.is_some()))
            .saturating_add(u64::from(self.options_object.is_some()))
            .saturating_add(u64::from(self.prototype.is_some()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
        if let Some(locales) = &self.locales_argument {
            trace_stored_value_root(locales, mark);
        }
        trace_stored_value_root(&self.options_argument, mark);
        if let Some(options) = &self.options_object {
            trace_stored_value_root(options, mark);
        }
        if let Some(prototype) = self.prototype {
            mark(CollectionRoot::Heap(prototype));
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlSegmenterSupportedLocalesStage {
    ReadLocaleMatcher,
    AwaitLocaleMatcher,
    AwaitLocaleMatcherPrimitive,
}

pub(super) struct IntlSegmenterSupportedLocalesContinuation {
    options_argument: StoredValue,
    options_object: Option<StoredValue>,
    requested_locales: Vec<String>,
    realm: RealmId,
    stage: IntlSegmenterSupportedLocalesStage,
    origin: JsStackFrame,
}

impl IntlSegmenterSupportedLocalesContinuation {
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

pub(super) struct IntlSegmenterSegmentContinuation {
    segmenter: ObjectId,
    realm: RealmId,
    origin: JsStackFrame,
}

impl IntlSegmenterSegmentContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.segmenter)));
    }
}

pub(super) struct IntlSegmentsContainingContinuation {
    segments: ObjectId,
    realm: RealmId,
    origin: JsStackFrame,
}

impl IntlSegmentsContainingContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.segments)));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlListFormatOperation {
    Format,
    FormatToParts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlListFormatValueStage {
    IteratorMethod,
    Iterator,
    NextMethod,
    NextResult,
    Done,
    Value,
}

pub(super) struct IntlListFormatValueContinuation {
    formatter: ObjectId,
    items: StoredValue,
    iterator: Option<StoredValue>,
    next: Option<StoredValue>,
    result: Option<StoredValue>,
    values: Vec<String>,
    operation: IntlListFormatOperation,
    realm: RealmId,
    stage: IntlListFormatValueStage,
    origin: JsStackFrame,
}

impl IntlListFormatValueContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        2_u64
            .saturating_add(u64::from(self.iterator.is_some()))
            .saturating_add(u64::from(self.next.is_some()))
            .saturating_add(u64::from(self.result.is_some()))
            .saturating_add(usize_to_u64(self.values.len()))
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        mark(CollectionRoot::Heap(HeapReference::Object(self.formatter)));
        trace_stored_value_root(&self.items, mark);
        for value in [
            self.iterator.as_ref(),
            self.next.as_ref(),
            self.result.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            trace_stored_value_root(value, mark);
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
    ZonedDateTime,
}

pub(super) enum IntlDateTimeFormatLocaleValue {
    Date(temporal_rs::Instant),
    Instant(temporal_rs::Instant),
    PlainDate(temporal_rs::PlainDate),
    PlainDateTime(temporal_rs::PlainDateTime),
    PlainMonthDay(temporal_rs::PlainMonthDay),
    PlainTime(temporal_rs::PlainTime),
    PlainYearMonth(temporal_rs::PlainYearMonth),
    ZonedDateTime(temporal_rs::ZonedDateTime),
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
    format_value: Option<IntlDateTimeFormatLocaleValue>,
    to_locale_string_time_zone: Option<String>,
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
                    state.locale_options.numeric = Some(runtime.to_boolean(&value)?);
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
        target: IntlCollatorTarget::Constructor { new_target },
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

#[expect(
    clippy::too_many_arguments,
    reason = "localeCompare carries both converted strings plus its raw ECMA-402 arguments and native resumption context"
)]
pub(super) fn begin_intl_string_locale_compare(
    runtime: &mut Runtime,
    first: JsString,
    second: JsString,
    locales: StoredValue,
    options_argument: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = IntlCollatorConstructorContinuation {
        target: IntlCollatorTarget::LocaleCompare { first, second },
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

#[expect(
    clippy::too_many_arguments,
    reason = "locale-sensitive case conversion carries its converted subject, raw locales, direction, and native resumption context"
)]
pub(super) fn begin_intl_string_case_mapping(
    runtime: &mut Runtime,
    subject: JsString,
    locales: StoredValue,
    uppercase: bool,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let state = IntlStringCaseContinuation { subject, uppercase };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::StringCase(Box::new(state)),
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
                        runtime.to_boolean(&value)?,
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
                let IntlCollatorTarget::Constructor { new_target } = state.target else {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "String.prototype.localeCompare awaited a Collator prototype",
                    }
                    .into());
                };
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
                        let target_realm = runtime.function_realm(new_target)?;
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
    let resolved =
        resolve_collator(&state.requested_locales, state.options.clone()).map_err(|_| {
            EngineFault::RuntimeInvariant {
                message: "canonical Collator inputs failed locale resolution",
            }
        })?;
    if let IntlCollatorTarget::LocaleCompare { first, second } = &state.target {
        execution_budget.charge_instructions(
            u64::from(first.len())
                .saturating_add(u64::from(second.len()))
                .saturating_add(1),
        )?;
        let ordering =
            compare_with_collator(&resolved, &first.to_utf8_lossy()?, &second.to_utf8_lossy()?)
                .map_err(|_| EngineFault::RuntimeInvariant {
                    message: "localeCompare Collator slots failed ICU comparison",
                })?;
        let result = match ordering {
            core::cmp::Ordering::Less => -1,
            core::cmp::Ordering::Equal => 0,
            core::cmp::Ordering::Greater => 1,
        };
        return Ok(NativeDispatch::Immediate(StoredValue::Number(
            JsNumber::from_i32(result),
        )));
    }
    let IntlCollatorTarget::Constructor { new_target } = state.target else {
        unreachable!("localeCompare returned before Collator allocation")
    };
    state.target = IntlCollatorTarget::Constructor { new_target };
    state.resolved = Some(resolved);
    state.stage = IntlCollatorConstructorStage::AwaitPrototype;
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
                store_intl_number_format_option(runtime, &mut state, option, value)?;
                advance_intl_number_format_option(&mut state);
            }
            IntlNumberFormatConstructorStage::AwaitOptionPrimitive => {
                let primitive = take_intl_number_format_constructor_completion(&mut completion)?;
                let option = IntlNumberFormatOption::ALL[state.option_index];
                store_intl_number_format_option(runtime, &mut state, option, primitive)?;
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
                if !runtime.to_boolean(&completion)? {
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
    runtime: &Runtime,
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
        state.options.use_grouping = Some(if runtime.to_boolean(&value)? {
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
            if !runtime.to_boolean(completion)? {
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

pub(super) fn begin_intl_relative_time_format_constructor(
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
            "Intl.RelativeTimeFormat requires 'new'",
        );
    };
    let locales = inputs.arguments.take_first_or_undefined();
    let options_argument = inputs.arguments.take_first_or_undefined();
    let state = IntlRelativeTimeFormatConstructorContinuation {
        new_target,
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        options: RelativeTimeFormatRequestOptions::default(),
        resolved: None,
        option_index: 0,
        realm,
        stage: IntlRelativeTimeFormatConstructorStage::ReadOption,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::RelativeTimeFormatConstructor(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_relative_time_format_options(
    runtime: &mut Runtime,
    mut state: IntlRelativeTimeFormatConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_relative_time_format_options(
            runtime,
            state,
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
    advance_intl_relative_time_format_constructor(runtime, state, None, return_to, execution_budget)
}

#[allow(
    clippy::too_many_lines,
    reason = "RelativeTimeFormat option Gets and resumable conversions stay in normative order"
)]
pub(super) fn advance_intl_relative_time_format_constructor(
    runtime: &mut Runtime,
    mut state: IntlRelativeTimeFormatConstructorContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            IntlRelativeTimeFormatConstructorStage::ReadOption => {
                let Some(option) = IntlRelativeTimeFormatOption::ALL
                    .get(state.option_index)
                    .copied()
                else {
                    return finish_intl_relative_time_format_options(
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
                        message: "Intl.RelativeTimeFormat option iteration lost its options object",
                    })?
                    .duplicate();
                charge_heap_property_lookup(runtime, &base, execution_budget)?;
                let name = JsString::from_utf8(option.name())?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = IntlRelativeTimeFormatConstructorStage::AwaitOption;
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
                return continue_intl_relative_time_format_constructor_after(
                    dispatch,
                    state,
                    runtime,
                    return_to,
                    execution_budget,
                );
            }
            IntlRelativeTimeFormatConstructorStage::AwaitOption => {
                let value = take_intl_relative_time_format_constructor_completion(&mut completion)?;
                let option = IntlRelativeTimeFormatOption::ALL[state.option_index];
                if matches!(value, StoredValue::Undefined) {
                    advance_intl_relative_time_format_option(&mut state);
                    continue;
                }
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    state.stage = IntlRelativeTimeFormatConstructorStage::AwaitOptionPrimitive;
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::String,
                        OperatorPrimitiveTarget::IntlRelativeTimeFormatConstructor(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
                store_intl_relative_time_format_option(&mut state, option, &text)?;
                advance_intl_relative_time_format_option(&mut state);
            }
            IntlRelativeTimeFormatConstructorStage::AwaitOptionPrimitive => {
                let value = take_intl_relative_time_format_constructor_completion(&mut completion)?;
                let option = IntlRelativeTimeFormatOption::ALL[state.option_index];
                let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
                store_intl_relative_time_format_option(&mut state, option, &text)?;
                advance_intl_relative_time_format_option(&mut state);
            }
            IntlRelativeTimeFormatConstructorStage::AwaitPrototype => {
                let requested =
                    take_intl_relative_time_format_constructor_completion(&mut completion)?;
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
                            runtime.realm_intl_relative_time_format_prototype(target_realm)?,
                        )
                    }
                };
                let resolved = state.resolved.ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.RelativeTimeFormat allocation lost its resolved slots",
                })?;
                let object = runtime.allocate_intl_relative_time_format(prototype, resolved)?;
                return Ok(NativeDispatch::Immediate(StoredValue::Object(object)));
            }
        }
    }
}

fn advance_intl_relative_time_format_option(
    state: &mut IntlRelativeTimeFormatConstructorContinuation,
) {
    state.option_index = state.option_index.saturating_add(1);
    state.stage = IntlRelativeTimeFormatConstructorStage::ReadOption;
}

fn store_intl_relative_time_format_option(
    state: &mut IntlRelativeTimeFormatConstructorContinuation,
    option: IntlRelativeTimeFormatOption,
    text: &JsString,
) -> Result<(), NativeFailure> {
    let value = text.to_utf8_lossy()?;
    match option {
        IntlRelativeTimeFormatOption::LocaleMatcher => {
            if !matches!(value.as_str(), "lookup" | "best fit") {
                return invalid_intl_relative_time_format_option(state, option);
            }
        }
        IntlRelativeTimeFormatOption::NumberingSystem => {
            let Some(numbering_system) = canonical_unicode_locale_type(&value) else {
                return invalid_intl_relative_time_format_option(state, option);
            };
            state.options.numbering_system = Some(numbering_system);
        }
        IntlRelativeTimeFormatOption::Style => {
            state.options.style = Some(match value.as_str() {
                "long" => RelativeTimeFormatStyle::Long,
                "short" => RelativeTimeFormatStyle::Short,
                "narrow" => RelativeTimeFormatStyle::Narrow,
                _ => return invalid_intl_relative_time_format_option(state, option),
            });
        }
        IntlRelativeTimeFormatOption::Numeric => {
            state.options.numeric = Some(match value.as_str() {
                "always" => RelativeTimeFormatNumeric::Always,
                "auto" => RelativeTimeFormatNumeric::Auto,
                _ => return invalid_intl_relative_time_format_option(state, option),
            });
        }
    }
    Ok(())
}

fn invalid_intl_relative_time_format_option<T>(
    state: &IntlRelativeTimeFormatConstructorContinuation,
    option: IntlRelativeTimeFormatOption,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        state.realm,
        state.origin.clone(),
        ExceptionKind::RangeError,
        &format!("invalid Intl.RelativeTimeFormat {} option", option.name()),
    )
}

fn finish_intl_relative_time_format_options(
    runtime: &mut Runtime,
    mut state: IntlRelativeTimeFormatConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget
        .charge_instructions(usize_to_u64(state.requested_locales.len()).saturating_add(1))?;
    state.resolved = Some(
        resolve_relative_time_format(&state.requested_locales, state.options.clone()).map_err(
            |_| EngineFault::RuntimeInvariant {
                message: "canonical RelativeTimeFormat inputs failed locale resolution",
            },
        )?,
    );
    state.stage = IntlRelativeTimeFormatConstructorStage::AwaitPrototype;
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
    continue_intl_relative_time_format_constructor_after(
        dispatch,
        state,
        runtime,
        return_to,
        execution_budget,
    )
}

fn continue_intl_relative_time_format_constructor_after(
    dispatch: NativeDispatch,
    state: IntlRelativeTimeFormatConstructorContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        |state| NativeContinuation::IntlRelativeTimeFormatConstructor(Box::new(state)),
        |state, value| {
            advance_intl_relative_time_format_constructor(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.RelativeTimeFormat property Get produced a structured result",
    )
}

fn take_intl_relative_time_format_constructor_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.RelativeTimeFormat constructor resumed without a completion",
    })
}

pub(super) fn begin_intl_relative_time_format_supported_locales_of(
    runtime: &mut Runtime,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let locales = arguments.take_first_or_undefined();
    let options_argument = arguments.take_first_or_undefined();
    let state = IntlRelativeTimeFormatSupportedLocalesContinuation {
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        realm,
        stage: IntlRelativeTimeFormatSupportedLocalesStage::ReadLocaleMatcher,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::RelativeTimeFormatSupportedLocalesOf(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_relative_time_format_supported_locales_options(
    runtime: &mut Runtime,
    mut state: IntlRelativeTimeFormatSupportedLocalesContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_relative_time_format_supported_locales(runtime, &state);
    }
    let options_argument = state.options_argument.duplicate();
    state.options_object = Some(
        match to_object_value(runtime, state.realm, options_argument, state.origin.clone())? {
            Ok(options) => options,
            Err(exception) => return Err(NativeFailure::Abrupt(exception)),
        },
    );
    advance_intl_relative_time_format_supported_locales(
        runtime,
        state,
        None,
        return_to,
        execution_budget,
    )
}

pub(super) fn advance_intl_relative_time_format_supported_locales(
    runtime: &mut Runtime,
    mut state: IntlRelativeTimeFormatSupportedLocalesContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    match state.stage {
        IntlRelativeTimeFormatSupportedLocalesStage::ReadLocaleMatcher => {
            let base = state
                .options_object
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.RelativeTimeFormat.supportedLocalesOf lost its options object",
                })?
                .duplicate();
            charge_heap_property_lookup(runtime, &base, execution_budget)?;
            let name = JsString::from_utf8("localeMatcher")?;
            let key = runtime.property_key_from_string(&name)?;
            state.stage = IntlRelativeTimeFormatSupportedLocalesStage::AwaitLocaleMatcher;
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
            continue_intl_relative_time_format_supported_locales_after(
                dispatch,
                state,
                runtime,
                return_to,
                execution_budget,
            )
        }
        IntlRelativeTimeFormatSupportedLocalesStage::AwaitLocaleMatcher => {
            let value =
                take_intl_relative_time_format_supported_locales_completion(&mut completion)?;
            if matches!(value, StoredValue::Undefined) {
                return finish_intl_relative_time_format_supported_locales(runtime, &state);
            }
            if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                state.stage =
                    IntlRelativeTimeFormatSupportedLocalesStage::AwaitLocaleMatcherPrimitive;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::IntlRelativeTimeFormatSupportedLocalesOf(Box::new(
                        state,
                    )),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            validate_intl_relative_time_format_locale_matcher(&state, value)?;
            finish_intl_relative_time_format_supported_locales(runtime, &state)
        }
        IntlRelativeTimeFormatSupportedLocalesStage::AwaitLocaleMatcherPrimitive => {
            let value =
                take_intl_relative_time_format_supported_locales_completion(&mut completion)?;
            validate_intl_relative_time_format_locale_matcher(&state, value)?;
            finish_intl_relative_time_format_supported_locales(runtime, &state)
        }
    }
}

fn validate_intl_relative_time_format_locale_matcher(
    state: &IntlRelativeTimeFormatSupportedLocalesContinuation,
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
        "invalid Intl.RelativeTimeFormat localeMatcher option",
    )
}

fn finish_intl_relative_time_format_supported_locales(
    runtime: &mut Runtime,
    state: &IntlRelativeTimeFormatSupportedLocalesContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    intl_locale_string_array(
        runtime,
        state.realm,
        relative_time_format_supported_locales(&state.requested_locales),
    )
}

fn continue_intl_relative_time_format_supported_locales_after(
    dispatch: NativeDispatch,
    state: IntlRelativeTimeFormatSupportedLocalesContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        |state| NativeContinuation::IntlRelativeTimeFormatSupportedLocalesOf(Box::new(state)),
        |state, value| {
            advance_intl_relative_time_format_supported_locales(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.RelativeTimeFormat.supportedLocalesOf Get produced a structured result",
    )
}

fn take_intl_relative_time_format_supported_locales_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.RelativeTimeFormat.supportedLocalesOf resumed without a completion",
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch keeps receiver, arguments, realm, return target, origin, and budget explicit"
)]
pub(super) fn begin_intl_relative_time_format_prototype(
    runtime: &mut Runtime,
    method: IntlRelativeTimeFormatPrototypeMethod,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(formatter) = receiver else {
        return intl_relative_time_format_brand_error(realm, origin);
    };
    let Some(resolved) = runtime
        .intl_relative_time_format_state(*formatter)?
        .cloned()
    else {
        return intl_relative_time_format_brand_error(realm, origin);
    };
    match method {
        IntlRelativeTimeFormatPrototypeMethod::ResolvedOptions => {
            intl_relative_time_format_resolved_options(runtime, realm, &resolved)
        }
        IntlRelativeTimeFormatPrototypeMethod::Format
        | IntlRelativeTimeFormatPrototypeMethod::FormatToParts => {
            let value = arguments.take_first_or_undefined();
            let unit = arguments.take_first_or_undefined();
            begin_intl_relative_time_format_value(
                runtime,
                IntlRelativeTimeFormatValueContinuation {
                    formatter: *formatter,
                    operation: if matches!(method, IntlRelativeTimeFormatPrototypeMethod::Format) {
                        IntlRelativeTimeFormatOperation::Format
                    } else {
                        IntlRelativeTimeFormatOperation::FormatToParts
                    },
                    unit: Some(unit),
                    value: None,
                    realm,
                    origin,
                },
                value,
                return_to,
                execution_budget,
            )
        }
    }
}

fn begin_intl_relative_time_format_value(
    runtime: &mut Runtime,
    state: IntlRelativeTimeFormatValueContinuation,
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
            OperatorPrimitiveTarget::IntlRelativeTimeFormatValue(Box::new(state)),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    finish_intl_relative_time_format_value_primitive(
        runtime,
        state,
        value,
        return_to,
        execution_budget,
    )
}

pub(super) fn finish_intl_relative_time_format_value_primitive(
    runtime: &mut Runtime,
    mut state: IntlRelativeTimeFormatValueContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.value = Some(operator_to_number(value, state.realm, &state.origin)?.as_f64());
    let unit = state.unit.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.RelativeTimeFormat operation lost its unit",
    })?;
    begin_intl_relative_time_format_unit(runtime, state, unit, return_to, execution_budget)
}

fn begin_intl_relative_time_format_unit(
    runtime: &mut Runtime,
    state: IntlRelativeTimeFormatValueContinuation,
    unit: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(unit, StoredValue::Function(_) | StoredValue::Object(_)) {
        let realm = state.realm;
        let origin = state.origin.clone();
        return begin_operator_primitive_conversion(
            runtime,
            unit,
            OperatorPrimitiveHint::String,
            OperatorPrimitiveTarget::IntlRelativeTimeFormatUnit(Box::new(state)),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    finish_intl_relative_time_format_unit_primitive(runtime, &state, unit)
}

pub(super) fn finish_intl_relative_time_format_unit_primitive(
    runtime: &mut Runtime,
    state: &IntlRelativeTimeFormatValueContinuation,
    unit: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let unit = operator_primitive_to_string(unit, state.realm, &state.origin)?.to_utf8_lossy()?;
    let unit = parse_intl_relative_time_unit(state, &unit)?;
    let value = state.value.ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.RelativeTimeFormat operation lost its numeric operand",
    })?;
    let resolved = runtime
        .intl_relative_time_format_state(state.formatter)?
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Intl.RelativeTimeFormat operation lost its branded receiver",
        })?;
    match state.operation {
        IntlRelativeTimeFormatOperation::Format => {
            let formatted = format_relative_time(resolved, value, unit)
                .map_err(|error| intl_relative_time_format_operation_error(state, error))?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&formatted)?,
            )))
        }
        IntlRelativeTimeFormatOperation::FormatToParts => {
            let parts = format_relative_time_to_parts(resolved, value, unit)
                .map_err(|error| intl_relative_time_format_operation_error(state, error))?;
            intl_relative_time_format_parts_array(runtime, state.realm, parts)
        }
    }
}

fn parse_intl_relative_time_unit(
    state: &IntlRelativeTimeFormatValueContinuation,
    unit: &str,
) -> Result<RelativeTimeUnit, NativeFailure> {
    match unit {
        "second" | "seconds" => Ok(RelativeTimeUnit::Second),
        "minute" | "minutes" => Ok(RelativeTimeUnit::Minute),
        "hour" | "hours" => Ok(RelativeTimeUnit::Hour),
        "day" | "days" => Ok(RelativeTimeUnit::Day),
        "week" | "weeks" => Ok(RelativeTimeUnit::Week),
        "month" | "months" => Ok(RelativeTimeUnit::Month),
        "quarter" | "quarters" => Ok(RelativeTimeUnit::Quarter),
        "year" | "years" => Ok(RelativeTimeUnit::Year),
        _ => intl_locale_list_error(
            state.realm,
            state.origin.clone(),
            ExceptionKind::RangeError,
            "invalid Intl.RelativeTimeFormat unit",
        ),
    }
}

fn intl_relative_time_format_operation_error(
    state: &IntlRelativeTimeFormatValueContinuation,
    error: RelativeTimeFormatError,
) -> NativeFailure {
    match error {
        RelativeTimeFormatError::NonFinite | RelativeTimeFormatError::InvalidOption => {
            NativeFailure::Abrupt(PendingException {
                realm: state.realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::RangeError,
                    message: JsString::from_utf8("Intl.RelativeTimeFormat value must be finite")
                        .expect("static Intl error is valid UTF-8"),
                },
                origin: state.origin.clone(),
            })
        }
        RelativeTimeFormatError::InvalidLocale | RelativeTimeFormatError::Data => {
            EngineFault::RuntimeInvariant {
                message: "resolved Intl.RelativeTimeFormat slots failed ICU formatting",
            }
            .into()
        }
    }
}

fn intl_relative_time_format_parts_array(
    runtime: &mut Runtime,
    realm: RealmId,
    parts: Vec<quickjs_intl::RelativeTimeFormatPart>,
) -> Result<NativeDispatch, NativeFailure> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(parts.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: parts.len(),
        })?;
    for part in parts {
        let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
        let mut properties = vec![
            ("type", StoredValue::String(JsString::from_utf8(part.kind)?)),
            (
                "value",
                StoredValue::String(JsString::from_utf8(&part.value)?),
            ),
        ];
        if let Some(unit) = part.unit {
            properties.push(("unit", StoredValue::String(JsString::from_utf8(unit)?)));
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

fn intl_relative_time_format_resolved_options(
    runtime: &mut Runtime,
    realm: RealmId,
    state: &RelativeTimeFormatState,
) -> Result<NativeDispatch, NativeFailure> {
    let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    let properties = [
        (
            "locale",
            StoredValue::String(JsString::from_utf8(&state.locale)?),
        ),
        (
            "style",
            StoredValue::String(JsString::from_utf8(state.style.as_str())?),
        ),
        (
            "numeric",
            StoredValue::String(JsString::from_utf8(state.numeric.as_str())?),
        ),
        (
            "numberingSystem",
            StoredValue::String(JsString::from_utf8(&state.numbering_system)?),
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

fn intl_relative_time_format_brand_error<T>(
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        realm,
        origin,
        ExceptionKind::TypeError,
        "Intl.RelativeTimeFormat method called on incompatible receiver",
    )
}

pub(super) fn begin_intl_list_format_constructor(
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
            "Intl.ListFormat requires 'new'",
        );
    };
    let locales = inputs.arguments.take_first_or_undefined();
    let options_argument = inputs.arguments.take_first_or_undefined();
    let state = IntlListFormatConstructorContinuation {
        new_target,
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        options: ListFormatRequestOptions::default(),
        resolved: None,
        option_index: 0,
        realm,
        stage: IntlListFormatConstructorStage::ReadOption,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::ListFormatConstructor(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_list_format_options(
    runtime: &mut Runtime,
    mut state: IntlListFormatConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_list_format_options(runtime, state, return_to, execution_budget);
    }
    if !matches!(
        state.options_argument,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        return intl_locale_list_error(
            state.realm,
            state.origin,
            ExceptionKind::TypeError,
            "Intl.ListFormat options must be an object",
        );
    }
    state.options_object = Some(state.options_argument.duplicate());
    advance_intl_list_format_constructor(runtime, state, None, return_to, execution_budget)
}

#[allow(
    clippy::too_many_lines,
    reason = "ListFormat option Gets and resumable conversions stay in normative order"
)]
pub(super) fn advance_intl_list_format_constructor(
    runtime: &mut Runtime,
    mut state: IntlListFormatConstructorContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            IntlListFormatConstructorStage::ReadOption => {
                let Some(option) = IntlListFormatOption::ALL.get(state.option_index).copied()
                else {
                    return finish_intl_list_format_options(
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
                        message: "Intl.ListFormat option iteration lost its options object",
                    })?
                    .duplicate();
                charge_heap_property_lookup(runtime, &base, execution_budget)?;
                let name = JsString::from_utf8(option.name())?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = IntlListFormatConstructorStage::AwaitOption;
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
                return continue_intl_list_format_constructor_after(
                    dispatch,
                    state,
                    runtime,
                    return_to,
                    execution_budget,
                );
            }
            IntlListFormatConstructorStage::AwaitOption => {
                let value = take_intl_list_format_constructor_completion(&mut completion)?;
                let option = IntlListFormatOption::ALL[state.option_index];
                if matches!(value, StoredValue::Undefined) {
                    advance_intl_list_format_option(&mut state);
                    continue;
                }
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    state.stage = IntlListFormatConstructorStage::AwaitOptionPrimitive;
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::String,
                        OperatorPrimitiveTarget::IntlListFormatConstructor(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
                store_intl_list_format_option(&mut state, option, &text)?;
                advance_intl_list_format_option(&mut state);
            }
            IntlListFormatConstructorStage::AwaitOptionPrimitive => {
                let value = take_intl_list_format_constructor_completion(&mut completion)?;
                let option = IntlListFormatOption::ALL[state.option_index];
                let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
                store_intl_list_format_option(&mut state, option, &text)?;
                advance_intl_list_format_option(&mut state);
            }
            IntlListFormatConstructorStage::AwaitPrototype => {
                let requested = take_intl_list_format_constructor_completion(&mut completion)?;
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
                            runtime.realm_intl_list_format_prototype(target_realm)?,
                        )
                    }
                };
                let resolved = state.resolved.ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.ListFormat allocation lost its resolved slots",
                })?;
                let object = runtime.allocate_intl_list_format(prototype, resolved)?;
                return Ok(NativeDispatch::Immediate(StoredValue::Object(object)));
            }
        }
    }
}

fn advance_intl_list_format_option(state: &mut IntlListFormatConstructorContinuation) {
    state.option_index = state.option_index.saturating_add(1);
    state.stage = IntlListFormatConstructorStage::ReadOption;
}

fn store_intl_list_format_option(
    state: &mut IntlListFormatConstructorContinuation,
    option: IntlListFormatOption,
    text: &JsString,
) -> Result<(), NativeFailure> {
    let value = text.to_utf8_lossy()?;
    match option {
        IntlListFormatOption::LocaleMatcher => {
            if !matches!(value.as_str(), "lookup" | "best fit") {
                return invalid_intl_list_format_option(state, option);
            }
        }
        IntlListFormatOption::Type => {
            state.options.list_type = Some(match value.as_str() {
                "conjunction" => ListFormatType::Conjunction,
                "disjunction" => ListFormatType::Disjunction,
                "unit" => ListFormatType::Unit,
                _ => return invalid_intl_list_format_option(state, option),
            });
        }
        IntlListFormatOption::Style => {
            state.options.style = Some(match value.as_str() {
                "long" => ListFormatStyle::Long,
                "short" => ListFormatStyle::Short,
                "narrow" => ListFormatStyle::Narrow,
                _ => return invalid_intl_list_format_option(state, option),
            });
        }
    }
    Ok(())
}

fn invalid_intl_list_format_option<T>(
    state: &IntlListFormatConstructorContinuation,
    option: IntlListFormatOption,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        state.realm,
        state.origin.clone(),
        ExceptionKind::RangeError,
        &format!("invalid Intl.ListFormat {} option", option.name()),
    )
}

fn finish_intl_list_format_options(
    runtime: &mut Runtime,
    mut state: IntlListFormatConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget
        .charge_instructions(usize_to_u64(state.requested_locales.len()).saturating_add(1))?;
    state.resolved = Some(
        resolve_list_format(&state.requested_locales, state.options).map_err(|_| {
            EngineFault::RuntimeInvariant {
                message: "canonical ListFormat inputs failed locale resolution",
            }
        })?,
    );
    state.stage = IntlListFormatConstructorStage::AwaitPrototype;
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
    continue_intl_list_format_constructor_after(
        dispatch,
        state,
        runtime,
        return_to,
        execution_budget,
    )
}

fn continue_intl_list_format_constructor_after(
    dispatch: NativeDispatch,
    state: IntlListFormatConstructorContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        |state| NativeContinuation::IntlListFormatConstructor(Box::new(state)),
        |state, value| {
            advance_intl_list_format_constructor(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.ListFormat property Get produced a structured result",
    )
}

fn take_intl_list_format_constructor_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.ListFormat constructor resumed without a completion",
    })
}

pub(super) fn begin_intl_list_format_supported_locales_of(
    runtime: &mut Runtime,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let locales = arguments.take_first_or_undefined();
    let options_argument = arguments.take_first_or_undefined();
    let state = IntlListFormatSupportedLocalesContinuation {
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        realm,
        stage: IntlListFormatSupportedLocalesStage::ReadLocaleMatcher,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::ListFormatSupportedLocalesOf(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_list_format_supported_locales_options(
    runtime: &mut Runtime,
    mut state: IntlListFormatSupportedLocalesContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_list_format_supported_locales(runtime, &state);
    }
    let options_argument = state.options_argument.duplicate();
    state.options_object = Some(
        match to_object_value(runtime, state.realm, options_argument, state.origin.clone())? {
            Ok(options) => options,
            Err(exception) => return Err(NativeFailure::Abrupt(exception)),
        },
    );
    advance_intl_list_format_supported_locales(runtime, state, None, return_to, execution_budget)
}

pub(super) fn advance_intl_list_format_supported_locales(
    runtime: &mut Runtime,
    mut state: IntlListFormatSupportedLocalesContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    match state.stage {
        IntlListFormatSupportedLocalesStage::ReadLocaleMatcher => {
            let base = state
                .options_object
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.ListFormat.supportedLocalesOf lost its options object",
                })?
                .duplicate();
            charge_heap_property_lookup(runtime, &base, execution_budget)?;
            let name = JsString::from_utf8("localeMatcher")?;
            let key = runtime.property_key_from_string(&name)?;
            state.stage = IntlListFormatSupportedLocalesStage::AwaitLocaleMatcher;
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
            continue_intl_list_format_supported_locales_after(
                dispatch,
                state,
                runtime,
                return_to,
                execution_budget,
            )
        }
        IntlListFormatSupportedLocalesStage::AwaitLocaleMatcher => {
            let value = take_intl_list_format_supported_locales_completion(&mut completion)?;
            if matches!(value, StoredValue::Undefined) {
                return finish_intl_list_format_supported_locales(runtime, &state);
            }
            if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                state.stage = IntlListFormatSupportedLocalesStage::AwaitLocaleMatcherPrimitive;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::IntlListFormatSupportedLocalesOf(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            validate_intl_list_format_locale_matcher(&state, value)?;
            finish_intl_list_format_supported_locales(runtime, &state)
        }
        IntlListFormatSupportedLocalesStage::AwaitLocaleMatcherPrimitive => {
            let value = take_intl_list_format_supported_locales_completion(&mut completion)?;
            validate_intl_list_format_locale_matcher(&state, value)?;
            finish_intl_list_format_supported_locales(runtime, &state)
        }
    }
}

fn validate_intl_list_format_locale_matcher(
    state: &IntlListFormatSupportedLocalesContinuation,
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
        "invalid Intl.ListFormat localeMatcher option",
    )
}

fn finish_intl_list_format_supported_locales(
    runtime: &mut Runtime,
    state: &IntlListFormatSupportedLocalesContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    intl_locale_string_array(
        runtime,
        state.realm,
        list_format_supported_locales(&state.requested_locales),
    )
}

fn continue_intl_list_format_supported_locales_after(
    dispatch: NativeDispatch,
    state: IntlListFormatSupportedLocalesContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        |state| NativeContinuation::IntlListFormatSupportedLocalesOf(Box::new(state)),
        |state, value| {
            advance_intl_list_format_supported_locales(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.ListFormat.supportedLocalesOf Get produced a structured result",
    )
}

fn take_intl_list_format_supported_locales_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.ListFormat.supportedLocalesOf resumed without a completion",
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch keeps receiver, arguments, realm, return target, origin, and budget explicit"
)]
pub(super) fn begin_intl_list_format_prototype(
    runtime: &mut Runtime,
    method: IntlListFormatPrototypeMethod,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(formatter) = receiver else {
        return intl_list_format_brand_error(realm, origin);
    };
    let Some(resolved) = runtime.intl_list_format_state(*formatter)?.cloned() else {
        return intl_list_format_brand_error(realm, origin);
    };
    match method {
        IntlListFormatPrototypeMethod::ResolvedOptions => {
            intl_list_format_resolved_options(runtime, realm, &resolved)
        }
        IntlListFormatPrototypeMethod::Format | IntlListFormatPrototypeMethod::FormatToParts => {
            let items = arguments.take_first_or_undefined();
            begin_intl_list_format_value(
                runtime,
                IntlListFormatValueContinuation {
                    formatter: *formatter,
                    items,
                    iterator: None,
                    next: None,
                    result: None,
                    values: Vec::new(),
                    operation: if matches!(method, IntlListFormatPrototypeMethod::Format) {
                        IntlListFormatOperation::Format
                    } else {
                        IntlListFormatOperation::FormatToParts
                    },
                    realm,
                    stage: IntlListFormatValueStage::IteratorMethod,
                    origin,
                },
                return_to,
                execution_budget,
            )
        }
    }
}

fn begin_intl_list_format_value(
    runtime: &mut Runtime,
    state: IntlListFormatValueContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.items, StoredValue::Undefined) {
        return finish_intl_list_format_operation(runtime, &state);
    }
    read_intl_list_format_property(
        runtime,
        state,
        &runtime.predefined_symbol_property_key(PredefinedAtom::SymbolIterator),
        return_to,
        execution_budget,
    )
}

pub(super) fn advance_intl_list_format_value(
    runtime: &mut Runtime,
    mut state: IntlListFormatValueContinuation,
    completion: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IntlListFormatValueStage::IteratorMethod => {
            let StoredValue::Function(method) = completion else {
                return intl_list_format_type_error(
                    state.realm,
                    state.origin,
                    "value is not iterable",
                );
            };
            let receiver = state.items.duplicate();
            state.stage = IntlListFormatValueStage::Iterator;
            call_intl_list_format_function(method, receiver, state, return_to)
        }
        IntlListFormatValueStage::Iterator => {
            if completion.heap_reference().is_none() {
                return intl_list_format_type_error(
                    state.realm,
                    state.origin,
                    "iterator is not an object",
                );
            }
            state.iterator = Some(completion);
            state.stage = IntlListFormatValueStage::NextMethod;
            read_intl_list_format_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Next),
                return_to,
                execution_budget,
            )
        }
        IntlListFormatValueStage::NextMethod => {
            state.next = Some(completion);
            call_intl_list_format_next(state, return_to, execution_budget)
        }
        IntlListFormatValueStage::NextResult => {
            if completion.heap_reference().is_none() {
                return intl_list_format_type_error(
                    state.realm,
                    state.origin,
                    "iterator result is not an object",
                );
            }
            state.result = Some(completion);
            state.stage = IntlListFormatValueStage::Done;
            read_intl_list_format_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Done),
                return_to,
                execution_budget,
            )
        }
        IntlListFormatValueStage::Done => {
            if runtime.to_boolean(&completion)? {
                return finish_intl_list_format_operation(runtime, &state);
            }
            state.stage = IntlListFormatValueStage::Value;
            read_intl_list_format_property(
                runtime,
                state,
                &runtime.predefined_property_key(PredefinedAtom::Value),
                return_to,
                execution_budget,
            )
        }
        IntlListFormatValueStage::Value => {
            if usize_to_u64(state.values.len()) >= MAX_SAFE_INTEGER {
                return close_intl_list_format_with_type_error(
                    runtime,
                    state,
                    "too many list elements",
                    return_to,
                    execution_budget,
                );
            }
            let StoredValue::String(value) = completion else {
                return close_intl_list_format_with_type_error(
                    runtime,
                    state,
                    "list element is not a string",
                    return_to,
                    execution_budget,
                );
            };
            state
                .values
                .try_reserve(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::FrameValues,
                    additional: 1,
                })?;
            execution_budget.charge_instructions(1)?;
            state.values.push(value.to_utf8_lossy()?);
            state.result = None;
            call_intl_list_format_next(state, return_to, execution_budget)
        }
    }
}

fn read_intl_list_format_property(
    runtime: &mut Runtime,
    state: IntlListFormatValueContinuation,
    key: &PropertyKey,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (base, property_name) = match state.stage {
        IntlListFormatValueStage::IteratorMethod => (&state.items, "Symbol.iterator"),
        IntlListFormatValueStage::NextMethod => (
            state
                .iterator
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.ListFormat next lookup has no iterator",
                })?,
            "next",
        ),
        IntlListFormatValueStage::Done => (
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "Intl.ListFormat done lookup has no iterator result",
            })?,
            "done",
        ),
        IntlListFormatValueStage::Value => (
            state.result.as_ref().ok_or(EngineFault::RuntimeInvariant {
                message: "Intl.ListFormat value lookup has no iterator result",
            })?,
            "value",
        ),
        IntlListFormatValueStage::Iterator | IntlListFormatValueStage::NextResult => {
            return Err(EngineFault::RuntimeInvariant {
                message: "Intl.ListFormat call stage attempted a property read",
            }
            .into());
        }
    };
    let base = base.duplicate();
    charge_iterator_property_lookup(runtime, &base, execution_budget)?;
    let name = JsString::from_utf8(property_name)?;
    let dispatch = begin_value_get(
        runtime,
        &base,
        key.clone(),
        Some(&name),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        intl_list_format_value_continuation,
        |state, value| {
            advance_intl_list_format_value(runtime, state, value, return_to, execution_budget)
        },
        "Intl.ListFormat iterable Get produced a structured result",
    )
}

fn intl_list_format_value_continuation(
    state: IntlListFormatValueContinuation,
) -> NativeContinuation {
    NativeContinuation::IntlListFormatValue(Box::new(state))
}

fn call_intl_list_format_next(
    mut state: IntlListFormatValueContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let next = state.next.as_ref().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.ListFormat iterator advance has no retained next method",
    })?;
    let StoredValue::Function(next) = next else {
        return intl_list_format_type_error(
            state.realm,
            state.origin,
            "iterator next is not callable",
        );
    };
    execution_budget.charge_instructions(1)?;
    let receiver = state
        .iterator
        .as_ref()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Intl.ListFormat iterator advance has no retained iterator",
        })?
        .duplicate();
    state.stage = IntlListFormatValueStage::NextResult;
    call_intl_list_format_function(*next, receiver, state, return_to)
}

fn call_intl_list_format_function(
    function: FunctionId,
    receiver: StoredValue,
    state: IntlListFormatValueContinuation,
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
    continuations.push(NativeContinuation::IntlListFormatValue(Box::new(state)));
    Ok(NativeDispatch::Call(NativeCall {
        function,
        receiver,
        arguments: CallArguments::from_values(Vec::new()),
        return_to,
        origin,
        continuations,
        pre_call: None,
        new_target: None,
        native_caller: None,
    }))
}

fn finish_intl_list_format_operation(
    runtime: &mut Runtime,
    state: &IntlListFormatValueContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    let resolved = runtime
        .intl_list_format_state(state.formatter)?
        .cloned()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Intl.ListFormat operation lost its branded receiver",
        })?;
    match state.operation {
        IntlListFormatOperation::Format => {
            let formatted =
                format_list(&resolved, &state.values).map_err(intl_list_format_operation_error)?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&formatted)?,
            )))
        }
        IntlListFormatOperation::FormatToParts => {
            let parts = format_list_to_parts(&resolved, &state.values)
                .map_err(intl_list_format_operation_error)?;
            intl_list_format_parts_array(runtime, state.realm, parts)
        }
    }
}

fn intl_list_format_parts_array(
    runtime: &mut Runtime,
    realm: RealmId,
    parts: Vec<quickjs_intl::ListFormatPart>,
) -> Result<NativeDispatch, NativeFailure> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(parts.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: parts.len(),
        })?;
    for part in parts {
        let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
        for (name, value) in [
            ("type", StoredValue::String(JsString::from_utf8(part.kind)?)),
            (
                "value",
                StoredValue::String(JsString::from_utf8(&part.value)?),
            ),
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
        values.push(StoredValue::Object(object));
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(
        runtime.allocate_array(realm, values)?,
    )))
}

fn intl_list_format_resolved_options(
    runtime: &mut Runtime,
    realm: RealmId,
    state: &ListFormatState,
) -> Result<NativeDispatch, NativeFailure> {
    let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    for (name, value) in [
        (
            "locale",
            StoredValue::String(JsString::from_utf8(&state.locale)?),
        ),
        (
            "type",
            StoredValue::String(JsString::from_utf8(state.list_type.as_str())?),
        ),
        (
            "style",
            StoredValue::String(JsString::from_utf8(state.style.as_str())?),
        ),
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

fn intl_list_format_operation_error(_error: ListFormatError) -> NativeFailure {
    EngineFault::RuntimeInvariant {
        message: "resolved Intl.ListFormat slots failed ICU formatting",
    }
    .into()
}

fn intl_list_format_brand_error<T>(
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<T, NativeFailure> {
    intl_list_format_type_error(
        realm,
        origin,
        "Intl.ListFormat method called on incompatible receiver",
    )
}

fn intl_list_format_type_error<T>(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<T, NativeFailure> {
    Err(NativeFailure::Abrupt(intl_list_format_exception(
        realm, origin, message,
    )?))
}

fn close_intl_list_format_with_type_error(
    runtime: &mut Runtime,
    state: IntlListFormatValueContinuation,
    message: &str,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let pending = intl_list_format_exception(state.realm, state.origin.clone(), message)?;
    let iterator = state.iterator.ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.ListFormat IteratorClose started before iterator acquisition",
    })?;
    begin_exceptional_iterator_close(runtime, iterator, pending, return_to, execution_budget)
}

fn intl_list_format_exception(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin,
    })
}

pub(super) fn begin_intl_display_names_constructor(
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
            "Intl.DisplayNames requires 'new'",
        );
    };
    let locales_argument = inputs.arguments.take_first_or_undefined();
    let options_argument = inputs.arguments.take_first_or_undefined();
    let state = IntlDisplayNamesConstructorContinuation {
        new_target,
        locales_argument: Some(locales_argument),
        options_argument,
        options_object: None,
        prototype: None,
        requested_locales: Vec::new(),
        options: DisplayNamesRequestOptions::default(),
        option_index: 0,
        realm,
        stage: IntlDisplayNamesConstructorStage::AwaitPrototype,
        origin,
    };
    let base = StoredValue::Function(new_target);
    charge_heap_property_lookup(runtime, &base, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let dispatch = begin_value_get(
        runtime,
        &base,
        key,
        None,
        realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_intl_display_names_constructor_after(
        dispatch,
        state,
        runtime,
        return_to,
        execution_budget,
    )
}

fn begin_intl_display_names_options(
    runtime: &mut Runtime,
    mut state: IntlDisplayNamesConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_display_names_options(runtime, state);
    }
    if !matches!(
        state.options_argument,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        return intl_locale_list_error(
            state.realm,
            state.origin,
            ExceptionKind::TypeError,
            "Intl.DisplayNames options must be an object",
        );
    }
    state.options_object = Some(state.options_argument.duplicate());
    state.stage = IntlDisplayNamesConstructorStage::ReadOption;
    advance_intl_display_names_constructor(runtime, state, None, return_to, execution_budget)
}

#[allow(
    clippy::too_many_lines,
    reason = "DisplayNames option Gets and resumable conversions stay in normative order"
)]
pub(super) fn advance_intl_display_names_constructor(
    runtime: &mut Runtime,
    mut state: IntlDisplayNamesConstructorContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            IntlDisplayNamesConstructorStage::AwaitPrototype => {
                let requested = take_intl_display_names_constructor_completion(&mut completion)?;
                state.prototype = Some(match requested {
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
                            runtime.realm_intl_display_names_prototype(target_realm)?,
                        )
                    }
                });
                let locales =
                    state
                        .locales_argument
                        .take()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "Intl.DisplayNames constructor lost its locales argument",
                        })?;
                state.stage = IntlDisplayNamesConstructorStage::ReadOption;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_intl_locale_list(
                    runtime,
                    locales,
                    IntlLocaleListTarget::DisplayNamesConstructor(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            IntlDisplayNamesConstructorStage::ReadOption => {
                let Some(option) = IntlDisplayNamesOption::ALL.get(state.option_index).copied()
                else {
                    return finish_intl_display_names_options(runtime, state);
                };
                let base = state
                    .options_object
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Intl.DisplayNames option iteration lost its options object",
                    })?
                    .duplicate();
                charge_heap_property_lookup(runtime, &base, execution_budget)?;
                let name = JsString::from_utf8(option.name())?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = IntlDisplayNamesConstructorStage::AwaitOption;
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
                return continue_intl_display_names_constructor_after(
                    dispatch,
                    state,
                    runtime,
                    return_to,
                    execution_budget,
                );
            }
            IntlDisplayNamesConstructorStage::AwaitOption => {
                let value = take_intl_display_names_constructor_completion(&mut completion)?;
                let option = IntlDisplayNamesOption::ALL[state.option_index];
                if matches!(value, StoredValue::Undefined) {
                    advance_intl_display_names_option(&mut state);
                    continue;
                }
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    state.stage = IntlDisplayNamesConstructorStage::AwaitOptionPrimitive;
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::String,
                        OperatorPrimitiveTarget::IntlDisplayNamesConstructor(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
                store_intl_display_names_option(&mut state, option, &text)?;
                advance_intl_display_names_option(&mut state);
            }
            IntlDisplayNamesConstructorStage::AwaitOptionPrimitive => {
                let value = take_intl_display_names_constructor_completion(&mut completion)?;
                let option = IntlDisplayNamesOption::ALL[state.option_index];
                let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
                store_intl_display_names_option(&mut state, option, &text)?;
                advance_intl_display_names_option(&mut state);
            }
        }
    }
}

fn advance_intl_display_names_option(state: &mut IntlDisplayNamesConstructorContinuation) {
    state.option_index = state.option_index.saturating_add(1);
    state.stage = IntlDisplayNamesConstructorStage::ReadOption;
}

fn store_intl_display_names_option(
    state: &mut IntlDisplayNamesConstructorContinuation,
    option: IntlDisplayNamesOption,
    text: &JsString,
) -> Result<(), NativeFailure> {
    let value = text.to_utf8_lossy()?;
    match option {
        IntlDisplayNamesOption::LocaleMatcher => {
            if !matches!(value.as_str(), "lookup" | "best fit") {
                return invalid_intl_display_names_option(state, option);
            }
        }
        IntlDisplayNamesOption::Style => {
            state.options.style = Some(match value.as_str() {
                "narrow" => DisplayNamesStyle::Narrow,
                "short" => DisplayNamesStyle::Short,
                "long" => DisplayNamesStyle::Long,
                _ => return invalid_intl_display_names_option(state, option),
            });
        }
        IntlDisplayNamesOption::Type => {
            state.options.name_type = Some(match value.as_str() {
                "language" => DisplayNamesType::Language,
                "region" => DisplayNamesType::Region,
                "script" => DisplayNamesType::Script,
                "currency" => DisplayNamesType::Currency,
                "calendar" => DisplayNamesType::Calendar,
                "dateTimeField" => DisplayNamesType::DateTimeField,
                _ => return invalid_intl_display_names_option(state, option),
            });
        }
        IntlDisplayNamesOption::Fallback => {
            state.options.fallback = Some(match value.as_str() {
                "code" => DisplayNamesFallback::Code,
                "none" => DisplayNamesFallback::None,
                _ => return invalid_intl_display_names_option(state, option),
            });
        }
        IntlDisplayNamesOption::LanguageDisplay => {
            state.options.language_display = Some(match value.as_str() {
                "dialect" => DisplayNamesLanguageDisplay::Dialect,
                "standard" => DisplayNamesLanguageDisplay::Standard,
                _ => return invalid_intl_display_names_option(state, option),
            });
        }
    }
    Ok(())
}

fn invalid_intl_display_names_option<T>(
    state: &IntlDisplayNamesConstructorContinuation,
    option: IntlDisplayNamesOption,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        state.realm,
        state.origin.clone(),
        ExceptionKind::RangeError,
        &format!("invalid Intl.DisplayNames {} option", option.name()),
    )
}

fn finish_intl_display_names_options(
    runtime: &mut Runtime,
    state: IntlDisplayNamesConstructorContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    if state.options.name_type.is_none() {
        return intl_locale_list_error(
            state.realm,
            state.origin,
            ExceptionKind::TypeError,
            "Intl.DisplayNames requires a type option",
        );
    }
    let resolved = resolve_display_names(&state.requested_locales, state.options).map_err(
        |error| match error {
            DisplayNamesError::MissingType => NativeFailure::Abrupt(PendingException {
                realm: state.realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::TypeError,
                    message: JsString::from_utf8("Intl.DisplayNames requires a type option")
                        .expect("static Intl error is valid UTF-8"),
                },
                origin: state.origin.clone(),
            }),
            DisplayNamesError::InvalidLocale
            | DisplayNamesError::InvalidCode
            | DisplayNamesError::Data => EngineFault::RuntimeInvariant {
                message: "canonical DisplayNames inputs failed locale resolution",
            }
            .into(),
        },
    )?;
    let prototype = state.prototype.ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.DisplayNames allocation lost its prototype",
    })?;
    let object = runtime.allocate_intl_display_names(prototype, resolved)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn continue_intl_display_names_constructor_after(
    dispatch: NativeDispatch,
    state: IntlDisplayNamesConstructorContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        |state| NativeContinuation::IntlDisplayNamesConstructor(Box::new(state)),
        |state, value| {
            advance_intl_display_names_constructor(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.DisplayNames property Get produced a structured result",
    )
}

fn take_intl_display_names_constructor_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.DisplayNames constructor resumed without a completion",
    })
}

pub(super) fn begin_intl_display_names_supported_locales_of(
    runtime: &mut Runtime,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let locales = arguments.take_first_or_undefined();
    let options_argument = arguments.take_first_or_undefined();
    let state = IntlDisplayNamesSupportedLocalesContinuation {
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        realm,
        stage: IntlDisplayNamesSupportedLocalesStage::ReadLocaleMatcher,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::DisplayNamesSupportedLocalesOf(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_display_names_supported_locales_options(
    runtime: &mut Runtime,
    mut state: IntlDisplayNamesSupportedLocalesContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_display_names_supported_locales(runtime, &state);
    }
    let options_argument = state.options_argument.duplicate();
    state.options_object = Some(
        match to_object_value(runtime, state.realm, options_argument, state.origin.clone())? {
            Ok(options) => options,
            Err(exception) => return Err(NativeFailure::Abrupt(exception)),
        },
    );
    advance_intl_display_names_supported_locales(runtime, state, None, return_to, execution_budget)
}

pub(super) fn advance_intl_display_names_supported_locales(
    runtime: &mut Runtime,
    mut state: IntlDisplayNamesSupportedLocalesContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    match state.stage {
        IntlDisplayNamesSupportedLocalesStage::ReadLocaleMatcher => {
            let base = state
                .options_object
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.DisplayNames.supportedLocalesOf lost its options object",
                })?
                .duplicate();
            charge_heap_property_lookup(runtime, &base, execution_budget)?;
            let name = JsString::from_utf8("localeMatcher")?;
            let key = runtime.property_key_from_string(&name)?;
            state.stage = IntlDisplayNamesSupportedLocalesStage::AwaitLocaleMatcher;
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
            continue_intl_display_names_supported_locales_after(
                dispatch,
                state,
                runtime,
                return_to,
                execution_budget,
            )
        }
        IntlDisplayNamesSupportedLocalesStage::AwaitLocaleMatcher => {
            let value = take_intl_display_names_supported_locales_completion(&mut completion)?;
            if matches!(value, StoredValue::Undefined) {
                return finish_intl_display_names_supported_locales(runtime, &state);
            }
            if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                state.stage = IntlDisplayNamesSupportedLocalesStage::AwaitLocaleMatcherPrimitive;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::IntlDisplayNamesSupportedLocalesOf(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            validate_intl_display_names_locale_matcher(&state, value)?;
            finish_intl_display_names_supported_locales(runtime, &state)
        }
        IntlDisplayNamesSupportedLocalesStage::AwaitLocaleMatcherPrimitive => {
            let value = take_intl_display_names_supported_locales_completion(&mut completion)?;
            validate_intl_display_names_locale_matcher(&state, value)?;
            finish_intl_display_names_supported_locales(runtime, &state)
        }
    }
}

fn validate_intl_display_names_locale_matcher(
    state: &IntlDisplayNamesSupportedLocalesContinuation,
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
        "invalid Intl.DisplayNames localeMatcher option",
    )
}

fn finish_intl_display_names_supported_locales(
    runtime: &mut Runtime,
    state: &IntlDisplayNamesSupportedLocalesContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    intl_locale_string_array(
        runtime,
        state.realm,
        display_names_supported_locales(&state.requested_locales),
    )
}

fn continue_intl_display_names_supported_locales_after(
    dispatch: NativeDispatch,
    state: IntlDisplayNamesSupportedLocalesContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        |state| NativeContinuation::IntlDisplayNamesSupportedLocalesOf(Box::new(state)),
        |state, value| {
            advance_intl_display_names_supported_locales(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.DisplayNames.supportedLocalesOf Get produced a structured result",
    )
}

fn take_intl_display_names_supported_locales_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.DisplayNames.supportedLocalesOf resumed without a completion",
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch keeps receiver, arguments, realm, return target, origin, and budget explicit"
)]
pub(super) fn begin_intl_display_names_prototype(
    runtime: &mut Runtime,
    method: IntlDisplayNamesPrototypeMethod,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(display_names) = receiver else {
        return intl_display_names_brand_error(realm, origin);
    };
    let Some(resolved) = runtime.intl_display_names_state(*display_names)?.cloned() else {
        return intl_display_names_brand_error(realm, origin);
    };
    match method {
        IntlDisplayNamesPrototypeMethod::ResolvedOptions => {
            intl_display_names_resolved_options(runtime, realm, &resolved)
        }
        IntlDisplayNamesPrototypeMethod::Of => {
            let code = arguments.take_first_or_undefined();
            begin_intl_display_names_of(
                runtime,
                IntlDisplayNamesOfContinuation {
                    display_names: *display_names,
                    realm,
                    origin,
                },
                code,
                return_to,
                execution_budget,
            )
        }
    }
}

fn begin_intl_display_names_of(
    runtime: &mut Runtime,
    state: IntlDisplayNamesOfContinuation,
    code: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(code, StoredValue::Function(_) | StoredValue::Object(_)) {
        let realm = state.realm;
        let origin = state.origin.clone();
        return begin_operator_primitive_conversion(
            runtime,
            code,
            OperatorPrimitiveHint::String,
            OperatorPrimitiveTarget::IntlDisplayNamesOf(Box::new(state)),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    finish_intl_display_names_of_primitive(runtime, &state, code)
}

pub(super) fn finish_intl_display_names_of_primitive(
    runtime: &mut Runtime,
    state: &IntlDisplayNamesOfContinuation,
    code: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let code = operator_primitive_to_string(code, state.realm, &state.origin)?.to_utf8_lossy()?;
    let resolved = runtime
        .intl_display_names_state(state.display_names)?
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Intl.DisplayNames.of lost its branded receiver",
        })?;
    match display_name(resolved, &code) {
        Ok(Some(name)) => Ok(NativeDispatch::Immediate(StoredValue::String(
            JsString::from_utf8(&name)?,
        ))),
        Ok(None) => Ok(NativeDispatch::Immediate(StoredValue::Undefined)),
        Err(DisplayNamesError::InvalidCode) => intl_locale_list_error(
            state.realm,
            state.origin.clone(),
            ExceptionKind::RangeError,
            "invalid code for Intl.DisplayNames.of",
        ),
        Err(
            DisplayNamesError::InvalidLocale
            | DisplayNamesError::MissingType
            | DisplayNamesError::Data,
        ) => Err(EngineFault::RuntimeInvariant {
            message: "resolved Intl.DisplayNames slots failed display-name lookup",
        }
        .into()),
    }
}

fn intl_display_names_resolved_options(
    runtime: &mut Runtime,
    realm: RealmId,
    state: &DisplayNamesState,
) -> Result<NativeDispatch, NativeFailure> {
    let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    let mut properties = vec![
        (
            "locale",
            StoredValue::String(JsString::from_utf8(&state.locale)?),
        ),
        (
            "style",
            StoredValue::String(JsString::from_utf8(state.style.as_str())?),
        ),
        (
            "type",
            StoredValue::String(JsString::from_utf8(state.name_type.as_str())?),
        ),
        (
            "fallback",
            StoredValue::String(JsString::from_utf8(state.fallback.as_str())?),
        ),
    ];
    if matches!(state.name_type, DisplayNamesType::Language) {
        properties.push((
            "languageDisplay",
            StoredValue::String(JsString::from_utf8(state.language_display.as_str())?),
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

fn intl_display_names_brand_error<T>(
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        realm,
        origin,
        ExceptionKind::TypeError,
        "Intl.DisplayNames method called on incompatible receiver",
    )
}

pub(super) fn begin_intl_duration_format_constructor(
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
            "Intl.DurationFormat requires 'new'",
        );
    };
    let locales_argument = inputs.arguments.take_first_or_undefined();
    let options_argument = inputs.arguments.take_first_or_undefined();
    let state = IntlDurationFormatConstructorContinuation {
        new_target,
        format_value: None,
        locales_argument: Some(locales_argument),
        options_argument,
        options_object: None,
        prototype: None,
        requested_locales: Vec::new(),
        options: DurationFormatRequestOptions::default(),
        option_index: 0,
        realm,
        stage: IntlDurationFormatConstructorStage::AwaitPrototype,
        origin,
    };
    let base = StoredValue::Function(new_target);
    charge_heap_property_lookup(runtime, &base, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let dispatch = begin_value_get(
        runtime,
        &base,
        key,
        None,
        realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_intl_duration_format_constructor_after(
        dispatch,
        state,
        runtime,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch keeps the Temporal value, arguments, realm, return target, origin, and budget explicit"
)]
pub(super) fn begin_intl_duration_to_locale_string(
    runtime: &mut Runtime,
    duration: temporal_rs::Duration,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let locales = arguments.take_first_or_undefined();
    let options_argument = arguments.take_first_or_undefined();
    let state = IntlDurationFormatConstructorContinuation {
        new_target: runtime.realm_intl_duration_format_constructor(realm)?,
        format_value: Some(duration),
        locales_argument: None,
        options_argument,
        options_object: None,
        prototype: None,
        requested_locales: Vec::new(),
        options: DurationFormatRequestOptions::default(),
        option_index: 0,
        realm,
        stage: IntlDurationFormatConstructorStage::ReadOption,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::DurationFormatConstructor(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_duration_format_options(
    runtime: &mut Runtime,
    mut state: IntlDurationFormatConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_duration_format_options(runtime, &state);
    }
    if !matches!(
        state.options_argument,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        return intl_locale_list_error(
            state.realm,
            state.origin,
            ExceptionKind::TypeError,
            "Intl.DurationFormat options must be an object",
        );
    }
    state.options_object = Some(state.options_argument.duplicate());
    state.stage = IntlDurationFormatConstructorStage::ReadOption;
    advance_intl_duration_format_constructor(runtime, state, None, return_to, execution_budget)
}

#[allow(
    clippy::too_many_lines,
    reason = "DurationFormat option Gets and resumable conversions stay in normative order"
)]
pub(super) fn advance_intl_duration_format_constructor(
    runtime: &mut Runtime,
    mut state: IntlDurationFormatConstructorContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            IntlDurationFormatConstructorStage::AwaitPrototype => {
                let requested = take_intl_duration_format_constructor_completion(&mut completion)?;
                state.prototype = Some(match requested {
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
                            runtime.realm_intl_duration_format_prototype(target_realm)?,
                        )
                    }
                });
                let locales =
                    state
                        .locales_argument
                        .take()
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "Intl.DurationFormat constructor lost its locales argument",
                        })?;
                state.stage = IntlDurationFormatConstructorStage::ReadOption;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_intl_locale_list(
                    runtime,
                    locales,
                    IntlLocaleListTarget::DurationFormatConstructor(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            IntlDurationFormatConstructorStage::ReadOption => {
                let Some(option) = IntlDurationFormatOption::ALL
                    .get(state.option_index)
                    .copied()
                else {
                    return finish_intl_duration_format_options(runtime, &state);
                };
                let base = state
                    .options_object
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Intl.DurationFormat option iteration lost its options object",
                    })?
                    .duplicate();
                charge_heap_property_lookup(runtime, &base, execution_budget)?;
                let name = JsString::from_utf8(option.name())?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = IntlDurationFormatConstructorStage::AwaitOption;
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
                return continue_intl_duration_format_constructor_after(
                    dispatch,
                    state,
                    runtime,
                    return_to,
                    execution_budget,
                );
            }
            IntlDurationFormatConstructorStage::AwaitOption => {
                let value = take_intl_duration_format_constructor_completion(&mut completion)?;
                let option = IntlDurationFormatOption::ALL[state.option_index];
                if matches!(value, StoredValue::Undefined) {
                    advance_intl_duration_format_option(&mut state);
                    continue;
                }
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    state.stage = IntlDurationFormatConstructorStage::AwaitOptionPrimitive;
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        if option.is_number() {
                            OperatorPrimitiveHint::Number
                        } else {
                            OperatorPrimitiveHint::String
                        },
                        OperatorPrimitiveTarget::IntlDurationFormatConstructor(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                store_intl_duration_format_option(&mut state, option, value)?;
                advance_intl_duration_format_option(&mut state);
            }
            IntlDurationFormatConstructorStage::AwaitOptionPrimitive => {
                let value = take_intl_duration_format_constructor_completion(&mut completion)?;
                let option = IntlDurationFormatOption::ALL[state.option_index];
                store_intl_duration_format_option(&mut state, option, value)?;
                advance_intl_duration_format_option(&mut state);
            }
        }
    }
}

fn advance_intl_duration_format_option(state: &mut IntlDurationFormatConstructorContinuation) {
    state.option_index = state.option_index.saturating_add(1);
    state.stage = IntlDurationFormatConstructorStage::ReadOption;
}

fn store_intl_duration_format_option(
    state: &mut IntlDurationFormatConstructorContinuation,
    option: IntlDurationFormatOption,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    if option == IntlDurationFormatOption::FractionalDigits {
        let number = operator_to_number(value, state.realm, &state.origin)?.as_f64();
        if !number.is_finite() || !(0.0..=9.0).contains(&number) {
            return invalid_intl_duration_format_option(state, option);
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the finite option is bounded to the inclusive u8 range before flooring"
        )]
        {
            state.options.fractional_digits = Some(number.floor() as u8);
        }
        return Ok(());
    }

    let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let text = text.to_utf8_lossy()?;
    match option {
        IntlDurationFormatOption::LocaleMatcher => {
            if !matches!(text.as_str(), "lookup" | "best fit") {
                return invalid_intl_duration_format_option(state, option);
            }
        }
        IntlDurationFormatOption::NumberingSystem => {
            let Some(value) = canonical_unicode_locale_type(&text) else {
                return invalid_intl_duration_format_option(state, option);
            };
            state.options.numbering_system = Some(value);
        }
        IntlDurationFormatOption::Style => {
            state.options.style = Some(match text.as_str() {
                "long" => DurationFormatStyle::Long,
                "short" => DurationFormatStyle::Short,
                "narrow" => DurationFormatStyle::Narrow,
                "digital" => DurationFormatStyle::Digital,
                _ => return invalid_intl_duration_format_option(state, option),
            });
        }
        IntlDurationFormatOption::UnitStyle(unit) => {
            let style = match text.as_str() {
                "long" => DurationUnitStyle::Long,
                "short" => DurationUnitStyle::Short,
                "narrow" => DurationUnitStyle::Narrow,
                "numeric"
                    if !matches!(
                        unit,
                        DurationUnit::Years
                            | DurationUnit::Months
                            | DurationUnit::Weeks
                            | DurationUnit::Days
                    ) =>
                {
                    DurationUnitStyle::Numeric
                }
                "2-digit"
                    if matches!(
                        unit,
                        DurationUnit::Hours | DurationUnit::Minutes | DurationUnit::Seconds
                    ) =>
                {
                    DurationUnitStyle::TwoDigit
                }
                _ => return invalid_intl_duration_format_option(state, option),
            };
            state.options.unit_styles[unit.index()] = Some(style);
        }
        IntlDurationFormatOption::UnitDisplay(unit) => {
            state.options.unit_displays[unit.index()] = Some(match text.as_str() {
                "auto" => DurationDisplay::Auto,
                "always" => DurationDisplay::Always,
                _ => return invalid_intl_duration_format_option(state, option),
            });
        }
        IntlDurationFormatOption::FractionalDigits => {
            return Err(EngineFault::RuntimeInvariant {
                message: "numeric Intl.DurationFormat option reached string storage",
            }
            .into());
        }
    }
    Ok(())
}

fn invalid_intl_duration_format_option<T>(
    state: &IntlDurationFormatConstructorContinuation,
    option: IntlDurationFormatOption,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        state.realm,
        state.origin.clone(),
        ExceptionKind::RangeError,
        &format!("invalid Intl.DurationFormat {} option", option.name()),
    )
}

fn finish_intl_duration_format_options(
    runtime: &mut Runtime,
    state: &IntlDurationFormatConstructorContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    let resolved =
        resolve_duration_format(&state.requested_locales, &state.options).map_err(|error| {
            match error {
                DurationFormatError::InvalidOption => NativeFailure::Abrupt(PendingException {
                    realm: state.realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::RangeError,
                        message: JsString::from_utf8("invalid Intl.DurationFormat options")
                            .expect("static Intl error is valid UTF-8"),
                    },
                    origin: state.origin.clone(),
                }),
                DurationFormatError::InvalidLocale
                | DurationFormatError::InvalidDuration
                | DurationFormatError::Data => EngineFault::RuntimeInvariant {
                    message: "canonical DurationFormat inputs failed locale resolution",
                }
                .into(),
            }
        })?;
    if let Some(duration) = state.format_value {
        let formatted = format_duration(&resolved, duration_record_from_temporal(duration))
            .map_err(|error| match error {
                DurationFormatError::InvalidDuration => NativeFailure::Abrupt(PendingException {
                    realm: state.realm,
                    payload: PendingExceptionPayload::EngineError {
                        kind: ExceptionKind::RangeError,
                        message: JsString::from_utf8("invalid duration record")
                            .expect("static Intl error is valid UTF-8"),
                    },
                    origin: state.origin.clone(),
                }),
                DurationFormatError::InvalidLocale
                | DurationFormatError::InvalidOption
                | DurationFormatError::Data => EngineFault::RuntimeInvariant {
                    message: "resolved Temporal.Duration locale-string slots failed ICU formatting",
                }
                .into(),
            })?;
        return Ok(NativeDispatch::Immediate(StoredValue::String(
            JsString::from_utf8(&formatted)?,
        )));
    }
    let prototype = state.prototype.ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.DurationFormat allocation lost its prototype",
    })?;
    let object = runtime.allocate_intl_duration_format(prototype, resolved)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn continue_intl_duration_format_constructor_after(
    dispatch: NativeDispatch,
    state: IntlDurationFormatConstructorContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        |state| NativeContinuation::IntlDurationFormatConstructor(Box::new(state)),
        |state, value| {
            advance_intl_duration_format_constructor(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.DurationFormat property Get produced a structured result",
    )
}

fn take_intl_duration_format_constructor_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.DurationFormat constructor resumed without a completion",
    })
}

pub(super) fn begin_intl_duration_format_supported_locales_of(
    runtime: &mut Runtime,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let locales = arguments.take_first_or_undefined();
    let options_argument = arguments.take_first_or_undefined();
    let state = IntlDurationFormatSupportedLocalesContinuation {
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        realm,
        stage: IntlDurationFormatSupportedLocalesStage::ReadLocaleMatcher,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::DurationFormatSupportedLocalesOf(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_duration_format_supported_locales_options(
    runtime: &mut Runtime,
    mut state: IntlDurationFormatSupportedLocalesContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_duration_format_supported_locales(runtime, &state);
    }
    if !matches!(
        state.options_argument,
        StoredValue::Function(_) | StoredValue::Object(_)
    ) {
        return intl_locale_list_error(
            state.realm,
            state.origin,
            ExceptionKind::TypeError,
            "Intl.DurationFormat.supportedLocalesOf options must be an object",
        );
    }
    state.options_object = Some(state.options_argument.duplicate());
    advance_intl_duration_format_supported_locales(
        runtime,
        state,
        None,
        return_to,
        execution_budget,
    )
}

pub(super) fn advance_intl_duration_format_supported_locales(
    runtime: &mut Runtime,
    mut state: IntlDurationFormatSupportedLocalesContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        IntlDurationFormatSupportedLocalesStage::ReadLocaleMatcher => {
            let base = state
                .options_object
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.DurationFormat.supportedLocalesOf lost its options object",
                })?
                .duplicate();
            charge_heap_property_lookup(runtime, &base, execution_budget)?;
            let name = JsString::from_utf8("localeMatcher")?;
            let key = runtime.property_key_from_string(&name)?;
            state.stage = IntlDurationFormatSupportedLocalesStage::AwaitLocaleMatcher;
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
            continue_intl_duration_format_supported_locales_after(
                dispatch,
                state,
                runtime,
                return_to,
                execution_budget,
            )
        }
        IntlDurationFormatSupportedLocalesStage::AwaitLocaleMatcher => {
            let value = completion.ok_or(EngineFault::RuntimeInvariant {
                message: "Intl.DurationFormat.supportedLocalesOf resumed without a completion",
            })?;
            if matches!(value, StoredValue::Undefined) {
                return finish_intl_duration_format_supported_locales(runtime, &state);
            }
            if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                state.stage = IntlDurationFormatSupportedLocalesStage::AwaitLocaleMatcherPrimitive;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::IntlDurationFormatSupportedLocalesOf(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            validate_intl_duration_format_locale_matcher(&state, value)?;
            finish_intl_duration_format_supported_locales(runtime, &state)
        }
        IntlDurationFormatSupportedLocalesStage::AwaitLocaleMatcherPrimitive => {
            let value = completion.ok_or(EngineFault::RuntimeInvariant {
                message: "Intl.DurationFormat.supportedLocalesOf resumed without a completion",
            })?;
            validate_intl_duration_format_locale_matcher(&state, value)?;
            finish_intl_duration_format_supported_locales(runtime, &state)
        }
    }
}

fn validate_intl_duration_format_locale_matcher(
    state: &IntlDurationFormatSupportedLocalesContinuation,
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
        "invalid Intl.DurationFormat localeMatcher option",
    )
}

fn finish_intl_duration_format_supported_locales(
    runtime: &mut Runtime,
    state: &IntlDurationFormatSupportedLocalesContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    intl_locale_string_array(
        runtime,
        state.realm,
        duration_format_supported_locales(&state.requested_locales),
    )
}

fn continue_intl_duration_format_supported_locales_after(
    dispatch: NativeDispatch,
    state: IntlDurationFormatSupportedLocalesContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        |state| NativeContinuation::IntlDurationFormatSupportedLocalesOf(Box::new(state)),
        |state, value| {
            advance_intl_duration_format_supported_locales(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.DurationFormat.supportedLocalesOf Get produced a structured result",
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch keeps receiver, arguments, realm, return target, origin, and budget explicit"
)]
pub(super) fn begin_intl_duration_format_prototype(
    runtime: &mut Runtime,
    method: IntlDurationFormatPrototypeMethod,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(formatter) = receiver else {
        return intl_duration_format_brand_error(realm, origin);
    };
    let Some(resolved) = runtime.intl_duration_format_state(*formatter)?.cloned() else {
        return intl_duration_format_brand_error(realm, origin);
    };
    match method {
        IntlDurationFormatPrototypeMethod::ResolvedOptions => {
            intl_duration_format_resolved_options(runtime, realm, &resolved)
        }
        IntlDurationFormatPrototypeMethod::Format
        | IntlDurationFormatPrototypeMethod::FormatToParts => {
            let input = arguments.take_first_or_undefined();
            begin_intl_duration_format_value(
                runtime,
                IntlDurationFormatValueContinuation {
                    formatter: *formatter,
                    input: input.duplicate(),
                    values: [0; 10],
                    found: false,
                    unit_index: 0,
                    operation: if method == IntlDurationFormatPrototypeMethod::Format {
                        IntlDurationFormatOperation::Format
                    } else {
                        IntlDurationFormatOperation::FormatToParts
                    },
                    realm,
                    stage: IntlDurationFormatValueStage::ReadProperty,
                    origin,
                },
                input,
                return_to,
                execution_budget,
            )
        }
    }
}

fn begin_intl_duration_format_value(
    runtime: &mut Runtime,
    state: IntlDurationFormatValueContinuation,
    input: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(object) = input
        && let Some(duration) = runtime.temporal_duration(object)?
    {
        return finish_intl_duration_format_record(
            runtime,
            &state,
            duration_record_from_temporal(duration),
        );
    }
    if let StoredValue::String(input) = input {
        let input = input.to_utf8_lossy()?;
        let duration = input.parse::<temporal_rs::Duration>().map_err(|_| {
            NativeFailure::Abrupt(PendingException {
                realm: state.realm,
                payload: PendingExceptionPayload::EngineError {
                    kind: ExceptionKind::RangeError,
                    message: JsString::from_utf8("invalid Temporal duration string")
                        .expect("static Intl error is valid UTF-8"),
                },
                origin: state.origin.clone(),
            })
        })?;
        return finish_intl_duration_format_record(
            runtime,
            &state,
            duration_record_from_temporal(duration),
        );
    }
    if !matches!(input, StoredValue::Function(_) | StoredValue::Object(_)) {
        return intl_duration_format_input_type_error(&state);
    }
    advance_intl_duration_format_value(runtime, state, None, return_to, execution_budget)
}

pub(super) fn advance_intl_duration_format_value(
    runtime: &mut Runtime,
    mut state: IntlDurationFormatValueContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            IntlDurationFormatValueStage::ReadProperty => {
                let Some(unit) = INTL_DURATION_RECORD_PROPERTY_ORDER
                    .get(state.unit_index)
                    .copied()
                else {
                    if !state.found {
                        return intl_duration_format_input_type_error(&state);
                    }
                    return finish_intl_duration_format_record(
                        runtime,
                        &state,
                        DurationRecord {
                            values: state.values,
                        },
                    );
                };
                let base = state.input.duplicate();
                charge_heap_property_lookup(runtime, &base, execution_budget)?;
                let name = JsString::from_utf8(unit.plural_name())?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = IntlDurationFormatValueStage::AwaitProperty;
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
                return continue_intl_duration_format_value_after(
                    dispatch,
                    state,
                    runtime,
                    return_to,
                    execution_budget,
                );
            }
            IntlDurationFormatValueStage::AwaitProperty => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.DurationFormat duration Get resumed without a completion",
                })?;
                if matches!(value, StoredValue::Undefined) {
                    advance_intl_duration_format_value_property(&mut state);
                    continue;
                }
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    state.stage = IntlDurationFormatValueStage::AwaitPropertyPrimitive;
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::Number,
                        OperatorPrimitiveTarget::IntlDurationFormatValue(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                store_intl_duration_format_value(&mut state, value)?;
                advance_intl_duration_format_value_property(&mut state);
            }
            IntlDurationFormatValueStage::AwaitPropertyPrimitive => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.DurationFormat duration conversion resumed without a value",
                })?;
                store_intl_duration_format_value(&mut state, value)?;
                advance_intl_duration_format_value_property(&mut state);
            }
        }
    }
}

fn advance_intl_duration_format_value_property(state: &mut IntlDurationFormatValueContinuation) {
    state.unit_index = state.unit_index.saturating_add(1);
    state.stage = IntlDurationFormatValueStage::ReadProperty;
}

fn store_intl_duration_format_value(
    state: &mut IntlDurationFormatValueContinuation,
    value: StoredValue,
) -> Result<(), NativeFailure> {
    let number = operator_to_number(value, state.realm, &state.origin)?.as_f64();
    let Some(value) = exact_integral_f64(number) else {
        return intl_duration_format_input_range_error(state);
    };
    let unit = INTL_DURATION_RECORD_PROPERTY_ORDER
        .get(state.unit_index)
        .copied()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Intl.DurationFormat duration unit index is out of range",
        })?;
    let slot = state
        .values
        .get_mut(unit.index())
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Intl.DurationFormat duration unit index is out of range",
        })?;
    *slot = value;
    state.found = true;
    Ok(())
}

fn exact_integral_f64(value: f64) -> Option<i128> {
    if !value.is_finite() || value.fract() != 0.0 {
        return None;
    }
    if value == 0.0 {
        return Some(0);
    }
    let bits = value.to_bits();
    let negative = bits >> 63 != 0;
    let exponent = i32::from(((bits >> 52) & 0x7ff) as u16) - 1023;
    if !(0..=126).contains(&exponent) {
        return None;
    }
    let significand = u128::from((bits & ((1_u64 << 52) - 1)) | (1_u64 << 52));
    let magnitude = if exponent >= 52 {
        significand.checked_shl((exponent - 52).cast_unsigned())?
    } else {
        let shift = (52 - exponent).cast_unsigned();
        let discarded_mask = (1_u128 << shift) - 1;
        if significand & discarded_mask != 0 {
            return None;
        }
        significand >> shift
    };
    let magnitude = i128::try_from(magnitude).ok()?;
    Some(if negative { -magnitude } else { magnitude })
}

fn duration_record_from_temporal(duration: temporal_rs::Duration) -> DurationRecord {
    DurationRecord {
        values: [
            i128::from(duration.years()),
            i128::from(duration.months()),
            i128::from(duration.weeks()),
            i128::from(duration.days()),
            i128::from(duration.hours()),
            i128::from(duration.minutes()),
            i128::from(duration.seconds()),
            i128::from(duration.milliseconds()),
            duration.microseconds(),
            duration.nanoseconds(),
        ],
    }
}

fn finish_intl_duration_format_record(
    runtime: &mut Runtime,
    state: &IntlDurationFormatValueContinuation,
    record: DurationRecord,
) -> Result<NativeDispatch, NativeFailure> {
    let resolved = runtime
        .intl_duration_format_state(state.formatter)?
        .cloned()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Intl.DurationFormat operation lost its branded receiver",
        })?;
    match state.operation {
        IntlDurationFormatOperation::Format => {
            let formatted = format_duration(&resolved, record)
                .map_err(|error| intl_duration_format_operation_error(state, error))?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&formatted)?,
            )))
        }
        IntlDurationFormatOperation::FormatToParts => {
            let parts = format_duration_to_parts(&resolved, record)
                .map_err(|error| intl_duration_format_operation_error(state, error))?;
            intl_duration_format_parts_array(runtime, state.realm, parts)
        }
    }
}

fn intl_duration_format_operation_error(
    state: &IntlDurationFormatValueContinuation,
    error: DurationFormatError,
) -> NativeFailure {
    match error {
        DurationFormatError::InvalidDuration => NativeFailure::Abrupt(PendingException {
            realm: state.realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("invalid duration record")
                    .expect("static Intl error is valid UTF-8"),
            },
            origin: state.origin.clone(),
        }),
        DurationFormatError::InvalidLocale
        | DurationFormatError::InvalidOption
        | DurationFormatError::Data => EngineFault::RuntimeInvariant {
            message: "resolved Intl.DurationFormat slots failed ICU formatting",
        }
        .into(),
    }
}

fn intl_duration_format_input_type_error<T>(
    state: &IntlDurationFormatValueContinuation,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        state.realm,
        state.origin.clone(),
        ExceptionKind::TypeError,
        "Intl.DurationFormat requires a duration-like value",
    )
}

fn intl_duration_format_input_range_error<T>(
    state: &IntlDurationFormatValueContinuation,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        state.realm,
        state.origin.clone(),
        ExceptionKind::RangeError,
        "Intl.DurationFormat duration fields must be finite integers",
    )
}

fn continue_intl_duration_format_value_after(
    dispatch: NativeDispatch,
    state: IntlDurationFormatValueContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        |state| NativeContinuation::IntlDurationFormatValue(Box::new(state)),
        |state, value| {
            advance_intl_duration_format_value(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.DurationFormat duration Get produced a structured result",
    )
}

fn intl_duration_format_parts_array(
    runtime: &mut Runtime,
    realm: RealmId,
    parts: Vec<quickjs_intl::DurationFormatPart>,
) -> Result<NativeDispatch, NativeFailure> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(parts.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: parts.len(),
        })?;
    for part in parts {
        let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
        let mut properties = vec![
            ("type", StoredValue::String(JsString::from_utf8(part.kind)?)),
            (
                "value",
                StoredValue::String(JsString::from_utf8(&part.value)?),
            ),
        ];
        if let Some(unit) = part.unit {
            properties.push(("unit", StoredValue::String(JsString::from_utf8(unit)?)));
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

fn intl_duration_format_resolved_options(
    runtime: &mut Runtime,
    realm: RealmId,
    state: &DurationFormatState,
) -> Result<NativeDispatch, NativeFailure> {
    let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    for (name, value) in [
        ("locale", state.locale.as_str()),
        ("numberingSystem", state.numbering_system.as_str()),
        ("style", state.style.as_str()),
    ] {
        append_intl_duration_format_resolved_property(runtime, object, name, value)?;
    }
    for unit in DurationUnit::ALL {
        let options = state.unit(unit);
        append_intl_duration_format_resolved_property(
            runtime,
            object,
            unit.plural_name(),
            options.style.as_str(),
        )?;
        append_intl_duration_format_resolved_property(
            runtime,
            object,
            unit.display_name(),
            options.display.as_str(),
        )?;
    }
    if let Some(digits) = state.fractional_digits {
        let name = JsString::from_utf8("fractionalDigits")?;
        let key = runtime.property_key_from_string(&name)?;
        runtime.append_data_property(
            HeapReference::Object(object),
            key,
            PropertyLayout::data(true, true, true),
            StoredValue::Number(JsNumber::from_f64(f64::from(digits))),
        )?;
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn append_intl_duration_format_resolved_property(
    runtime: &mut Runtime,
    object: ObjectId,
    name: &str,
    value: &str,
) -> Result<(), NativeFailure> {
    let name = JsString::from_utf8(name)?;
    let key = runtime.property_key_from_string(&name)?;
    runtime.append_data_property(
        HeapReference::Object(object),
        key,
        PropertyLayout::data(true, true, true),
        StoredValue::String(JsString::from_utf8(value)?),
    )?;
    Ok(())
}

fn intl_duration_format_brand_error<T>(
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        realm,
        origin,
        ExceptionKind::TypeError,
        "Intl.DurationFormat method called on incompatible receiver",
    )
}

pub(super) fn begin_intl_segmenter_constructor(
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
            "Intl.Segmenter requires 'new'",
        );
    };
    let locales_argument = inputs.arguments.take_first_or_undefined();
    let options_argument = inputs.arguments.take_first_or_undefined();
    let state = IntlSegmenterConstructorContinuation {
        new_target,
        locales_argument: Some(locales_argument),
        options_argument,
        options_object: None,
        prototype: None,
        requested_locales: Vec::new(),
        options: SegmenterRequestOptions::default(),
        option_index: 0,
        realm,
        stage: IntlSegmenterConstructorStage::AwaitPrototype,
        origin,
    };
    let base = StoredValue::Function(new_target);
    charge_heap_property_lookup(runtime, &base, execution_budget)?;
    let key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let dispatch = begin_value_get(
        runtime,
        &base,
        key,
        None,
        realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_intl_segmenter_constructor_after(dispatch, state, runtime, return_to, execution_budget)
}

fn begin_intl_segmenter_options(
    runtime: &mut Runtime,
    mut state: IntlSegmenterConstructorContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_segmenter_options(runtime, &state);
    }
    let options_argument = state.options_argument.duplicate();
    state.options_object = Some(
        match to_object_value(runtime, state.realm, options_argument, state.origin.clone())? {
            Ok(options) => options,
            Err(exception) => return Err(NativeFailure::Abrupt(exception)),
        },
    );
    state.stage = IntlSegmenterConstructorStage::ReadOption;
    advance_intl_segmenter_constructor(runtime, state, None, return_to, execution_budget)
}

pub(super) fn advance_intl_segmenter_constructor(
    runtime: &mut Runtime,
    mut state: IntlSegmenterConstructorContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            IntlSegmenterConstructorStage::AwaitPrototype => {
                return finish_intl_segmenter_prototype_lookup(
                    runtime,
                    state,
                    &mut completion,
                    return_to,
                    execution_budget,
                );
            }
            IntlSegmenterConstructorStage::ReadOption => {
                let Some(option) = IntlSegmenterOption::ALL.get(state.option_index).copied() else {
                    return finish_intl_segmenter_options(runtime, &state);
                };
                let base = state
                    .options_object
                    .as_ref()
                    .ok_or(EngineFault::RuntimeInvariant {
                        message: "Intl.Segmenter option iteration lost its options object",
                    })?
                    .duplicate();
                charge_heap_property_lookup(runtime, &base, execution_budget)?;
                let name = JsString::from_utf8(option.name())?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = IntlSegmenterConstructorStage::AwaitOption;
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
                return continue_intl_segmenter_constructor_after(
                    dispatch,
                    state,
                    runtime,
                    return_to,
                    execution_budget,
                );
            }
            IntlSegmenterConstructorStage::AwaitOption => {
                let value = take_intl_segmenter_constructor_completion(&mut completion)?;
                let option = IntlSegmenterOption::ALL[state.option_index];
                if matches!(value, StoredValue::Undefined) {
                    advance_intl_segmenter_option(&mut state);
                    continue;
                }
                if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                    state.stage = IntlSegmenterConstructorStage::AwaitOptionPrimitive;
                    let realm = state.realm;
                    let origin = state.origin.clone();
                    return begin_operator_primitive_conversion(
                        runtime,
                        value,
                        OperatorPrimitiveHint::String,
                        OperatorPrimitiveTarget::IntlSegmenterConstructor(Box::new(state)),
                        realm,
                        return_to,
                        origin,
                        execution_budget,
                    );
                }
                let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
                store_intl_segmenter_option(&mut state, option, &text)?;
                advance_intl_segmenter_option(&mut state);
            }
            IntlSegmenterConstructorStage::AwaitOptionPrimitive => {
                let value = take_intl_segmenter_constructor_completion(&mut completion)?;
                let option = IntlSegmenterOption::ALL[state.option_index];
                let text = operator_primitive_to_string(value, state.realm, &state.origin)?;
                store_intl_segmenter_option(&mut state, option, &text)?;
                advance_intl_segmenter_option(&mut state);
            }
        }
    }
}

fn finish_intl_segmenter_prototype_lookup(
    runtime: &mut Runtime,
    mut state: IntlSegmenterConstructorContinuation,
    completion: &mut Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let requested = take_intl_segmenter_constructor_completion(completion)?;
    state.prototype = Some(match requested {
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
            HeapReference::Object(runtime.realm_intl_segmenter_prototype(target_realm)?)
        }
    });
    let locales = state
        .locales_argument
        .take()
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Intl.Segmenter constructor lost its locales argument",
        })?;
    state.stage = IntlSegmenterConstructorStage::ReadOption;
    let realm = state.realm;
    let origin = state.origin.clone();
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::SegmenterConstructor(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn advance_intl_segmenter_option(state: &mut IntlSegmenterConstructorContinuation) {
    state.option_index = state.option_index.saturating_add(1);
    state.stage = IntlSegmenterConstructorStage::ReadOption;
}

fn store_intl_segmenter_option(
    state: &mut IntlSegmenterConstructorContinuation,
    option: IntlSegmenterOption,
    text: &JsString,
) -> Result<(), NativeFailure> {
    let value = text.to_utf8_lossy()?;
    match option {
        IntlSegmenterOption::LocaleMatcher => {
            if !matches!(value.as_str(), "lookup" | "best fit") {
                return invalid_intl_segmenter_option(state, option);
            }
        }
        IntlSegmenterOption::Granularity => {
            state.options.granularity = Some(match value.as_str() {
                "grapheme" => SegmenterGranularity::Grapheme,
                "word" => SegmenterGranularity::Word,
                "sentence" => SegmenterGranularity::Sentence,
                _ => return invalid_intl_segmenter_option(state, option),
            });
        }
    }
    Ok(())
}

fn invalid_intl_segmenter_option<T>(
    state: &IntlSegmenterConstructorContinuation,
    option: IntlSegmenterOption,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        state.realm,
        state.origin.clone(),
        ExceptionKind::RangeError,
        &format!("invalid Intl.Segmenter {} option", option.name()),
    )
}

fn finish_intl_segmenter_options(
    runtime: &mut Runtime,
    state: &IntlSegmenterConstructorContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    let resolved =
        resolve_segmenter(&state.requested_locales, state.options).map_err(
            |error| match error {
                SegmenterError::InvalidLocale | SegmenterError::Data => {
                    EngineFault::RuntimeInvariant {
                        message: "canonical Segmenter inputs failed locale resolution",
                    }
                }
            },
        )?;
    let prototype = state.prototype.ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.Segmenter allocation lost its prototype",
    })?;
    let object = runtime.allocate_intl_segmenter(prototype, resolved)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn continue_intl_segmenter_constructor_after(
    dispatch: NativeDispatch,
    state: IntlSegmenterConstructorContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        |state| NativeContinuation::IntlSegmenterConstructor(Box::new(state)),
        |state, value| {
            advance_intl_segmenter_constructor(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.Segmenter property Get produced a structured result",
    )
}

fn take_intl_segmenter_constructor_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.Segmenter constructor resumed without a completion",
    })
}

pub(super) fn begin_intl_segmenter_supported_locales_of(
    runtime: &mut Runtime,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let locales = arguments.take_first_or_undefined();
    let options_argument = arguments.take_first_or_undefined();
    let state = IntlSegmenterSupportedLocalesContinuation {
        options_argument,
        options_object: None,
        requested_locales: Vec::new(),
        realm,
        stage: IntlSegmenterSupportedLocalesStage::ReadLocaleMatcher,
        origin: origin.clone(),
    };
    begin_intl_locale_list(
        runtime,
        locales,
        IntlLocaleListTarget::SegmenterSupportedLocalesOf(Box::new(state)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

fn begin_intl_segmenter_supported_locales_options(
    runtime: &mut Runtime,
    mut state: IntlSegmenterSupportedLocalesContinuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(state.options_argument, StoredValue::Undefined) {
        return finish_intl_segmenter_supported_locales(runtime, &state);
    }
    let options_argument = state.options_argument.duplicate();
    state.options_object = Some(
        match to_object_value(runtime, state.realm, options_argument, state.origin.clone())? {
            Ok(options) => options,
            Err(exception) => return Err(NativeFailure::Abrupt(exception)),
        },
    );
    advance_intl_segmenter_supported_locales(runtime, state, None, return_to, execution_budget)
}

pub(super) fn advance_intl_segmenter_supported_locales(
    runtime: &mut Runtime,
    mut state: IntlSegmenterSupportedLocalesContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    match state.stage {
        IntlSegmenterSupportedLocalesStage::ReadLocaleMatcher => {
            let base = state
                .options_object
                .as_ref()
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl.Segmenter.supportedLocalesOf lost its options object",
                })?
                .duplicate();
            charge_heap_property_lookup(runtime, &base, execution_budget)?;
            let name = JsString::from_utf8("localeMatcher")?;
            let key = runtime.property_key_from_string(&name)?;
            state.stage = IntlSegmenterSupportedLocalesStage::AwaitLocaleMatcher;
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
            continue_intl_segmenter_supported_locales_after(
                dispatch,
                state,
                runtime,
                return_to,
                execution_budget,
            )
        }
        IntlSegmenterSupportedLocalesStage::AwaitLocaleMatcher => {
            let value = take_intl_segmenter_supported_locales_completion(&mut completion)?;
            if matches!(value, StoredValue::Undefined) {
                return finish_intl_segmenter_supported_locales(runtime, &state);
            }
            if matches!(value, StoredValue::Function(_) | StoredValue::Object(_)) {
                state.stage = IntlSegmenterSupportedLocalesStage::AwaitLocaleMatcherPrimitive;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::IntlSegmenterSupportedLocalesOf(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            validate_intl_segmenter_locale_matcher(&state, value)?;
            finish_intl_segmenter_supported_locales(runtime, &state)
        }
        IntlSegmenterSupportedLocalesStage::AwaitLocaleMatcherPrimitive => {
            let value = take_intl_segmenter_supported_locales_completion(&mut completion)?;
            validate_intl_segmenter_locale_matcher(&state, value)?;
            finish_intl_segmenter_supported_locales(runtime, &state)
        }
    }
}

fn validate_intl_segmenter_locale_matcher(
    state: &IntlSegmenterSupportedLocalesContinuation,
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
        "invalid Intl.Segmenter localeMatcher option",
    )
}

fn finish_intl_segmenter_supported_locales(
    runtime: &mut Runtime,
    state: &IntlSegmenterSupportedLocalesContinuation,
) -> Result<NativeDispatch, NativeFailure> {
    intl_locale_string_array(
        runtime,
        state.realm,
        segmenter_supported_locales(&state.requested_locales),
    )
}

fn continue_intl_segmenter_supported_locales_after(
    dispatch: NativeDispatch,
    state: IntlSegmenterSupportedLocalesContinuation,
    runtime: &mut Runtime,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    continue_get_after(
        dispatch,
        state,
        |state| NativeContinuation::IntlSegmenterSupportedLocalesOf(Box::new(state)),
        |state, value| {
            advance_intl_segmenter_supported_locales(
                runtime,
                state,
                Some(value),
                return_to,
                execution_budget,
            )
        },
        "Intl.Segmenter.supportedLocalesOf Get produced a structured result",
    )
}

fn take_intl_segmenter_supported_locales_completion(
    completion: &mut Option<StoredValue>,
) -> Result<StoredValue, EngineFault> {
    completion.take().ok_or(EngineFault::RuntimeInvariant {
        message: "Intl.Segmenter.supportedLocalesOf resumed without a completion",
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch keeps receiver, arguments, realm, return target, origin, and budget explicit"
)]
pub(super) fn begin_intl_segmenter_prototype(
    runtime: &mut Runtime,
    method: IntlSegmenterPrototypeMethod,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(segmenter) = receiver else {
        return intl_segmenter_brand_error(realm, origin);
    };
    let Some(resolved) = runtime.intl_segmenter_state(*segmenter)?.cloned() else {
        return intl_segmenter_brand_error(realm, origin);
    };
    match method {
        IntlSegmenterPrototypeMethod::ResolvedOptions => {
            intl_segmenter_resolved_options(runtime, realm, &resolved)
        }
        IntlSegmenterPrototypeMethod::Segment => {
            let input = arguments.take_first_or_undefined();
            begin_intl_segmenter_segment(
                runtime,
                IntlSegmenterSegmentContinuation {
                    segmenter: *segmenter,
                    realm,
                    origin,
                },
                input,
                return_to,
                execution_budget,
            )
        }
    }
}

fn begin_intl_segmenter_segment(
    runtime: &mut Runtime,
    state: IntlSegmenterSegmentContinuation,
    input: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(input, StoredValue::Function(_) | StoredValue::Object(_)) {
        let realm = state.realm;
        let origin = state.origin.clone();
        return begin_operator_primitive_conversion(
            runtime,
            input,
            OperatorPrimitiveHint::String,
            OperatorPrimitiveTarget::IntlSegmenterSegment(Box::new(state)),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    finish_intl_segmenter_segment_primitive(runtime, &state, input)
}

pub(super) fn finish_intl_segmenter_segment_primitive(
    runtime: &mut Runtime,
    state: &IntlSegmenterSegmentContinuation,
    input: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let input = operator_primitive_to_string(input, state.realm, &state.origin)?;
    let resolved =
        runtime
            .intl_segmenter_state(state.segmenter)?
            .ok_or(EngineFault::RuntimeInvariant {
                message: "Intl.Segmenter.segment lost its branded receiver",
            })?;
    let code_units = input.code_units().collect::<Vec<_>>();
    let boundaries = segment_boundaries(resolved, &code_units).map_err(|error| match error {
        SegmenterError::InvalidLocale | SegmenterError::Data => EngineFault::RuntimeInvariant {
            message: "resolved Intl.Segmenter slots failed segmentation",
        },
    })?;
    let prototype = runtime.realm_intl_segments_prototype(state.realm)?;
    let object = runtime.allocate_intl_segments(prototype, state.segmenter, input, boundaries)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn intl_segmenter_resolved_options(
    runtime: &mut Runtime,
    realm: RealmId,
    state: &SegmenterState,
) -> Result<NativeDispatch, NativeFailure> {
    let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    for (name, value) in [
        ("locale", state.locale.as_str()),
        ("granularity", state.granularity.as_str()),
    ] {
        let name = JsString::from_utf8(name)?;
        let key = runtime.property_key_from_string(&name)?;
        runtime.append_data_property(
            HeapReference::Object(object),
            key,
            PropertyLayout::data(true, true, true),
            StoredValue::String(JsString::from_utf8(value)?),
        )?;
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

pub(super) fn begin_intl_segments_containing(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(segments) = receiver else {
        return intl_segments_brand_error(realm, origin);
    };
    if runtime.intl_segments_state(*segments)?.is_none() {
        return intl_segments_brand_error(realm, origin);
    }
    let index = arguments.take_first_or_undefined();
    let state = IntlSegmentsContainingContinuation {
        segments: *segments,
        realm,
        origin,
    };
    if matches!(index, StoredValue::Function(_) | StoredValue::Object(_)) {
        let realm = state.realm;
        let origin = state.origin.clone();
        return begin_operator_primitive_conversion(
            runtime,
            index,
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::IntlSegmentsContaining(Box::new(state)),
            realm,
            return_to,
            origin,
            execution_budget,
        );
    }
    finish_intl_segments_containing_primitive(runtime, &state, index)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "ToIntegerOrInfinity is non-negative and bounded by the UTF-16 string length"
)]
pub(super) fn finish_intl_segments_containing_primitive(
    runtime: &mut Runtime,
    state: &IntlSegmentsContainingContinuation,
    index: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let integer =
        number_to_integer_or_infinity(operator_to_number(index, state.realm, &state.origin)?);
    let Some((input, boundary)) = ({
        let segments =
            runtime
                .intl_segments_state(state.segments)?
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl Segments.containing lost its branded receiver",
                })?;
        if integer < 0.0 || integer >= f64::from(segments.input.len()) {
            None
        } else {
            let index = integer as usize;
            segments
                .boundaries
                .iter()
                .copied()
                .find(|boundary| boundary.start <= index && index < boundary.end)
                .map(|boundary| (segments.input.clone(), boundary))
        }
    }) else {
        return Ok(NativeDispatch::Immediate(StoredValue::Undefined));
    };
    create_intl_segment_data_object(runtime, state.realm, &input, boundary)
}

pub(super) fn begin_intl_segments_iterator(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(segments) = receiver else {
        return intl_segments_brand_error(realm, origin);
    };
    if runtime.intl_segments_state(*segments)?.is_none() {
        return intl_segments_brand_error(realm, origin);
    }
    let prototype = runtime.realm_intl_segment_iterator_prototype(realm)?;
    let iterator = runtime.allocate_intl_segment_iterator(prototype, *segments)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(iterator)))
}

pub(super) fn begin_intl_segment_iterator_next(
    runtime: &mut Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::Object(iterator) = receiver else {
        return intl_segment_iterator_brand_error(realm, origin);
    };
    let Some((segments_id, next_segment)) = runtime
        .intl_segment_iterator_state(*iterator)?
        .map(|state| (state.segments, state.next_segment))
    else {
        return intl_segment_iterator_brand_error(realm, origin);
    };
    let segment = {
        let segments =
            runtime
                .intl_segments_state(segments_id)?
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Intl Segment Iterator lost its Segments object",
                })?;
        segments
            .boundaries
            .get(next_segment)
            .copied()
            .map(|boundary| (segments.input.clone(), boundary))
    };
    let Some((input, boundary)) = segment else {
        return iterator_result(runtime, realm, StoredValue::Undefined, true);
    };
    let iterator_state = runtime.intl_segment_iterator_state_mut(*iterator)?.ok_or(
        EngineFault::RuntimeInvariant {
            message: "Intl Segment Iterator lost its branded state",
        },
    )?;
    iterator_state.next_segment = next_segment.saturating_add(1);
    let NativeDispatch::Immediate(value) =
        create_intl_segment_data_object(runtime, realm, &input, boundary)?
    else {
        return Err(EngineFault::RuntimeInvariant {
            message: "segment data creation returned a structured dispatch",
        }
        .into());
    };
    iterator_result(runtime, realm, value, false)
}

fn create_intl_segment_data_object(
    runtime: &mut Runtime,
    realm: RealmId,
    input: &JsString,
    boundary: SegmentBoundary,
) -> Result<NativeDispatch, NativeFailure> {
    let start = u32::try_from(boundary.start).map_err(|_| EngineFault::RuntimeInvariant {
        message: "Intl segment start exceeds the JavaScript string domain",
    })?;
    let end = u32::try_from(boundary.end).map_err(|_| EngineFault::RuntimeInvariant {
        message: "Intl segment end exceeds the JavaScript string domain",
    })?;
    let object = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    let mut properties = vec![
        ("segment", StoredValue::String(input.slice(start..end)?)),
        ("index", StoredValue::Number(JsNumber::from_u32(start))),
        ("input", StoredValue::String(input.clone())),
    ];
    if let Some(is_word_like) = boundary.is_word_like {
        properties.push(("isWordLike", StoredValue::Boolean(is_word_like)));
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

fn intl_segmenter_brand_error<T>(realm: RealmId, origin: JsStackFrame) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        realm,
        origin,
        ExceptionKind::TypeError,
        "Intl.Segmenter method called on incompatible receiver",
    )
}

fn intl_segments_brand_error<T>(realm: RealmId, origin: JsStackFrame) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        realm,
        origin,
        ExceptionKind::TypeError,
        "Intl Segments method called on incompatible receiver",
    )
}

fn intl_segment_iterator_brand_error<T>(
    realm: RealmId,
    origin: JsStackFrame,
) -> Result<T, NativeFailure> {
    intl_locale_list_error(
        realm,
        origin,
        ExceptionKind::TypeError,
        "Intl Segment Iterator.next called on incompatible receiver",
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
        to_locale_string_time_zone: None,
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
        format_value: Some(IntlDateTimeFormatLocaleValue::Date(instant)),
        to_locale_string_time_zone: None,
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

#[allow(
    clippy::too_many_arguments,
    reason = "native dispatch keeps the Temporal value, arguments, realm, return target, origin, and budget explicit"
)]
pub(super) fn begin_intl_temporal_to_locale_string(
    runtime: &mut Runtime,
    value: IntlDateTimeFormatLocaleValue,
    mut arguments: CallArguments,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (required, defaults, to_locale_string_time_zone) = match &value {
        IntlDateTimeFormatLocaleValue::Date(_) => {
            return Err(EngineFault::RuntimeInvariant {
                message: "the Date locale-string service reached the Temporal entry point",
            }
            .into());
        }
        IntlDateTimeFormatLocaleValue::Instant(_)
        | IntlDateTimeFormatLocaleValue::PlainDateTime(_) => (
            IntlDateTimeFormatRequired::Any,
            IntlDateTimeFormatDefaults::All,
            None,
        ),
        IntlDateTimeFormatLocaleValue::PlainDate(_)
        | IntlDateTimeFormatLocaleValue::PlainMonthDay(_)
        | IntlDateTimeFormatLocaleValue::PlainYearMonth(_) => (
            IntlDateTimeFormatRequired::Date,
            IntlDateTimeFormatDefaults::Date,
            None,
        ),
        IntlDateTimeFormatLocaleValue::PlainTime(_) => (
            IntlDateTimeFormatRequired::Time,
            IntlDateTimeFormatDefaults::Time,
            None,
        ),
        IntlDateTimeFormatLocaleValue::ZonedDateTime(date_time) => (
            IntlDateTimeFormatRequired::Any,
            IntlDateTimeFormatDefaults::ZonedDateTime,
            Some(date_time.time_zone().identifier().map_err(|_| {
                EngineFault::RuntimeInvariant {
                    message: "a Temporal.ZonedDateTime slot lacked a valid time-zone identifier",
                }
            })?),
        ),
    };
    let locales = arguments.take_first_or_undefined();
    let options_argument = arguments.take_first_or_undefined();
    let state = IntlDateTimeFormatConstructorContinuation {
        new_target: runtime.realm_intl_date_time_format_constructor(realm)?,
        format_value: Some(value),
        to_locale_string_time_zone,
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
                if option == IntlDateTimeFormatOption::TimeZone
                    && state.to_locale_string_time_zone.is_some()
                {
                    return intl_locale_list_error(
                        state.realm,
                        state.origin,
                        ExceptionKind::TypeError,
                        "Temporal.ZonedDateTime locale options must not specify timeZone",
                    );
                }
                if option == IntlDateTimeFormatOption::Hour12 {
                    state.options.hour12 = Some(runtime.to_boolean(&value)?);
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
                if !runtime.to_boolean(&completion)? {
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
        let time_zone = if let Some(time_zone) = &state.to_locale_string_time_zone {
            time_zone.clone()
        } else {
            temporal_rs::Temporal::local_now()
                .time_zone()
                .and_then(|time_zone| time_zone.identifier())
                .map_err(|_| EngineFault::RuntimeInvariant {
                    message: "the host did not provide a valid system time-zone identifier",
                })?
        };
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
    if let Some(value) = state.format_value.take() {
        let input = resolve_intl_date_time_format_locale_value(&resolved, value)?;
        if input.calendar.as_deref().is_some_and(|calendar| {
            let iso_is_compatible = !matches!(
                input.identity,
                IntlDateTimeInputIdentity::PlainYearMonth
                    | IntlDateTimeInputIdentity::PlainMonthDay
            );
            calendar != resolved.calendar.as_str() && !(iso_is_compatible && calendar == "iso8601")
        }) {
            return intl_locale_list_error(
                state.realm,
                state.origin,
                ExceptionKind::RangeError,
                "Temporal calendar does not match the resolved Intl.DateTimeFormat calendar",
            );
        }
        let formatted = format_datetime(&resolved, &input.value)
            .map_err(|error| intl_date_time_format_failure(state.realm, &state.origin, error))?;
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
    if state.options.date_style.is_some() || state.options.time_style.is_some() {
        return Ok(());
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
        IntlDateTimeFormatDefaults::Date
            | IntlDateTimeFormatDefaults::All
            | IntlDateTimeFormatDefaults::ZonedDateTime
    ) {
        state.options.year = Some(DateTimeComponentStyle::Numeric);
        state.options.month = Some(DateTimeComponentStyle::Numeric);
        state.options.day = Some(DateTimeComponentStyle::Numeric);
    }
    if matches!(
        state.defaults,
        IntlDateTimeFormatDefaults::Time
            | IntlDateTimeFormatDefaults::All
            | IntlDateTimeFormatDefaults::ZonedDateTime
    ) {
        state.options.hour = Some(DateTimeComponentStyle::Numeric);
        state.options.minute = Some(DateTimeComponentStyle::Numeric);
        state.options.second = Some(DateTimeComponentStyle::Numeric);
    }
    if state.defaults == IntlDateTimeFormatDefaults::ZonedDateTime
        && state.options.time_zone_name.is_none()
    {
        state.options.time_zone_name = Some(DateTimeTimeZoneName::Short);
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
            if !runtime.to_boolean(completion)? {
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

#[allow(
    clippy::too_many_lines,
    reason = "the closed Temporal locale-string input matrix keeps each branded slot projection auditable"
)]
fn resolve_intl_date_time_format_locale_value(
    resolved: &DateTimeFormatState,
    value: IntlDateTimeFormatLocaleValue,
) -> Result<ResolvedDateTimeInput, NativeFailure> {
    match value {
        IntlDateTimeFormatLocaleValue::Date(instant) => project_epoch_date_time_input(
            &resolved.time_zone,
            instant,
            DateTimeFormatInputKind::Epoch,
            IntlDateTimeInputIdentity::Number,
        ),
        IntlDateTimeFormatLocaleValue::Instant(instant) => project_epoch_date_time_input(
            &resolved.time_zone,
            instant,
            DateTimeFormatInputKind::Instant,
            IntlDateTimeInputIdentity::Instant,
        ),
        IntlDateTimeFormatLocaleValue::PlainDateTime(date_time) => {
            let calendar = date_time.calendar().identifier().to_owned();
            Ok(resolve_plain_date_time_input(
                &date_time,
                DateTimeFormatInputKind::PlainDateTime,
                IntlDateTimeInputIdentity::PlainDateTime,
                Some(calendar),
            ))
        }
        IntlDateTimeFormatLocaleValue::PlainDate(date) => {
            let calendar = date.calendar().identifier().to_owned();
            let instant = temporal_rs::Instant::from(date.epoch_ns_for_utc());
            Ok(resolve_anchored_plain_date_time_input(
                instant,
                DateTimeFormatInputKind::PlainDate,
                IntlDateTimeInputIdentity::PlainDate,
                Some(calendar),
            )?)
        }
        IntlDateTimeFormatLocaleValue::PlainYearMonth(year_month) => {
            let calendar = year_month.calendar_id().to_owned();
            let instant = temporal_rs::Instant::from(year_month.epoch_ns_for_utc());
            Ok(resolve_anchored_plain_date_time_input(
                instant,
                DateTimeFormatInputKind::PlainYearMonth,
                IntlDateTimeInputIdentity::PlainYearMonth,
                Some(calendar),
            )?)
        }
        IntlDateTimeFormatLocaleValue::PlainMonthDay(month_day) => {
            let calendar = month_day.calendar_id().to_owned();
            let instant = temporal_rs::Instant::from(month_day.epoch_ns_for_utc());
            Ok(resolve_anchored_plain_date_time_input(
                instant,
                DateTimeFormatInputKind::PlainMonthDay,
                IntlDateTimeInputIdentity::PlainMonthDay,
                Some(calendar),
            )?)
        }
        IntlDateTimeFormatLocaleValue::PlainTime(time) => {
            Ok(resolve_plain_time_date_time_input(&time))
        }
        IntlDateTimeFormatLocaleValue::ZonedDateTime(date_time) => {
            let calendar = date_time.calendar().identifier().to_owned();
            let instant = temporal_rs::Instant::from(*date_time.epoch_nanoseconds());
            let mut input = project_epoch_date_time_input(
                &resolved.time_zone,
                instant,
                DateTimeFormatInputKind::Instant,
                IntlDateTimeInputIdentity::Instant,
            )?;
            input.calendar = Some(calendar);
            Ok(input)
        }
    }
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
        return Ok(Some(resolve_plain_time_date_time_input(&time)));
    }
    Ok(None)
}

fn resolve_plain_time_date_time_input(time: &temporal_rs::PlainTime) -> ResolvedDateTimeInput {
    let nanosecond = u32::from(time.millisecond()) * 1_000_000
        + u32::from(time.microsecond()) * 1_000
        + u32::from(time.nanosecond());
    ResolvedDateTimeInput {
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
    }
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
    intl_date_time_format_failure(state.realm, &state.origin, error)
}

fn intl_date_time_format_failure(
    realm: RealmId,
    origin: &JsStackFrame,
    error: DateTimeFormatError,
) -> NativeFailure {
    match error {
        DateTimeFormatError::NoFields => NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::TypeError,
                message: JsString::from_utf8(
                    "Intl.DateTimeFormat has no fields for this Temporal value",
                )
                .expect("static Intl error is valid UTF-8"),
            },
            origin: origin.clone(),
        }),
        DateTimeFormatError::InvalidDateTime => NativeFailure::Abrupt(PendingException {
            realm,
            payload: PendingExceptionPayload::EngineError {
                kind: ExceptionKind::RangeError,
                message: JsString::from_utf8("Intl.DateTimeFormat value is outside its range")
                    .expect("static Intl error is valid UTF-8"),
            },
            origin: origin.clone(),
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

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive match routes canonical locale lists to every ECMA-402 continuation"
)]
fn finish_intl_locale_list(
    runtime: &mut Runtime,
    realm: RealmId,
    locales: Vec<StoredValue>,
    target: IntlLocaleListTarget,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let target = match target {
        IntlLocaleListTarget::ReturnArray => {
            return Ok(NativeDispatch::Immediate(StoredValue::Object(
                runtime.allocate_array(realm, locales)?,
            )));
        }
        target => target,
    };
    let requested_locales = intl_locale_strings(locales)?;
    match target {
        IntlLocaleListTarget::StringCase(state) => {
            let locale = requested_locales.first().map_or("en-US", String::as_str);
            let components =
                locale_components(locale).map_err(|_| EngineFault::RuntimeInvariant {
                    message: "canonical String case locale failed component parsing",
                })?;
            finish_locale_case_mapping(
                &state.subject,
                &components.language,
                state.uppercase,
                execution_budget,
            )
        }
        IntlLocaleListTarget::CollatorConstructor(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_collator_options(runtime, *state, return_to, execution_budget)
        }
        IntlLocaleListTarget::CollatorSupportedLocalesOf(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_collator_supported_locales_options(
                runtime,
                *state,
                return_to,
                execution_budget,
            )
        }
        IntlLocaleListTarget::NumberFormatConstructor(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_number_format_options(runtime, *state, return_to, execution_budget)
        }
        IntlLocaleListTarget::NumberFormatSupportedLocalesOf(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_number_format_supported_locales_options(
                runtime,
                *state,
                return_to,
                execution_budget,
            )
        }
        IntlLocaleListTarget::DateTimeFormatConstructor(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_date_time_format_options(runtime, *state, return_to, execution_budget)
        }
        IntlLocaleListTarget::DateTimeFormatSupportedLocalesOf(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_date_time_format_supported_locales_options(
                runtime,
                *state,
                return_to,
                execution_budget,
            )
        }
        IntlLocaleListTarget::PluralRulesConstructor(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_plural_rules_options(runtime, *state, return_to, execution_budget)
        }
        IntlLocaleListTarget::PluralRulesSupportedLocalesOf(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_plural_rules_supported_locales_options(
                runtime,
                *state,
                return_to,
                execution_budget,
            )
        }
        IntlLocaleListTarget::RelativeTimeFormatConstructor(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_relative_time_format_options(runtime, *state, return_to, execution_budget)
        }
        IntlLocaleListTarget::RelativeTimeFormatSupportedLocalesOf(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_relative_time_format_supported_locales_options(
                runtime,
                *state,
                return_to,
                execution_budget,
            )
        }
        IntlLocaleListTarget::ListFormatConstructor(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_list_format_options(runtime, *state, return_to, execution_budget)
        }
        IntlLocaleListTarget::ListFormatSupportedLocalesOf(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_list_format_supported_locales_options(
                runtime,
                *state,
                return_to,
                execution_budget,
            )
        }
        IntlLocaleListTarget::DisplayNamesConstructor(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_display_names_options(runtime, *state, return_to, execution_budget)
        }
        IntlLocaleListTarget::DisplayNamesSupportedLocalesOf(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_display_names_supported_locales_options(
                runtime,
                *state,
                return_to,
                execution_budget,
            )
        }
        IntlLocaleListTarget::DurationFormatConstructor(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_duration_format_options(runtime, *state, return_to, execution_budget)
        }
        IntlLocaleListTarget::DurationFormatSupportedLocalesOf(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_duration_format_supported_locales_options(
                runtime,
                *state,
                return_to,
                execution_budget,
            )
        }
        IntlLocaleListTarget::SegmenterConstructor(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_segmenter_options(runtime, *state, return_to, execution_budget)
        }
        IntlLocaleListTarget::SegmenterSupportedLocalesOf(mut state) => {
            state.requested_locales = requested_locales;
            begin_intl_segmenter_supported_locales_options(
                runtime,
                *state,
                return_to,
                execution_budget,
            )
        }
        IntlLocaleListTarget::ReturnArray => unreachable!("ReturnArray returned before dispatch"),
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
