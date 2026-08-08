use super::super::conversions::operator_primitive_to_string;
#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use temporal_rs::{
    Duration, Instant, PlainDate, PlainDateTime, PlainTime, PlainYearMonth, Sign, ZonedDateTime,
    options::{
        RelativeTo, RoundingIncrement, RoundingMode, RoundingOptions, ToStringRoundingOptions, Unit,
    },
    parsers::Precision,
};

pub(in crate::vm) struct TemporalDurationConstructorContinuation {
    arguments: Vec<StoredValue>,
    converted: Vec<JsNumber>,
    new_target: FunctionId,
}

impl TemporalDurationConstructorContinuation {
    pub(in crate::vm) fn retained_values(&self) -> u64 {
        usize_to_u64(self.arguments.len()).saturating_add(1)
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
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

pub(in crate::vm) enum TemporalDurationLikeTarget {
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
    PlainTimeArithmetic {
        receiver: PlainTime,
        subtract: bool,
    },
    PlainDateArithmetic {
        receiver: PlainDate,
        subtract: bool,
        options: StoredValue,
    },
    PlainDateTimeArithmetic {
        receiver: PlainDateTime,
        subtract: bool,
        options: StoredValue,
    },
    PlainYearMonthArithmetic {
        receiver: PlainYearMonth,
        subtract: bool,
        options: StoredValue,
    },
    ZonedDateTimeArithmetic {
        receiver: ZonedDateTime,
        subtract: bool,
        options: StoredValue,
    },
}

impl TemporalDurationLikeTarget {
    fn retained_values(&self) -> u64 {
        match self {
            Self::Allocate
            | Self::Arithmetic { .. }
            | Self::InstantArithmetic { .. }
            | Self::PlainTimeArithmetic { .. } => 0,
            Self::CompareFirst { .. } => 2,
            Self::CompareSecond { .. }
            | Self::PlainDateArithmetic { .. }
            | Self::PlainDateTimeArithmetic { .. }
            | Self::PlainYearMonthArithmetic { .. }
            | Self::ZonedDateTimeArithmetic { .. } => 1,
        }
    }

    fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        match self {
            Self::Allocate
            | Self::Arithmetic { .. }
            | Self::InstantArithmetic { .. }
            | Self::PlainTimeArithmetic { .. } => {}
            Self::CompareFirst { second, options } => {
                trace_stored_value_root(second, mark);
                trace_stored_value_root(options, mark);
            }
            Self::CompareSecond { options, .. }
            | Self::PlainDateArithmetic { options, .. }
            | Self::PlainDateTimeArithmetic { options, .. }
            | Self::PlainYearMonthArithmetic { options, .. }
            | Self::ZonedDateTimeArithmetic { options, .. } => {
                trace_stored_value_root(options, mark);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum TemporalDurationBagStage {
    ReadField,
    AwaitField,
    AwaitConversion,
}

pub(in crate::vm) struct TemporalDurationBagContinuation {
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
    pub(in crate::vm) fn retained_values(&self) -> u64 {
        1_u64.saturating_add(self.target.retained_values())
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.base, mark);
        self.target.trace_roots(mark);
    }
}

pub(in crate::vm) struct TemporalDurationCompareOptionsContinuation {
    first: Duration,
    second: Duration,
    options: StoredValue,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalDurationCompareOptionsContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalDurationTotalStage {
    AwaitRelativeTo,
    AwaitUnit,
}

pub(in crate::vm) struct TemporalDurationTotalContinuation {
    duration: Duration,
    options: StoredValue,
    relative_to: Option<RelativeTo>,
    stage: TemporalDurationTotalStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalDurationTotalContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
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

pub(in crate::vm) struct TemporalDurationRoundContinuation {
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
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[derive(Clone, Copy)]
enum TemporalDurationToStringStage {
    FractionalSecondDigits,
    RoundingMode,
    SmallestUnit,
}

pub(in crate::vm) struct TemporalDurationToStringContinuation {
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
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "native entry points own their source frame and may retain it across suspension"
)]
pub(in crate::vm) fn begin_temporal_duration_constructor(
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
pub(in crate::vm) fn advance_temporal_duration_constructor(
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

pub(in crate::vm) fn finish_temporal_duration_constructor_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    duration: Duration,
    requested: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let duration = normalize_temporal_duration_fields(duration)?;
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
pub(in crate::vm) fn dispatch_temporal_duration_prototype(
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
pub(in crate::vm) fn advance_temporal_duration_round_options(
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
                temporal_relative_to_from_value(runtime, &value, state.realm, &state.origin)?;
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
pub(in crate::vm) fn finish_temporal_duration_round_largest_unit(
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
pub(in crate::vm) fn finish_temporal_duration_round_rounding_increment(
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
pub(in crate::vm) fn finish_temporal_duration_round_rounding_mode(
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
pub(in crate::vm) fn finish_temporal_duration_round_smallest_unit(
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

pub(in crate::vm) fn advance_temporal_duration_total_options(
    runtime: &mut Runtime,
    mut state: TemporalDurationTotalContinuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        TemporalDurationTotalStage::AwaitRelativeTo => {
            state.relative_to =
                temporal_relative_to_from_value(runtime, &value, state.realm, &state.origin)?;
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

pub(in crate::vm) fn finish_temporal_duration_total_unit(
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

/// Temporal's internal Duration fields are ECMAScript Numbers. The Rust
/// kernel keeps exact integer intermediates, so normalize any kernel result
/// before it becomes observable to JavaScript or feeds later Duration calls.
pub(in crate::vm) fn normalize_temporal_duration_fields(
    duration: Duration,
) -> Result<Duration, NativeFailure> {
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "the intermediate f64 is the ECMAScript Number representation"
    )]
    let number_i64 = |value: i64| value as f64 as i64;
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "the intermediate f64 is the ECMAScript Number representation"
    )]
    let number_i128 = |value: i128| value as f64 as i128;
    Duration::new(
        number_i64(duration.years()),
        number_i64(duration.months()),
        number_i64(duration.weeks()),
        number_i64(duration.days()),
        number_i64(duration.hours()),
        number_i64(duration.minutes()),
        number_i64(duration.seconds()),
        number_i64(duration.milliseconds()),
        number_i128(duration.microseconds()),
        number_i128(duration.nanoseconds()),
    )
    .map_err(|_| {
        EngineFault::RuntimeInvariant {
            message: "normalizing a valid Temporal.Duration to ECMAScript Number fields failed",
        }
        .into()
    })
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "native entry points receive one owned source frame consistently"
)]
pub(in crate::vm) fn dispatch_temporal_duration_static(
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
pub(in crate::vm) fn begin_temporal_duration_like(
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
pub(in crate::vm) fn advance_temporal_duration_property_bag(
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
    clippy::too_many_lines,
    reason = "one exhaustive conversion dispatcher preserves target-specific completion and native suspension context"
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
        TemporalDurationLikeTarget::PlainTimeArithmetic { receiver, subtract } => {
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
            allocate_temporal_plain_time_result(runtime, realm, result)
        }
        TemporalDurationLikeTarget::PlainDateArithmetic {
            receiver,
            subtract,
            options,
        } => begin_temporal_plain_date_from_options(
            runtime,
            TemporalPlainDateOverflowTarget::Arithmetic {
                receiver,
                duration,
                subtract,
            },
            options,
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalDurationLikeTarget::PlainDateTimeArithmetic {
            receiver,
            subtract,
            options,
        } => begin_temporal_plain_date_from_options(
            runtime,
            TemporalPlainDateOverflowTarget::DateTimeArithmetic {
                receiver,
                duration,
                subtract,
            },
            options,
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalDurationLikeTarget::PlainYearMonthArithmetic {
            receiver,
            subtract,
            options,
        } => begin_temporal_plain_date_from_options(
            runtime,
            TemporalPlainDateOverflowTarget::YearMonthArithmetic {
                receiver,
                duration,
                subtract,
            },
            options,
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalDurationLikeTarget::ZonedDateTimeArithmetic {
            receiver,
            subtract,
            options,
        } => begin_temporal_plain_date_from_options(
            runtime,
            TemporalPlainDateOverflowTarget::ZonedDateTimeArithmetic {
                receiver,
                duration,
                subtract,
            },
            options,
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
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

pub(in crate::vm) fn finish_temporal_duration_compare_options(
    runtime: &mut Runtime,
    state: &TemporalDurationCompareOptionsContinuation,
    relative_to: &StoredValue,
) -> Result<NativeDispatch, NativeFailure> {
    let relative_to =
        temporal_relative_to_from_value(runtime, relative_to, state.realm, &state.origin)?;
    complete_temporal_duration_compare(
        state.first,
        state.second,
        relative_to,
        state.realm,
        &state.origin,
    )
}

fn temporal_relative_to_from_value(
    runtime: &Runtime,
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
        StoredValue::Object(object) => {
            if let Some(date) = runtime.temporal_plain_date(*object)? {
                return Ok(Some(RelativeTo::PlainDate(date)));
            }
            if let Some(date_time) = runtime.temporal_plain_date_time(*object)? {
                return Ok(Some(RelativeTo::PlainDate(date_time.to_plain_date())));
            }
            Err(NativeFailure::Abrupt(temporal_pending_exception(
                realm,
                origin,
                ExceptionKind::TypeError,
                "Temporal relativeTo must be a string or Temporal object",
            )?))
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
pub(in crate::vm) fn advance_temporal_duration_to_string_options(
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
pub(in crate::vm) fn finish_temporal_duration_to_string_fractional_second_digits(
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
pub(in crate::vm) fn finish_temporal_duration_to_string_rounding_mode(
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

pub(in crate::vm) fn finish_temporal_duration_to_string_smallest_unit(
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
