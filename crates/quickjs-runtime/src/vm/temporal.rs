//! Initial `Temporal.Instant` JavaScript boundary over `temporal_rs`.

use temporal_rs::{
    Duration, Instant, Sign, TimeZone,
    error::ErrorKind as TemporalErrorKind,
    options::{
        DifferenceSettings, RelativeTo, RoundingIncrement, RoundingMode, RoundingOptions,
        ToStringRoundingOptions, Unit,
    },
    parsers::Precision,
};

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) struct TemporalDurationConstructorContinuation {
    arguments: Vec<StoredValue>,
    converted: Vec<JsNumber>,
    new_target: FunctionId,
}

impl TemporalDurationConstructorContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        usize_to_u64(self.arguments.len()).saturating_add(1)
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        for argument in &self.arguments {
            trace_stored_value_root(argument, mark);
        }
        mark(CollectionRoot::Heap(HeapReference::Function(
            self.new_target,
        )));
    }
}

const TEMPORAL_DURATION_BAG_FIELDS: [(&str, usize); 10] = [
    ("days", 3),
    ("hours", 4),
    ("microseconds", 8),
    ("milliseconds", 7),
    ("minutes", 5),
    ("months", 1),
    ("nanoseconds", 9),
    ("seconds", 6),
    ("weeks", 2),
    ("years", 0),
];

enum TemporalDurationLikeTarget {
    Allocate,
    CompareFirst {
        second: StoredValue,
        options: StoredValue,
    },
    CompareSecond {
        first: Duration,
        options: StoredValue,
    },
    Arithmetic {
        receiver: Duration,
        subtract: bool,
    },
    InstantArithmetic {
        receiver: Instant,
        subtract: bool,
    },
}

impl TemporalDurationLikeTarget {
    fn retained_values(&self) -> u64 {
        match self {
            Self::Allocate | Self::Arithmetic { .. } | Self::InstantArithmetic { .. } => 0,
            Self::CompareFirst { .. } => 2,
            Self::CompareSecond { .. } => 1,
        }
    }

    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::Allocate | Self::Arithmetic { .. } | Self::InstantArithmetic { .. } => {}
            Self::CompareFirst { second, options } => {
                trace_stored_value_root(second, mark);
                trace_stored_value_root(options, mark);
            }
            Self::CompareSecond { options, .. } => trace_stored_value_root(options, mark),
        }
    }
}

#[derive(Clone, Copy)]
enum TemporalDurationBagStage {
    ReadField,
    AwaitField,
    AwaitConversion,
}

pub(super) struct TemporalDurationBagContinuation {
    base: StoredValue,
    fields: [Option<i128>; 10],
    next: usize,
    any: bool,
    stage: TemporalDurationBagStage,
    target: TemporalDurationLikeTarget,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalDurationBagContinuation {
    pub(super) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(self.target.retained_values())
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.base, mark);
        self.target.trace_roots(mark);
    }
}

pub(super) struct TemporalDurationCompareOptionsContinuation {
    first: Duration,
    second: Duration,
    options: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalDurationCompareOptionsContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalDurationTotalStage {
    AwaitRelativeTo,
    AwaitUnit,
}

pub(super) struct TemporalDurationTotalContinuation {
    duration: Duration,
    options: StoredValue,
    relative_to: Option<RelativeTo>,
    stage: TemporalDurationTotalStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalDurationTotalContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalDurationRoundStage {
    LargestUnit,
    RelativeTo,
    RoundingIncrement,
    RoundingMode,
    SmallestUnit,
}

pub(super) struct TemporalDurationRoundContinuation {
    duration: Duration,
    options: StoredValue,
    largest_unit: Option<Unit>,
    relative_to: Option<RelativeTo>,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    stage: TemporalDurationRoundStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalDurationRoundContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalInstantRoundStage {
    RoundingIncrement,
    RoundingMode,
    SmallestUnit,
}

pub(super) struct TemporalInstantRoundContinuation {
    instant: Instant,
    options: StoredValue,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    stage: TemporalInstantRoundStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalInstantRoundContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalInstantDifferenceStage {
    LargestUnit,
    RoundingIncrement,
    RoundingMode,
    SmallestUnit,
}

pub(super) struct TemporalInstantDifferenceContinuation {
    receiver: Instant,
    other: Instant,
    options: StoredValue,
    largest_unit: Option<Unit>,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    since: bool,
    stage: TemporalInstantDifferenceStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalInstantDifferenceContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalDurationToStringStage {
    FractionalSecondDigits,
    RoundingMode,
    SmallestUnit,
}

pub(super) struct TemporalDurationToStringContinuation {
    duration: Duration,
    options: StoredValue,
    precision: Precision,
    rounding_mode: RoundingMode,
    smallest_unit: Option<Unit>,
    stage: TemporalDurationToStringStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalDurationToStringContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalInstantToStringStage {
    FractionalSecondDigits,
    RoundingMode,
    SmallestUnit,
    TimeZone,
}

pub(super) struct TemporalInstantToStringContinuation {
    instant: Instant,
    options: StoredValue,
    precision: Precision,
    rounding_mode: RoundingMode,
    smallest_unit: Option<Unit>,
    stage: TemporalInstantToStringStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalInstantToStringContinuation {
    pub(super) const fn retained_values() -> u64 {
        1
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "native entry points own their source frame and may retain it across suspension"
)]
pub(super) fn begin_temporal_duration_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = inputs.new_target else {
        return temporal_type_error(realm, &origin, "Temporal.Duration is not callable");
    };
    let mut arguments = inputs.arguments.into_remaining_values();
    arguments.truncate(10);
    arguments
        .try_reserve(10_usize.saturating_sub(arguments.len()))
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 10_usize.saturating_sub(arguments.len()),
        })?;
    while arguments.len() < 10 {
        arguments.push(StoredValue::Undefined);
    }
    let mut converted = Vec::new();
    converted
        .try_reserve_exact(10)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: 10,
        })?;
    advance_temporal_duration_constructor(
        runtime,
        TemporalDurationConstructorContinuation {
            arguments,
            converted,
            new_target,
        },
        None,
        realm,
        return_to,
        &origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "Temporal component conversion is resumable across user-defined primitive conversion"
)]
pub(super) fn advance_temporal_duration_constructor(
    runtime: &mut Runtime,
    mut state: TemporalDurationConstructorContinuation,
    completion: Option<JsNumber>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let Some(value) = completion {
        state.converted.push(value);
    }
    while state.converted.len() < state.arguments.len() {
        let index = state.converted.len();
        let argument = std::mem::replace(&mut state.arguments[index], StoredValue::Undefined);
        if matches!(argument, StoredValue::Undefined) {
            state.converted.push(JsNumber::from_i32(0));
            continue;
        }
        return begin_operator_primitive_conversion(
            runtime,
            argument,
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::TemporalDurationConstructor(Box::new(state)),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        );
    }

    let mut fields = [0_i128; 10];
    for (index, (field, value)) in fields.iter_mut().zip(&state.converted).enumerate() {
        let value = value.as_f64();
        let Some(integer) = temporal_duration_integer(value, index) else {
            return temporal_range_error(
                realm,
                origin,
                "Temporal.Duration fields must be finite integral Numbers",
            );
        };
        *field = integer;
    }
    let duration = match Duration::new(
        temporal_duration_i64_field(fields[0])?,
        temporal_duration_i64_field(fields[1])?,
        temporal_duration_i64_field(fields[2])?,
        temporal_duration_i64_field(fields[3])?,
        temporal_duration_i64_field(fields[4])?,
        temporal_duration_i64_field(fields[5])?,
        temporal_duration_i64_field(fields[6])?,
        temporal_duration_i64_field(fields[7])?,
        fields[8],
        fields[9],
    ) {
        Ok(duration) => duration,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    begin_temporal_duration_wrapper(
        runtime,
        realm,
        state.new_target,
        duration,
        return_to,
        origin.clone(),
        execution_budget,
    )
}

fn begin_temporal_duration_wrapper(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: FunctionId,
    duration: Duration,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    begin_intrinsic_get(
        runtime,
        realm,
        HeapReference::Function(new_target),
        StoredValue::Function(new_target),
        &prototype_key,
        IntrinsicGetContinuation::TemporalDurationConstructor {
            new_target,
            duration,
        },
        return_to,
        Some(origin),
        execution_budget,
    )
}

pub(super) fn finish_temporal_duration_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    duration: Duration,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        _ => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_temporal_duration_prototype(realm)?)
        }
    };
    let object = runtime.allocate_temporal_duration(prototype, duration)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the native method may suspend while retaining its explicit call context"
)]
pub(super) fn dispatch_temporal_duration_prototype(
    runtime: &mut Runtime,
    method: TemporalDurationPrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let duration = require_temporal_duration(runtime, receiver, realm, origin)?;
    let number = |value| NativeDispatch::Immediate(StoredValue::Number(JsNumber::from_i64(value)));
    match method {
        TemporalDurationPrototypeMethod::Years => Ok(number(duration.years())),
        TemporalDurationPrototypeMethod::Months => Ok(number(duration.months())),
        TemporalDurationPrototypeMethod::Weeks => Ok(number(duration.weeks())),
        TemporalDurationPrototypeMethod::Days => Ok(number(duration.days())),
        TemporalDurationPrototypeMethod::Hours => Ok(number(duration.hours())),
        TemporalDurationPrototypeMethod::Minutes => Ok(number(duration.minutes())),
        TemporalDurationPrototypeMethod::Seconds => Ok(number(duration.seconds())),
        TemporalDurationPrototypeMethod::Milliseconds => Ok(number(duration.milliseconds())),
        TemporalDurationPrototypeMethod::Microseconds => {
            Ok(temporal_duration_i128_number(duration.microseconds()))
        }
        TemporalDurationPrototypeMethod::Nanoseconds => {
            Ok(temporal_duration_i128_number(duration.nanoseconds()))
        }
        TemporalDurationPrototypeMethod::Sign => {
            let sign = match duration.sign() {
                Sign::Negative => -1,
                Sign::Zero => 0,
                Sign::Positive => 1,
            };
            Ok(number(sign))
        }
        TemporalDurationPrototypeMethod::Blank => Ok(NativeDispatch::Immediate(
            StoredValue::Boolean(duration.is_zero()),
        )),
        TemporalDurationPrototypeMethod::Abs => {
            allocate_temporal_duration_result(runtime, realm, duration.abs())
        }
        TemporalDurationPrototypeMethod::Negated => {
            allocate_temporal_duration_result(runtime, realm, duration.negated())
        }
        TemporalDurationPrototypeMethod::With => begin_temporal_duration_with(
            runtime,
            duration,
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalDurationPrototypeMethod::Add | TemporalDurationPrototypeMethod::Subtract => {
            begin_temporal_duration_like(
                runtime,
                arguments.take_first_or_undefined(),
                TemporalDurationLikeTarget::Arithmetic {
                    receiver: duration,
                    subtract: matches!(method, TemporalDurationPrototypeMethod::Subtract),
                },
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalDurationPrototypeMethod::Round => begin_temporal_duration_round(
            runtime,
            duration,
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalDurationPrototypeMethod::Total => begin_temporal_duration_total(
            runtime,
            duration,
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalDurationPrototypeMethod::ToString => begin_temporal_duration_to_string(
            runtime,
            duration,
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalDurationPrototypeMethod::ToJson
        | TemporalDurationPrototypeMethod::ToLocaleString => complete_temporal_duration_to_string(
            duration,
            Precision::Auto,
            RoundingMode::Trunc,
            None,
            realm,
            origin,
        ),
        TemporalDurationPrototypeMethod::ValueOf => temporal_type_error(
            realm,
            origin,
            "Temporal.Duration cannot be converted to a primitive value",
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the partial-duration conversion may suspend with the native call context"
)]
fn begin_temporal_duration_with(
    runtime: &mut Runtime,
    receiver: Duration,
    partial: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if partial.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.Duration.prototype.with requires an object",
        );
    }
    advance_temporal_duration_property_bag(
        runtime,
        TemporalDurationBagContinuation {
            base: partial,
            fields: temporal_duration_fields(receiver),
            next: 0,
            any: false,
            stage: TemporalDurationBagStage::ReadField,
            target: TemporalDurationLikeTarget::Allocate,
            realm,
            origin,
        },
        None,
        return_to,
        execution_budget,
    )
}

fn temporal_duration_fields(duration: Duration) -> [Option<i128>; 10] {
    [
        Some(i128::from(duration.years())),
        Some(i128::from(duration.months())),
        Some(i128::from(duration.weeks())),
        Some(i128::from(duration.days())),
        Some(i128::from(duration.hours())),
        Some(i128::from(duration.minutes())),
        Some(i128::from(duration.seconds())),
        Some(i128::from(duration.milliseconds())),
        Some(duration.microseconds()),
        Some(duration.nanoseconds()),
    ]
}

#[allow(
    clippy::too_many_arguments,
    reason = "the ordered Temporal options reader carries its native call context across user code"
)]
fn begin_temporal_duration_round(
    runtime: &mut Runtime,
    duration: Duration,
    round_to: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match round_to {
        StoredValue::Undefined => temporal_type_error(
            realm,
            &origin,
            "Temporal.Duration.prototype.round requires an options object or smallest-unit string",
        ),
        StoredValue::String(source) => {
            let smallest_unit = temporal_round_unit(&source, realm, &origin)?;
            complete_temporal_duration_round(
                runtime,
                duration,
                None,
                None,
                RoundingIncrement::ONE,
                RoundingMode::HalfExpand,
                Some(smallest_unit),
                realm,
                &origin,
            )
        }
        options if options.heap_reference().is_some() => begin_temporal_duration_round_get(
            runtime,
            TemporalDurationRoundContinuation {
                duration,
                options,
                largest_unit: None,
                relative_to: None,
                rounding_increment: RoundingIncrement::ONE,
                rounding_mode: RoundingMode::HalfExpand,
                stage: TemporalDurationRoundStage::LargestUnit,
                realm,
                origin,
            },
            "largestUnit",
            TemporalDurationRoundStage::LargestUnit,
            return_to,
            execution_budget,
        ),
        _ => temporal_type_error(
            realm,
            &origin,
            "Temporal.Duration.prototype.round requires an options object or smallest-unit string",
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "each observable Temporal options Get retains the complete native continuation state"
)]
fn begin_temporal_duration_round_get(
    runtime: &mut Runtime,
    mut state: TemporalDurationRoundContinuation,
    name: &str,
    next_stage: TemporalDurationRoundStage,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = next_stage;
    charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
    let name = JsString::from_utf8(name)?;
    let key = runtime.property_key_from_string(&name)?;
    let realm = state.realm;
    let origin = state.origin.clone();
    let dispatch = begin_value_get(
        runtime,
        &state.options,
        key,
        Some(&name),
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    match continue_get_state_after(
        dispatch,
        state,
        temporal_duration_round_continuation,
        "Temporal.Duration round option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => advance_temporal_duration_round_options(
            runtime,
            state,
            value,
            return_to,
            execution_budget,
        ),
        GetContinuationDispatch::Suspended(dispatch) => Ok(dispatch),
    }
}

fn temporal_duration_round_continuation(
    state: TemporalDurationRoundContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalDurationRoundOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one ordered state table preserves the specification's observable option-read and coercion sequence across suspension"
)]
pub(super) fn advance_temporal_duration_round_options(
    runtime: &mut Runtime,
    mut state: TemporalDurationRoundContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalDurationRoundStage::LargestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_duration_round_get(
                    runtime,
                    state,
                    "relativeTo",
                    TemporalDurationRoundStage::RelativeTo,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalDurationRoundLargestUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalDurationRoundStage::RelativeTo => {
            state.relative_to =
                temporal_relative_to_from_value(&value, state.realm, &state.origin)?;
            begin_temporal_duration_round_get(
                runtime,
                state,
                "roundingIncrement",
                TemporalDurationRoundStage::RoundingIncrement,
                return_to,
                execution_budget,
            )
        }
        TemporalDurationRoundStage::RoundingIncrement => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_duration_round_get(
                    runtime,
                    state,
                    "roundingMode",
                    TemporalDurationRoundStage::RoundingMode,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::TemporalDurationRoundRoundingIncrement(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalDurationRoundStage::RoundingMode => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_duration_round_get(
                    runtime,
                    state,
                    "smallestUnit",
                    TemporalDurationRoundStage::SmallestUnit,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalDurationRoundRoundingMode(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalDurationRoundStage::SmallestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return complete_temporal_duration_round(
                    runtime,
                    state.duration,
                    state.largest_unit,
                    state.relative_to,
                    state.rounding_increment,
                    state.rounding_mode,
                    None,
                    state.realm,
                    &state.origin,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalDurationRoundSmallestUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion option continuation still owns the resumable native call context"
)]
pub(super) fn finish_temporal_duration_round_largest_unit(
    runtime: &mut Runtime,
    mut state: TemporalDurationRoundContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.largest_unit = Some(temporal_round_unit(&source, state.realm, &state.origin)?);
    begin_temporal_duration_round_get(
        runtime,
        state,
        "relativeTo",
        TemporalDurationRoundStage::RelativeTo,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion option continuation still owns the resumable native call context"
)]
pub(super) fn finish_temporal_duration_round_rounding_increment(
    runtime: &mut Runtime,
    mut state: TemporalDurationRoundContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let value = operator_to_number(value, state.realm, &state.origin)?.as_f64();
    state.rounding_increment = match RoundingIncrement::try_from(value) {
        Ok(increment) => increment,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                state.realm,
                &state.origin,
                error,
            )?));
        }
    };
    begin_temporal_duration_round_get(
        runtime,
        state,
        "roundingMode",
        TemporalDurationRoundStage::RoundingMode,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion option continuation still owns the resumable native call context"
)]
pub(super) fn finish_temporal_duration_round_rounding_mode(
    runtime: &mut Runtime,
    mut state: TemporalDurationRoundContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.rounding_mode = temporal_rounding_mode(&source, state.realm, &state.origin)?;
    begin_temporal_duration_round_get(
        runtime,
        state,
        "smallestUnit",
        TemporalDurationRoundStage::SmallestUnit,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the final post-coercion option continuation owns the native result allocation context"
)]
pub(super) fn finish_temporal_duration_round_smallest_unit(
    runtime: &mut Runtime,
    state: TemporalDurationRoundContinuation,
    value: StoredValue,
    _return_to: Option<CallReturn>,
    _execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let smallest_unit = temporal_round_unit(&source, state.realm, &state.origin)?;
    complete_temporal_duration_round(
        runtime,
        state.duration,
        state.largest_unit,
        state.relative_to,
        state.rounding_increment,
        state.rounding_mode,
        Some(smallest_unit),
        state.realm,
        &state.origin,
    )
}

fn temporal_round_unit(
    source: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Unit, NativeFailure> {
    let source = source.to_utf8_lossy()?;
    match source.parse::<Unit>() {
        Ok(unit) => Ok(unit),
        Err(_) => Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "invalid Temporal unit",
        )?)),
    }
}

fn temporal_rounding_mode(
    source: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<RoundingMode, NativeFailure> {
    let source = source.to_utf8_lossy()?;
    let mode = match source.as_str() {
        "ceil" => RoundingMode::Ceil,
        "floor" => RoundingMode::Floor,
        "expand" => RoundingMode::Expand,
        "trunc" => RoundingMode::Trunc,
        "halfCeil" => RoundingMode::HalfCeil,
        "halfFloor" => RoundingMode::HalfFloor,
        "halfExpand" => RoundingMode::HalfExpand,
        "halfTrunc" => RoundingMode::HalfTrunc,
        "halfEven" => RoundingMode::HalfEven,
        _ => {
            return Err(NativeFailure::Abrupt(temporal_pending_exception(
                realm,
                origin,
                ExceptionKind::RangeError,
                "invalid Temporal roundingMode",
            )?));
        }
    };
    Ok(mode)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the completed JavaScript option record is passed explicitly to the shared temporal kernel"
)]
fn complete_temporal_duration_round(
    runtime: &mut Runtime,
    duration: Duration,
    largest_unit: Option<Unit>,
    relative_to: Option<RelativeTo>,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    smallest_unit: Option<Unit>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let mut options = RoundingOptions::default();
    options.largest_unit = largest_unit;
    options.smallest_unit = smallest_unit;
    options.rounding_mode = Some(rounding_mode);
    options.increment = Some(rounding_increment);
    let rounded = match duration.round(options, relative_to) {
        Ok(rounded) => rounded,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    allocate_temporal_duration_result(runtime, realm, rounded)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the options reader may suspend while retaining the native call context"
)]
fn begin_temporal_duration_total(
    runtime: &mut Runtime,
    duration: Duration,
    total_of: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::String(unit) = total_of {
        let unit = temporal_duration_unit(&unit, realm, &origin)?;
        return complete_temporal_duration_total(duration, unit, None, realm, &origin);
    }
    if total_of.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.Duration.prototype.total requires a unit string or options object",
        );
    }
    begin_temporal_duration_total_get(
        runtime,
        TemporalDurationTotalContinuation {
            duration,
            options: total_of,
            relative_to: None,
            stage: TemporalDurationTotalStage::AwaitRelativeTo,
            realm,
            origin,
        },
        "relativeTo",
        TemporalDurationTotalStage::AwaitRelativeTo,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "each observable options Get carries the resumable native call context"
)]
fn begin_temporal_duration_total_get(
    runtime: &mut Runtime,
    mut state: TemporalDurationTotalContinuation,
    name: &str,
    next_stage: TemporalDurationTotalStage,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = next_stage;
    charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
    let name = JsString::from_utf8(name)?;
    let key = runtime.property_key_from_string(&name)?;
    let realm = state.realm;
    let origin = state.origin.clone();
    let dispatch = begin_value_get(
        runtime,
        &state.options,
        key,
        Some(&name),
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    match continue_get_state_after(
        dispatch,
        state,
        temporal_duration_total_continuation,
        "Temporal.Duration total option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => advance_temporal_duration_total_options(
            runtime,
            state,
            value,
            return_to,
            execution_budget,
        ),
        GetContinuationDispatch::Suspended(dispatch) => Ok(dispatch),
    }
}

fn temporal_duration_total_continuation(
    state: TemporalDurationTotalContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalDurationTotalOptions(Box::new(state))
}

pub(super) fn advance_temporal_duration_total_options(
    runtime: &mut Runtime,
    mut state: TemporalDurationTotalContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalDurationTotalStage::AwaitRelativeTo => {
            state.relative_to =
                temporal_relative_to_from_value(&value, state.realm, &state.origin)?;
            begin_temporal_duration_total_get(
                runtime,
                state,
                "unit",
                TemporalDurationTotalStage::AwaitUnit,
                return_to,
                execution_budget,
            )
        }
        TemporalDurationTotalStage::AwaitUnit => {
            if matches!(value, StoredValue::Undefined) {
                return temporal_range_error(
                    state.realm,
                    &state.origin,
                    "Temporal.Duration.prototype.total requires a unit",
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalDurationTotalUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

pub(super) fn finish_temporal_duration_total_unit(
    mut state: TemporalDurationTotalContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let unit = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let unit = temporal_duration_unit(&unit, state.realm, &state.origin)?;
    complete_temporal_duration_total(
        state.duration,
        unit,
        state.relative_to.take(),
        state.realm,
        &state.origin,
    )
}

fn temporal_duration_unit(
    source: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Unit, NativeFailure> {
    let source = source.to_utf8_lossy()?;
    let Ok(unit) = source.parse::<Unit>() else {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "invalid Temporal unit",
        )?));
    };
    if unit == Unit::Auto {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "auto is not a valid Temporal unit here",
        )?));
    }
    Ok(unit)
}

fn complete_temporal_duration_total(
    duration: Duration,
    unit: Unit,
    relative_to: Option<RelativeTo>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let total = match duration.total(unit, relative_to) {
        Ok(total) => total,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::Number(
        JsNumber::from_f64(total.as_inner()),
    )))
}

fn temporal_duration_i128_number(value: i128) -> NativeDispatch {
    #[allow(
        clippy::cast_precision_loss,
        reason = "Temporal.Duration fields are exposed as ECMAScript Numbers"
    )]
    let value = value as f64;
    NativeDispatch::Immediate(StoredValue::Number(JsNumber::from_f64(value)))
}

fn allocate_temporal_duration_result(
    runtime: &mut Runtime,
    realm: RealmId,
    duration: Duration,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = HeapReference::Object(runtime.realm_temporal_duration_prototype(realm)?);
    let object = runtime.allocate_temporal_duration(prototype, duration)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn require_temporal_duration(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Duration, NativeFailure> {
    if let StoredValue::Object(object) = receiver
        && let Some(duration) = runtime.temporal_duration(*object)?
    {
        return Ok(duration);
    }
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a Temporal.Duration object")?,
        },
        origin: origin.clone(),
    }))
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "native entry points receive one owned source frame consistently"
)]
pub(super) fn dispatch_temporal_duration_static(
    runtime: &mut Runtime,
    method: TemporalDurationStaticMethod,
    realm: RealmId,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        TemporalDurationStaticMethod::From => begin_temporal_duration_like(
            runtime,
            arguments.take_first_or_undefined(),
            TemporalDurationLikeTarget::Allocate,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        TemporalDurationStaticMethod::Compare => {
            let first = arguments.take_first_or_undefined();
            let second = arguments.take_first_or_undefined();
            let options = arguments.take_first_or_undefined();
            begin_temporal_duration_like(
                runtime,
                first,
                TemporalDurationLikeTarget::CompareFirst { second, options },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Temporal duration conversion preserves its target and native suspension context"
)]
fn begin_temporal_duration_like(
    runtime: &mut Runtime,
    value: StoredValue,
    target: TemporalDurationLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(object) = value
        && let Some(duration) = runtime.temporal_duration(object)?
    {
        return continue_temporal_duration_like(
            runtime,
            duration,
            target,
            realm,
            return_to,
            &origin,
            execution_budget,
        );
    }
    if let StoredValue::String(value) = value {
        let source = value.to_utf8_lossy()?;
        let duration = match Duration::from_utf8(source.as_bytes()) {
            Ok(duration) => duration,
            Err(error) => {
                return Err(NativeFailure::Abrupt(temporal_range_exception_from_error(
                    realm, &origin, error,
                )?));
            }
        };
        return continue_temporal_duration_like(
            runtime,
            duration,
            target,
            realm,
            return_to,
            &origin,
            execution_budget,
        );
    }
    if value.heap_reference().is_some() {
        return advance_temporal_duration_property_bag(
            runtime,
            TemporalDurationBagContinuation {
                base: value,
                fields: [None; 10],
                next: 0,
                any: false,
                stage: TemporalDurationBagStage::ReadField,
                target,
                realm,
                origin,
            },
            None,
            return_to,
            execution_budget,
        );
    }
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("Temporal.Duration requires a string or property bag")?,
        },
        origin,
    }))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the explicit state machine preserves the normative property Get and ToNumber order"
)]
pub(super) fn advance_temporal_duration_property_bag(
    runtime: &mut Runtime,
    mut state: TemporalDurationBagContinuation,
    completion: Option<StoredValue>,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let mut completion = completion;
    loop {
        match state.stage {
            TemporalDurationBagStage::ReadField => {
                if state.next == TEMPORAL_DURATION_BAG_FIELDS.len() {
                    if !state.any {
                        return temporal_type_error(
                            state.realm,
                            &state.origin,
                            "Temporal.Duration property bag has no duration fields",
                        );
                    }
                    let duration =
                        duration_from_partial_fields(&state.fields, state.realm, &state.origin)?;
                    return continue_temporal_duration_like(
                        runtime,
                        duration,
                        state.target,
                        state.realm,
                        return_to,
                        &state.origin,
                        execution_budget,
                    );
                }
                charge_heap_property_lookup(runtime, &state.base, execution_budget)?;
                let name = JsString::from_utf8(TEMPORAL_DURATION_BAG_FIELDS[state.next].0)?;
                let key = runtime.property_key_from_string(&name)?;
                state.stage = TemporalDurationBagStage::AwaitField;
                let dispatch = begin_value_get(
                    runtime,
                    &state.base,
                    key,
                    Some(&name),
                    state.realm,
                    return_to,
                    state.origin.clone(),
                    execution_budget,
                )?;
                match continue_get_state_after(
                    dispatch,
                    state,
                    temporal_duration_bag_continuation,
                    "Temporal.Duration field Get produced a structured result",
                )? {
                    GetContinuationDispatch::Ready {
                        state: resumed,
                        value,
                    } => {
                        state = resumed;
                        completion = Some(value);
                    }
                    GetContinuationDispatch::Suspended(dispatch) => return Ok(dispatch),
                }
            }
            TemporalDurationBagStage::AwaitField => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.Duration field Get resumed without a value",
                })?;
                if matches!(value, StoredValue::Undefined) {
                    state.next = state.next.saturating_add(1);
                    state.stage = TemporalDurationBagStage::ReadField;
                    continue;
                }
                state.any = true;
                state.stage = TemporalDurationBagStage::AwaitConversion;
                let realm = state.realm;
                let origin = state.origin.clone();
                return begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::Number,
                    OperatorPrimitiveTarget::TemporalDurationBag(Box::new(state)),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                );
            }
            TemporalDurationBagStage::AwaitConversion => {
                let value = completion.take().ok_or(EngineFault::RuntimeInvariant {
                    message: "Temporal.Duration field conversion resumed without a value",
                })?;
                let number = operator_to_number(value, state.realm, &state.origin)?.as_f64();
                let canonical = TEMPORAL_DURATION_BAG_FIELDS[state.next].1;
                let Some(integer) = temporal_duration_integer(number, canonical) else {
                    return temporal_range_error(
                        state.realm,
                        &state.origin,
                        "Temporal.Duration fields must be finite integral Numbers",
                    );
                };
                state.fields[canonical] = Some(integer);
                state.next = state.next.saturating_add(1);
                state.stage = TemporalDurationBagStage::ReadField;
            }
        }
    }
}

fn temporal_duration_bag_continuation(
    state: TemporalDurationBagContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalDurationBag(Box::new(state))
}

fn duration_from_partial_fields(
    fields: &[Option<i128>; 10],
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Duration, NativeFailure> {
    match Duration::new(
        temporal_duration_i64_field(fields[0].unwrap_or_default())?,
        temporal_duration_i64_field(fields[1].unwrap_or_default())?,
        temporal_duration_i64_field(fields[2].unwrap_or_default())?,
        temporal_duration_i64_field(fields[3].unwrap_or_default())?,
        temporal_duration_i64_field(fields[4].unwrap_or_default())?,
        temporal_duration_i64_field(fields[5].unwrap_or_default())?,
        temporal_duration_i64_field(fields[6].unwrap_or_default())?,
        temporal_duration_i64_field(fields[7].unwrap_or_default())?,
        fields[8].unwrap_or_default(),
        fields[9].unwrap_or_default(),
    ) {
        Ok(duration) => Ok(duration),
        Err(error) => Err(NativeFailure::Abrupt(temporal_exception_from_error(
            realm, origin, error,
        )?)),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "the explicit binary64 bounds place each integral field inside its Rust domain"
)]
fn temporal_duration_integer(value: f64, index: usize) -> Option<i128> {
    if !value.is_finite() || value.fract() != 0.0 {
        return None;
    }
    if index < 8 {
        if !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&value) {
            return None;
        }
        return Some(i128::from(value as i64));
    }
    if !(-170_141_183_460_469_231_731_687_303_715_884_105_728.0
        ..170_141_183_460_469_231_731_687_303_715_884_105_728.0)
        .contains(&value)
    {
        return None;
    }
    Some(value as i128)
}

fn temporal_duration_i64_field(value: i128) -> Result<i64, NativeFailure> {
    i64::try_from(value).map_err(|_| {
        EngineFault::RuntimeInvariant {
            message: "validated Temporal.Duration field escaped its i64 domain",
        }
        .into()
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "each conversion target resumes with explicit realm, return, source, and fuel context"
)]
fn continue_temporal_duration_like(
    runtime: &mut Runtime,
    duration: Duration,
    target: TemporalDurationLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match target {
        TemporalDurationLikeTarget::Allocate => {
            allocate_temporal_duration_result(runtime, realm, duration)
        }
        TemporalDurationLikeTarget::CompareFirst { second, options } => {
            begin_temporal_duration_like(
                runtime,
                second,
                TemporalDurationLikeTarget::CompareSecond {
                    first: duration,
                    options,
                },
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalDurationLikeTarget::CompareSecond { first, options } => {
            begin_temporal_duration_compare_options(
                runtime,
                first,
                duration,
                &options,
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalDurationLikeTarget::Arithmetic { receiver, subtract } => {
            let result = if subtract {
                receiver.subtract(&duration)
            } else {
                receiver.add(&duration)
            };
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            allocate_temporal_duration_result(runtime, realm, result)
        }
        TemporalDurationLikeTarget::InstantArithmetic { receiver, subtract } => {
            let result = if subtract {
                receiver.subtract(&duration)
            } else {
                receiver.add(&duration)
            };
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            };
            allocate_temporal_instant_result(runtime, realm, result)
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the options Get retains both converted durations and the native suspension context"
)]
fn begin_temporal_duration_compare_options(
    runtime: &mut Runtime,
    first: Duration,
    second: Duration,
    options: &StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return complete_temporal_duration_compare(first, second, None, realm, &origin);
    }
    if options.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.Duration.compare options must be an object",
        );
    }
    charge_heap_property_lookup(runtime, options, execution_budget)?;
    let name = JsString::from_utf8("relativeTo")?;
    let key = runtime.property_key_from_string(&name)?;
    let state = TemporalDurationCompareOptionsContinuation {
        first,
        second,
        options: options.duplicate(),
        realm,
        origin: origin.clone(),
    };
    let dispatch = begin_value_get(
        runtime,
        options,
        key,
        Some(&name),
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    match continue_get_state_after(
        dispatch,
        state,
        temporal_duration_compare_options_continuation,
        "Temporal.Duration relativeTo Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            finish_temporal_duration_compare_options(runtime, &state, &value)
        }
        GetContinuationDispatch::Suspended(dispatch) => Ok(dispatch),
    }
}

fn temporal_duration_compare_options_continuation(
    state: TemporalDurationCompareOptionsContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalDurationCompareOptions(state)
}

pub(super) fn finish_temporal_duration_compare_options(
    _runtime: &mut Runtime,
    state: &TemporalDurationCompareOptionsContinuation,
    relative_to: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let relative_to = temporal_relative_to_from_value(relative_to, state.realm, &state.origin)?;
    complete_temporal_duration_compare(
        state.first,
        state.second,
        relative_to,
        state.realm,
        &state.origin,
    )
}

fn temporal_relative_to_from_value(
    value: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Option<RelativeTo>, NativeFailure> {
    match value {
        StoredValue::Undefined => Ok(None),
        StoredValue::String(source) => {
            let source = source.to_utf8_lossy()?;
            match RelativeTo::try_from_str(&source) {
                Ok(relative_to) => Ok(Some(relative_to)),
                Err(error) => Err(NativeFailure::Abrupt(temporal_exception_from_error(
                    realm, origin, error,
                )?)),
            }
        }
        _ => Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::TypeError,
            "Temporal relativeTo must be a string or Temporal object",
        )?)),
    }
}

fn complete_temporal_duration_compare(
    first: Duration,
    second: Duration,
    relative_to: Option<RelativeTo>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let ordering = match first.compare(&second, relative_to) {
        Ok(ordering) => ordering,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    let value = match ordering {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    };
    Ok(NativeDispatch::Immediate(StoredValue::Number(
        JsNumber::from_i32(value),
    )))
}

pub(super) fn begin_temporal_instant_constructor(
    runtime: &mut Runtime,
    realm: RealmId,
    mut inputs: CallInputs,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let Some(new_target) = inputs.new_target else {
        return temporal_type_error(realm, &origin, "Temporal.Instant is not callable");
    };
    begin_operator_primitive_conversion(
        runtime,
        inputs.arguments.take_first_or_undefined(),
        OperatorPrimitiveHint::Number,
        OperatorPrimitiveTarget::TemporalInstantNanoseconds {
            new_target: Some(new_target),
        },
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

pub(super) fn begin_temporal_instant_static(
    runtime: &mut Runtime,
    method: TemporalInstantStaticMethod,
    realm: RealmId,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let value = arguments.take_first_or_undefined();
    match method {
        TemporalInstantStaticMethod::From => begin_temporal_instant_like(
            runtime,
            value,
            TemporalInstantLikeTarget::Allocate,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        TemporalInstantStaticMethod::Compare => {
            let second = arguments.take_first_or_undefined();
            begin_temporal_instant_like(
                runtime,
                value,
                TemporalInstantLikeTarget::CompareFirst { second },
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalInstantStaticMethod::FromEpochNanoseconds => begin_operator_primitive_conversion(
            runtime,
            value,
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::TemporalInstantNanoseconds { new_target: None },
            realm,
            return_to,
            origin,
            execution_budget,
        ),
        TemporalInstantStaticMethod::FromEpochMilliseconds => begin_operator_primitive_conversion(
            runtime,
            value,
            OperatorPrimitiveHint::Number,
            OperatorPrimitiveTarget::TemporalInstantMilliseconds,
            realm,
            return_to,
            origin,
            execution_budget,
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Temporal instant conversion is resumable across user-defined primitive conversion"
)]
fn begin_temporal_instant_like(
    runtime: &mut Runtime,
    value: StoredValue,
    target: TemporalInstantLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if let StoredValue::Object(object) = value
        && let Some(instant) = runtime.temporal_instant(object)?
    {
        return continue_temporal_instant_like(
            runtime,
            instant,
            target,
            realm,
            return_to,
            &origin,
            execution_budget,
        );
    }
    begin_operator_primitive_conversion(
        runtime,
        value,
        OperatorPrimitiveHint::String,
        OperatorPrimitiveTarget::TemporalInstantString(Box::new(target)),
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the primitive-conversion finish restores the complete native continuation context"
)]
pub(super) fn finish_temporal_instant_string(
    runtime: &mut Runtime,
    value: StoredValue,
    target: TemporalInstantLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let StoredValue::String(value) = value else {
        return temporal_type_error(realm, origin, "Temporal.Instant requires a string");
    };
    let source = value.to_utf8_lossy()?;
    let instant = match Instant::from_utf8(source.as_bytes()) {
        Ok(instant) => instant,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_range_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    continue_temporal_instant_like(
        runtime,
        instant,
        target,
        realm,
        return_to,
        origin,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "each conversion target resumes with explicit realm, return, source, and fuel context"
)]
fn continue_temporal_instant_like(
    runtime: &mut Runtime,
    instant: Instant,
    target: TemporalInstantLikeTarget,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match target {
        TemporalInstantLikeTarget::Allocate => {
            allocate_temporal_instant_result(runtime, realm, instant)
        }
        TemporalInstantLikeTarget::CompareFirst { second } => begin_temporal_instant_like(
            runtime,
            second,
            TemporalInstantLikeTarget::CompareSecond {
                first_epoch_nanoseconds: instant.as_i128(),
            },
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalInstantLikeTarget::CompareSecond {
            first_epoch_nanoseconds,
        } => {
            let ordering = first_epoch_nanoseconds.cmp(&instant.as_i128());
            let result = match ordering {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            Ok(NativeDispatch::Immediate(StoredValue::Number(
                JsNumber::from_i32(result),
            )))
        }
        TemporalInstantLikeTarget::Equals {
            receiver_epoch_nanoseconds,
        } => Ok(NativeDispatch::Immediate(StoredValue::Boolean(
            receiver_epoch_nanoseconds == instant.as_i128(),
        ))),
        TemporalInstantLikeTarget::Difference {
            receiver,
            options,
            since,
        } => begin_temporal_instant_difference(
            runtime,
            receiver,
            instant,
            options,
            since,
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the conversion finish carries the standard native constructor continuation context"
)]
pub(super) fn finish_temporal_instant_nanoseconds(
    runtime: &mut Runtime,
    value: &StoredValue,
    new_target: Option<FunctionId>,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let bigint = to_bigint_from_primitive(value, realm, origin)?;
    let Some(epoch_nanoseconds) = bigint.to_i128() else {
        return temporal_range_error(realm, origin, "instant is outside the supported range");
    };
    let instant = match Instant::try_new(epoch_nanoseconds) {
        Ok(instant) => instant,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    if let Some(new_target) = new_target {
        return begin_temporal_instant_wrapper(
            runtime,
            realm,
            new_target,
            instant,
            return_to,
            origin.clone(),
            execution_budget,
        );
    }
    allocate_temporal_instant_result(runtime, realm, instant)
}

pub(super) fn finish_temporal_instant_milliseconds(
    runtime: &mut Runtime,
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let value = operator_to_number(value, realm, origin)?.as_f64();
    if !value.is_finite() || value.fract() != 0.0 || value.abs() > 8_640_000_000_000_000.0 {
        return temporal_range_error(
            realm,
            origin,
            "epoch milliseconds are outside the supported range",
        );
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the finite integral Temporal.Instant bound fits exactly in i64"
    )]
    let milliseconds = value as i64;
    let instant = match Instant::from_epoch_milliseconds(milliseconds) {
        Ok(instant) => instant,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    allocate_temporal_instant_result(runtime, realm, instant)
}

fn begin_temporal_instant_wrapper(
    runtime: &mut Runtime,
    realm: RealmId,
    new_target: FunctionId,
    instant: Instant,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    begin_intrinsic_get(
        runtime,
        realm,
        HeapReference::Function(new_target),
        StoredValue::Function(new_target),
        &prototype_key,
        IntrinsicGetContinuation::TemporalInstantConstructor {
            new_target,
            epoch_nanoseconds: instant.as_i128(),
        },
        return_to,
        Some(origin),
        execution_budget,
    )
}

pub(super) fn finish_temporal_instant_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    epoch_nanoseconds: i128,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = match requested {
        StoredValue::Function(function) => HeapReference::Function(*function),
        StoredValue::Object(object) => HeapReference::Object(*object),
        _ => {
            let realm = runtime.function_realm(new_target)?;
            HeapReference::Object(runtime.realm_temporal_instant_prototype(realm)?)
        }
    };
    let instant =
        Instant::try_new(epoch_nanoseconds).map_err(|_| EngineFault::RuntimeInvariant {
            message: "validated Temporal.Instant escaped its constructor continuation",
        })?;
    let object = runtime.allocate_temporal_instant(prototype, instant)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

fn allocate_temporal_instant_result(
    runtime: &mut Runtime,
    realm: RealmId,
    instant: Instant,
) -> Result<NativeDispatch, NativeFailure> {
    let prototype = HeapReference::Object(runtime.realm_temporal_instant_prototype(realm)?);
    let object = runtime.allocate_temporal_instant(prototype, instant)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(object)))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the shared prototype dispatcher preserves explicit call, return, source, and fuel context"
)]
pub(super) fn dispatch_temporal_instant_prototype(
    runtime: &mut Runtime,
    method: TemporalInstantPrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let instant = require_temporal_instant(runtime, receiver, realm, origin)?;
    match method {
        TemporalInstantPrototypeMethod::EpochMilliseconds => Ok(NativeDispatch::Immediate(
            StoredValue::Number(JsNumber::from_i64(instant.epoch_milliseconds())),
        )),
        TemporalInstantPrototypeMethod::EpochNanoseconds => Ok(NativeDispatch::Immediate(
            StoredValue::BigInt(Arc::new(JsBigInt::from_i128(instant.as_i128()))),
        )),
        TemporalInstantPrototypeMethod::Add | TemporalInstantPrototypeMethod::Subtract => {
            begin_temporal_duration_like(
                runtime,
                arguments.take_first_or_undefined(),
                TemporalDurationLikeTarget::InstantArithmetic {
                    receiver: instant,
                    subtract: matches!(method, TemporalInstantPrototypeMethod::Subtract),
                },
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalInstantPrototypeMethod::Until | TemporalInstantPrototypeMethod::Since => {
            let other = arguments.take_first_or_undefined();
            let options = arguments.take_first_or_undefined();
            begin_temporal_instant_like(
                runtime,
                other,
                TemporalInstantLikeTarget::Difference {
                    receiver: instant,
                    options,
                    since: matches!(method, TemporalInstantPrototypeMethod::Since),
                },
                realm,
                return_to,
                origin.clone(),
                execution_budget,
            )
        }
        TemporalInstantPrototypeMethod::Round => begin_temporal_instant_round(
            runtime,
            instant,
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalInstantPrototypeMethod::Equals => begin_temporal_instant_like(
            runtime,
            arguments.take_first_or_undefined(),
            TemporalInstantLikeTarget::Equals {
                receiver_epoch_nanoseconds: instant.as_i128(),
            },
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalInstantPrototypeMethod::ToString => begin_temporal_instant_to_string(
            runtime,
            instant,
            arguments.take_first_or_undefined(),
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalInstantPrototypeMethod::ToJson => complete_temporal_instant_to_string(
            instant,
            Precision::Auto,
            RoundingMode::Trunc,
            None,
            StoredValue::Undefined,
            realm,
            origin,
        ),
        TemporalInstantPrototypeMethod::ValueOf => temporal_type_error(
            realm,
            origin,
            "Temporal.Instant cannot be converted to a primitive value",
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the ordered Temporal options reader retains the native call context across user code"
)]
fn begin_temporal_instant_round(
    runtime: &mut Runtime,
    instant: Instant,
    round_to: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match round_to {
        StoredValue::Undefined => temporal_type_error(
            realm,
            &origin,
            "Temporal.Instant.prototype.round requires an options object or smallest-unit string",
        ),
        StoredValue::String(source) => {
            let smallest_unit = temporal_round_unit(&source, realm, &origin)?;
            complete_temporal_instant_round(
                runtime,
                instant,
                RoundingIncrement::ONE,
                RoundingMode::HalfExpand,
                Some(smallest_unit),
                realm,
                &origin,
            )
        }
        options if options.heap_reference().is_some() => begin_temporal_instant_round_get(
            runtime,
            TemporalInstantRoundContinuation {
                instant,
                options,
                rounding_increment: RoundingIncrement::ONE,
                rounding_mode: RoundingMode::HalfExpand,
                stage: TemporalInstantRoundStage::RoundingIncrement,
                realm,
                origin,
            },
            "roundingIncrement",
            TemporalInstantRoundStage::RoundingIncrement,
            return_to,
            execution_budget,
        ),
        _ => temporal_type_error(
            realm,
            &origin,
            "Temporal.Instant.prototype.round requires an options object or smallest-unit string",
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "each observable Temporal options Get retains the complete native continuation state"
)]
fn begin_temporal_instant_round_get(
    runtime: &mut Runtime,
    mut state: TemporalInstantRoundContinuation,
    name: &str,
    next_stage: TemporalInstantRoundStage,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = next_stage;
    charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
    let name = JsString::from_utf8(name)?;
    let key = runtime.property_key_from_string(&name)?;
    let realm = state.realm;
    let origin = state.origin.clone();
    let dispatch = begin_value_get(
        runtime,
        &state.options,
        key,
        Some(&name),
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    match continue_get_state_after(
        dispatch,
        state,
        temporal_instant_round_continuation,
        "Temporal.Instant round option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => advance_temporal_instant_round_options(
            runtime,
            state,
            value,
            return_to,
            execution_budget,
        ),
        GetContinuationDispatch::Suspended(dispatch) => Ok(dispatch),
    }
}

fn temporal_instant_round_continuation(
    state: TemporalInstantRoundContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalInstantRoundOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    reason = "one ordered state table preserves the specification's observable option-read and coercion sequence across suspension"
)]
pub(super) fn advance_temporal_instant_round_options(
    runtime: &mut Runtime,
    state: TemporalInstantRoundContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalInstantRoundStage::RoundingIncrement => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_instant_round_get(
                    runtime,
                    state,
                    "roundingMode",
                    TemporalInstantRoundStage::RoundingMode,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::TemporalInstantRoundRoundingIncrement(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalInstantRoundStage::RoundingMode => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_instant_round_get(
                    runtime,
                    state,
                    "smallestUnit",
                    TemporalInstantRoundStage::SmallestUnit,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalInstantRoundRoundingMode(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalInstantRoundStage::SmallestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return complete_temporal_instant_round(
                    runtime,
                    state.instant,
                    state.rounding_increment,
                    state.rounding_mode,
                    None,
                    state.realm,
                    &state.origin,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalInstantRoundSmallestUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion option continuation still owns the resumable native call context"
)]
pub(super) fn finish_temporal_instant_round_rounding_increment(
    runtime: &mut Runtime,
    mut state: TemporalInstantRoundContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let value = operator_to_number(value, state.realm, &state.origin)?.as_f64();
    state.rounding_increment = match RoundingIncrement::try_from(value) {
        Ok(increment) => increment,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                state.realm,
                &state.origin,
                error,
            )?));
        }
    };
    begin_temporal_instant_round_get(
        runtime,
        state,
        "roundingMode",
        TemporalInstantRoundStage::RoundingMode,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion option continuation still owns the resumable native call context"
)]
pub(super) fn finish_temporal_instant_round_rounding_mode(
    runtime: &mut Runtime,
    mut state: TemporalInstantRoundContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.rounding_mode = temporal_rounding_mode(&source, state.realm, &state.origin)?;
    begin_temporal_instant_round_get(
        runtime,
        state,
        "smallestUnit",
        TemporalInstantRoundStage::SmallestUnit,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the final post-coercion option continuation owns the native result allocation context"
)]
pub(super) fn finish_temporal_instant_round_smallest_unit(
    runtime: &mut Runtime,
    state: &TemporalInstantRoundContinuation,
    value: StoredValue,
    _return_to: Option<CallReturn>,
    _execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let smallest_unit = temporal_round_unit(&source, state.realm, &state.origin)?;
    complete_temporal_instant_round(
        runtime,
        state.instant,
        state.rounding_increment,
        state.rounding_mode,
        Some(smallest_unit),
        state.realm,
        &state.origin,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the completed JavaScript option record is passed explicitly to the shared temporal kernel"
)]
fn complete_temporal_instant_round(
    runtime: &mut Runtime,
    instant: Instant,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    smallest_unit: Option<Unit>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let mut options = RoundingOptions::default();
    options.smallest_unit = smallest_unit;
    options.rounding_mode = Some(rounding_mode);
    options.increment = Some(rounding_increment);
    let rounded = match instant.round(options) {
        Ok(rounded) => rounded,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    allocate_temporal_instant_result(runtime, realm, rounded)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the difference operand is converted before the options object is observed"
)]
fn begin_temporal_instant_difference(
    runtime: &mut Runtime,
    receiver: Instant,
    other: Instant,
    options: StoredValue,
    since: bool,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return complete_temporal_instant_difference(
            runtime,
            receiver,
            other,
            None,
            RoundingIncrement::ONE,
            RoundingMode::Trunc,
            None,
            since,
            realm,
            &origin,
        );
    }
    if options.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.Instant.prototype.until options must be an object",
        );
    }
    begin_temporal_instant_difference_get(
        runtime,
        TemporalInstantDifferenceContinuation {
            receiver,
            other,
            options,
            largest_unit: None,
            rounding_increment: RoundingIncrement::ONE,
            rounding_mode: RoundingMode::Trunc,
            since,
            stage: TemporalInstantDifferenceStage::LargestUnit,
            realm,
            origin,
        },
        "largestUnit",
        TemporalInstantDifferenceStage::LargestUnit,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "each observable difference option Get retains the complete native call context"
)]
fn begin_temporal_instant_difference_get(
    runtime: &mut Runtime,
    mut state: TemporalInstantDifferenceContinuation,
    name: &str,
    next_stage: TemporalInstantDifferenceStage,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = next_stage;
    charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
    let name = JsString::from_utf8(name)?;
    let key = runtime.property_key_from_string(&name)?;
    let realm = state.realm;
    let origin = state.origin.clone();
    let dispatch = begin_value_get(
        runtime,
        &state.options,
        key,
        Some(&name),
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    match continue_get_state_after(
        dispatch,
        state,
        temporal_instant_difference_continuation,
        "Temporal.Instant difference option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_instant_difference_options(
                runtime,
                state,
                value,
                return_to,
                execution_budget,
            )
        }
        GetContinuationDispatch::Suspended(dispatch) => Ok(dispatch),
    }
}

fn temporal_instant_difference_continuation(
    state: TemporalInstantDifferenceContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalInstantDifferenceOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one ordered state table preserves the specification's observable difference option-read and coercion sequence across suspension"
)]
pub(super) fn advance_temporal_instant_difference_options(
    runtime: &mut Runtime,
    state: TemporalInstantDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalInstantDifferenceStage::LargestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_instant_difference_get(
                    runtime,
                    state,
                    "roundingIncrement",
                    TemporalInstantDifferenceStage::RoundingIncrement,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalInstantDifferenceLargestUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalInstantDifferenceStage::RoundingIncrement => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_instant_difference_get(
                    runtime,
                    state,
                    "roundingMode",
                    TemporalInstantDifferenceStage::RoundingMode,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::Number,
                OperatorPrimitiveTarget::TemporalInstantDifferenceRoundingIncrement(Box::new(
                    state,
                )),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalInstantDifferenceStage::RoundingMode => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_instant_difference_get(
                    runtime,
                    state,
                    "smallestUnit",
                    TemporalInstantDifferenceStage::SmallestUnit,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalInstantDifferenceRoundingMode(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalInstantDifferenceStage::SmallestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return complete_temporal_instant_difference(
                    runtime,
                    state.receiver,
                    state.other,
                    state.largest_unit,
                    state.rounding_increment,
                    state.rounding_mode,
                    None,
                    state.since,
                    state.realm,
                    &state.origin,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalInstantDifferenceSmallestUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion option continuation retains the native difference call context"
)]
pub(super) fn finish_temporal_instant_difference_largest_unit(
    runtime: &mut Runtime,
    mut state: TemporalInstantDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.largest_unit = Some(temporal_round_unit(&source, state.realm, &state.origin)?);
    begin_temporal_instant_difference_get(
        runtime,
        state,
        "roundingIncrement",
        TemporalInstantDifferenceStage::RoundingIncrement,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion option continuation retains the native difference call context"
)]
pub(super) fn finish_temporal_instant_difference_rounding_increment(
    runtime: &mut Runtime,
    mut state: TemporalInstantDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let value = operator_to_number(value, state.realm, &state.origin)?.as_f64();
    state.rounding_increment = match RoundingIncrement::try_from(value) {
        Ok(increment) => increment,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                state.realm,
                &state.origin,
                error,
            )?));
        }
    };
    begin_temporal_instant_difference_get(
        runtime,
        state,
        "roundingMode",
        TemporalInstantDifferenceStage::RoundingMode,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion option continuation retains the native difference call context"
)]
pub(super) fn finish_temporal_instant_difference_rounding_mode(
    runtime: &mut Runtime,
    mut state: TemporalInstantDifferenceContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.rounding_mode = temporal_rounding_mode(&source, state.realm, &state.origin)?;
    begin_temporal_instant_difference_get(
        runtime,
        state,
        "smallestUnit",
        TemporalInstantDifferenceStage::SmallestUnit,
        return_to,
        execution_budget,
    )
}

pub(super) fn finish_temporal_instant_difference_smallest_unit(
    runtime: &mut Runtime,
    state: &TemporalInstantDifferenceContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let smallest_unit = temporal_round_unit(&source, state.realm, &state.origin)?;
    complete_temporal_instant_difference(
        runtime,
        state.receiver,
        state.other,
        state.largest_unit,
        state.rounding_increment,
        state.rounding_mode,
        Some(smallest_unit),
        state.since,
        state.realm,
        &state.origin,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the completed JavaScript options record is passed explicitly to the shared temporal kernel"
)]
fn complete_temporal_instant_difference(
    runtime: &mut Runtime,
    receiver: Instant,
    other: Instant,
    largest_unit: Option<Unit>,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    smallest_unit: Option<Unit>,
    since: bool,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let mut settings = DifferenceSettings::default();
    settings.largest_unit = largest_unit;
    settings.smallest_unit = smallest_unit;
    settings.rounding_mode = Some(rounding_mode);
    settings.increment = Some(rounding_increment);
    let duration = if since {
        receiver.since(&other, settings)
    } else {
        receiver.until(&other, settings)
    };
    let duration = match duration {
        Ok(duration) => duration,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    allocate_temporal_duration_result(runtime, realm, duration)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the ordered formatting options reader owns the branded duration and its resumable call context"
)]
fn begin_temporal_duration_to_string(
    runtime: &mut Runtime,
    duration: Duration,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return complete_temporal_duration_to_string(
            duration,
            Precision::Auto,
            RoundingMode::Trunc,
            None,
            realm,
            &origin,
        );
    }
    if options.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.Duration.prototype.toString options must be an object",
        );
    }
    begin_temporal_duration_to_string_get(
        runtime,
        TemporalDurationToStringContinuation {
            duration,
            options,
            precision: Precision::Auto,
            rounding_mode: RoundingMode::Trunc,
            smallest_unit: None,
            stage: TemporalDurationToStringStage::FractionalSecondDigits,
            realm,
            origin,
        },
        "fractionalSecondDigits",
        TemporalDurationToStringStage::FractionalSecondDigits,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "each observable formatting option Get retains the complete native call context"
)]
fn begin_temporal_duration_to_string_get(
    runtime: &mut Runtime,
    mut state: TemporalDurationToStringContinuation,
    name: &str,
    next_stage: TemporalDurationToStringStage,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = next_stage;
    charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
    let name = JsString::from_utf8(name)?;
    let key = runtime.property_key_from_string(&name)?;
    let realm = state.realm;
    let origin = state.origin.clone();
    let dispatch = begin_value_get(
        runtime,
        &state.options,
        key,
        Some(&name),
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    match continue_get_state_after(
        dispatch,
        state,
        temporal_duration_to_string_continuation,
        "Temporal.Duration toString option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_duration_to_string_options(
                runtime,
                state,
                value,
                return_to,
                execution_budget,
            )
        }
        GetContinuationDispatch::Suspended(dispatch) => Ok(dispatch),
    }
}

fn temporal_duration_to_string_continuation(
    state: TemporalDurationToStringContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalDurationToStringOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    reason = "one ordered state table preserves the specification's observable formatting option-read and coercion sequence across suspension"
)]
pub(super) fn advance_temporal_duration_to_string_options(
    runtime: &mut Runtime,
    mut state: TemporalDurationToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalDurationToStringStage::FractionalSecondDigits => match value {
            StoredValue::Undefined => begin_temporal_duration_to_string_get(
                runtime,
                state,
                "roundingMode",
                TemporalDurationToStringStage::RoundingMode,
                return_to,
                execution_budget,
            ),
            StoredValue::Number(number) => {
                state.precision =
                    temporal_fractional_second_digits(number, state.realm, &state.origin)?;
                begin_temporal_duration_to_string_get(
                    runtime,
                    state,
                    "roundingMode",
                    TemporalDurationToStringStage::RoundingMode,
                    return_to,
                    execution_budget,
                )
            }
            value => {
                let realm = state.realm;
                let origin = state.origin.clone();
                begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::TemporalDurationToStringFractionalSecondDigits(
                        Box::new(state),
                    ),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                )
            }
        },
        TemporalDurationToStringStage::RoundingMode => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_duration_to_string_get(
                    runtime,
                    state,
                    "smallestUnit",
                    TemporalDurationToStringStage::SmallestUnit,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalDurationToStringRoundingMode(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalDurationToStringStage::SmallestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return complete_temporal_duration_to_string(
                    state.duration,
                    state.precision,
                    state.rounding_mode,
                    state.smallest_unit,
                    state.realm,
                    &state.origin,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalDurationToStringSmallestUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion continuation retains the native formatting call context"
)]
pub(super) fn finish_temporal_duration_to_string_fractional_second_digits(
    runtime: &mut Runtime,
    mut state: TemporalDurationToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    if source.to_utf8_lossy()?.as_str() != "auto" {
        return temporal_range_error(
            state.realm,
            &state.origin,
            "fractionalSecondDigits must be a Number or the string auto",
        );
    }
    state.precision = Precision::Auto;
    begin_temporal_duration_to_string_get(
        runtime,
        state,
        "roundingMode",
        TemporalDurationToStringStage::RoundingMode,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion continuation retains the native formatting call context"
)]
pub(super) fn finish_temporal_duration_to_string_rounding_mode(
    runtime: &mut Runtime,
    mut state: TemporalDurationToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.rounding_mode = temporal_rounding_mode(&source, state.realm, &state.origin)?;
    begin_temporal_duration_to_string_get(
        runtime,
        state,
        "smallestUnit",
        TemporalDurationToStringStage::SmallestUnit,
        return_to,
        execution_budget,
    )
}

pub(super) fn finish_temporal_duration_to_string_smallest_unit(
    state: &TemporalDurationToStringContinuation,
    value: StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    let smallest_unit = temporal_round_unit(&source, state.realm, &state.origin)?;
    complete_temporal_duration_to_string(
        state.duration,
        state.precision,
        state.rounding_mode,
        Some(smallest_unit),
        state.realm,
        &state.origin,
    )
}

fn complete_temporal_duration_to_string(
    duration: Duration,
    precision: Precision,
    rounding_mode: RoundingMode,
    smallest_unit: Option<Unit>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    match smallest_unit {
        None | Some(Unit::Second | Unit::Millisecond | Unit::Microsecond | Unit::Nanosecond) => {}
        Some(
            Unit::Auto
            | Unit::Minute
            | Unit::Hour
            | Unit::Day
            | Unit::Week
            | Unit::Month
            | Unit::Year,
        ) => {
            return temporal_range_error(
                realm,
                origin,
                "smallestUnit must be second, millisecond, microsecond, or nanosecond",
            );
        }
    }
    let options = ToStringRoundingOptions {
        precision,
        smallest_unit,
        rounding_mode: Some(rounding_mode),
    };
    let rendered = match duration.as_temporal_string(options) {
        Ok(rendered) => rendered,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::String(
        JsString::from_utf8(&rendered)?,
    )))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the ordered formatting options reader owns the branded instant and its resumable call context"
)]
fn begin_temporal_instant_to_string(
    runtime: &mut Runtime,
    instant: Instant,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return complete_temporal_instant_to_string(
            instant,
            Precision::Auto,
            RoundingMode::Trunc,
            None,
            StoredValue::Undefined,
            realm,
            &origin,
        );
    }
    if options.heap_reference().is_none() {
        return temporal_type_error(
            realm,
            &origin,
            "Temporal.Instant.prototype.toString options must be an object",
        );
    }
    begin_temporal_instant_to_string_get(
        runtime,
        TemporalInstantToStringContinuation {
            instant,
            options,
            precision: Precision::Auto,
            rounding_mode: RoundingMode::Trunc,
            smallest_unit: None,
            stage: TemporalInstantToStringStage::FractionalSecondDigits,
            realm,
            origin,
        },
        "fractionalSecondDigits",
        TemporalInstantToStringStage::FractionalSecondDigits,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "each observable formatting option Get retains the complete native call context"
)]
fn begin_temporal_instant_to_string_get(
    runtime: &mut Runtime,
    mut state: TemporalInstantToStringContinuation,
    name: &str,
    next_stage: TemporalInstantToStringStage,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    state.stage = next_stage;
    charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
    let name = JsString::from_utf8(name)?;
    let key = runtime.property_key_from_string(&name)?;
    let realm = state.realm;
    let origin = state.origin.clone();
    let dispatch = begin_value_get(
        runtime,
        &state.options,
        key,
        Some(&name),
        realm,
        return_to,
        origin,
        execution_budget,
    )?;
    match continue_get_state_after(
        dispatch,
        state,
        temporal_instant_to_string_continuation,
        "Temporal.Instant toString option Get produced a structured result",
    )? {
        GetContinuationDispatch::Ready { state, value } => {
            advance_temporal_instant_to_string_options(
                runtime,
                state,
                value,
                return_to,
                execution_budget,
            )
        }
        GetContinuationDispatch::Suspended(dispatch) => Ok(dispatch),
    }
}

fn temporal_instant_to_string_continuation(
    state: TemporalInstantToStringContinuation,
) -> NativeContinuation {
    NativeContinuation::TemporalInstantToStringOptions(Box::new(state))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one ordered state table preserves the specification's observable formatting option-read and coercion sequence across suspension"
)]
pub(super) fn advance_temporal_instant_to_string_options(
    runtime: &mut Runtime,
    mut state: TemporalInstantToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalInstantToStringStage::FractionalSecondDigits => match value {
            StoredValue::Undefined => begin_temporal_instant_to_string_get(
                runtime,
                state,
                "roundingMode",
                TemporalInstantToStringStage::RoundingMode,
                return_to,
                execution_budget,
            ),
            StoredValue::Number(number) => {
                state.precision =
                    temporal_fractional_second_digits(number, state.realm, &state.origin)?;
                begin_temporal_instant_to_string_get(
                    runtime,
                    state,
                    "roundingMode",
                    TemporalInstantToStringStage::RoundingMode,
                    return_to,
                    execution_budget,
                )
            }
            value => {
                let realm = state.realm;
                let origin = state.origin.clone();
                begin_operator_primitive_conversion(
                    runtime,
                    value,
                    OperatorPrimitiveHint::String,
                    OperatorPrimitiveTarget::TemporalInstantToStringFractionalSecondDigits(
                        Box::new(state),
                    ),
                    realm,
                    return_to,
                    origin,
                    execution_budget,
                )
            }
        },
        TemporalInstantToStringStage::RoundingMode => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_instant_to_string_get(
                    runtime,
                    state,
                    "smallestUnit",
                    TemporalInstantToStringStage::SmallestUnit,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalInstantToStringRoundingMode(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalInstantToStringStage::SmallestUnit => {
            if matches!(value, StoredValue::Undefined) {
                return begin_temporal_instant_to_string_get(
                    runtime,
                    state,
                    "timeZone",
                    TemporalInstantToStringStage::TimeZone,
                    return_to,
                    execution_budget,
                );
            }
            let realm = state.realm;
            let origin = state.origin.clone();
            begin_operator_primitive_conversion(
                runtime,
                value,
                OperatorPrimitiveHint::String,
                OperatorPrimitiveTarget::TemporalInstantToStringSmallestUnit(Box::new(state)),
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        TemporalInstantToStringStage::TimeZone => complete_temporal_instant_to_string(
            state.instant,
            state.precision,
            state.rounding_mode,
            state.smallest_unit,
            value,
            state.realm,
            &state.origin,
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion continuation retains the native formatting call context"
)]
pub(super) fn finish_temporal_instant_to_string_fractional_second_digits(
    runtime: &mut Runtime,
    mut state: TemporalInstantToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    if source.to_utf8_lossy()?.as_str() != "auto" {
        return temporal_range_error(
            state.realm,
            &state.origin,
            "fractionalSecondDigits must be a Number or the string auto",
        );
    }
    state.precision = Precision::Auto;
    begin_temporal_instant_to_string_get(
        runtime,
        state,
        "roundingMode",
        TemporalInstantToStringStage::RoundingMode,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion continuation retains the native formatting call context"
)]
pub(super) fn finish_temporal_instant_to_string_rounding_mode(
    runtime: &mut Runtime,
    mut state: TemporalInstantToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.rounding_mode = temporal_rounding_mode(&source, state.realm, &state.origin)?;
    begin_temporal_instant_to_string_get(
        runtime,
        state,
        "smallestUnit",
        TemporalInstantToStringStage::SmallestUnit,
        return_to,
        execution_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the post-coercion continuation retains the native formatting call context"
)]
pub(super) fn finish_temporal_instant_to_string_smallest_unit(
    runtime: &mut Runtime,
    mut state: TemporalInstantToStringContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let source = operator_primitive_to_string(value, state.realm, &state.origin)?;
    state.smallest_unit = Some(temporal_round_unit(&source, state.realm, &state.origin)?);
    begin_temporal_instant_to_string_get(
        runtime,
        state,
        "timeZone",
        TemporalInstantToStringStage::TimeZone,
        return_to,
        execution_budget,
    )
}

fn temporal_fractional_second_digits(
    value: JsNumber,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Precision, NativeFailure> {
    let value = value.as_f64();
    if !value.is_finite() {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "fractionalSecondDigits must be finite",
        )?));
    }
    let digits = value.floor();
    if !(0.0..=9.0).contains(&digits) {
        return Err(NativeFailure::Abrupt(temporal_pending_exception(
            realm,
            origin,
            ExceptionKind::RangeError,
            "fractionalSecondDigits must be between zero and nine",
        )?));
    }
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "the validated integer digit count is in the inclusive u8 range zero through nine"
    )]
    let digits = digits as u8;
    Ok(Precision::Digit(digits))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the completed JavaScript formatting options are passed explicitly to the shared temporal kernel"
)]
fn complete_temporal_instant_to_string(
    instant: Instant,
    precision: Precision,
    rounding_mode: RoundingMode,
    smallest_unit: Option<Unit>,
    time_zone: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    match smallest_unit {
        None
        | Some(
            Unit::Minute | Unit::Second | Unit::Millisecond | Unit::Microsecond | Unit::Nanosecond,
        ) => {}
        Some(Unit::Auto | Unit::Hour | Unit::Day | Unit::Week | Unit::Month | Unit::Year) => {
            return temporal_range_error(
                realm,
                origin,
                "smallestUnit must be minute, second, millisecond, microsecond, or nanosecond",
            );
        }
    }
    let time_zone = match time_zone {
        StoredValue::Undefined => None,
        StoredValue::String(source) => {
            let source = source.to_utf8_lossy()?;
            match TimeZone::try_from_str(&source) {
                Ok(time_zone) => Some(time_zone),
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                        realm, origin, error,
                    )?));
                }
            }
        }
        _ => {
            return temporal_type_error(
                realm,
                origin,
                "Temporal.Instant.prototype.toString timeZone must be a string",
            );
        }
    };
    let options = ToStringRoundingOptions {
        precision,
        smallest_unit,
        rounding_mode: Some(rounding_mode),
    };
    let rendered = match instant.to_ixdtf_string(time_zone, options) {
        Ok(rendered) => rendered,
        Err(error) => {
            return Err(NativeFailure::Abrupt(temporal_exception_from_error(
                realm, origin, error,
            )?));
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::String(
        JsString::from_utf8(&rendered)?,
    )))
}

fn temporal_range_exception_from_error(
    realm: RealmId,
    origin: &JsStackFrame,
    error: temporal_rs::TemporalError,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::RangeError,
            message: JsString::from_utf8(error.into_message())?,
        },
        origin: origin.clone(),
    })
}

fn require_temporal_instant(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Instant, NativeFailure> {
    if let StoredValue::Object(object) = receiver
        && let Some(instant) = runtime.temporal_instant(*object)?
    {
        return Ok(instant);
    }
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8("not a Temporal.Instant object")?,
        },
        origin: origin.clone(),
    }))
}

fn temporal_exception_from_error(
    realm: RealmId,
    origin: &JsStackFrame,
    error: temporal_rs::TemporalError,
) -> Result<PendingException, NativeFailure> {
    let kind = match error.kind() {
        TemporalErrorKind::Type => ExceptionKind::TypeError,
        TemporalErrorKind::Syntax => ExceptionKind::SyntaxError,
        TemporalErrorKind::Generic | TemporalErrorKind::Range | TemporalErrorKind::Assert => {
            ExceptionKind::RangeError
        }
    };
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(error.into_message())?,
        },
        origin: origin.clone(),
    })
}

fn temporal_type_error(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    temporal_exception(realm, origin, ExceptionKind::TypeError, message)
}

fn temporal_range_error(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    temporal_exception(realm, origin, ExceptionKind::RangeError, message)
}

fn temporal_exception(
    realm: RealmId,
    origin: &JsStackFrame,
    kind: ExceptionKind,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(temporal_pending_exception(
        realm, origin, kind, message,
    )?))
}

fn temporal_pending_exception(
    realm: RealmId,
    origin: &JsStackFrame,
    kind: ExceptionKind,
    message: &str,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    })
}
