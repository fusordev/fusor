use super::super::conversions::operator_primitive_to_string;
#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use temporal_rs::{
    Calendar, Instant, TimeZone, ZonedDateTime,
    options::{
        DifferenceSettings, RoundingIncrement, RoundingMode, RoundingOptions,
        ToStringRoundingOptions, Unit,
    },
    parsers::Precision,
};

#[derive(Clone, Copy)]
enum TemporalInstantRoundStage {
    RoundingIncrement,
    RoundingMode,
    SmallestUnit,
}

pub(in crate::vm) struct TemporalInstantRoundContinuation {
    instant: Instant,
    options: StoredValue,
    rounding_increment: RoundingIncrement,
    rounding_mode: RoundingMode,
    stage: TemporalInstantRoundStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl TemporalInstantRoundContinuation {
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
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

pub(in crate::vm) struct TemporalInstantDifferenceContinuation {
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
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
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

pub(in crate::vm) struct TemporalInstantToStringContinuation {
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
    pub(in crate::vm) const fn retained_values() -> u64 {
        1
    }

    pub(in crate::vm) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
    }
}

pub(in crate::vm) fn begin_temporal_instant_constructor(
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

pub(in crate::vm) fn begin_temporal_instant_static(
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
    if let StoredValue::Object(object) = value {
        if let Some(instant) = runtime.temporal_instant(object)? {
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
        // ToTemporalInstant: a branded ZonedDateTime contributes its exact
        // epoch-nanoseconds slot without any observable property reads.
        if let Some(date_time) = runtime.temporal_zoned_date_time(object)? {
            let instant = match Instant::try_new(date_time.epoch_nanoseconds().as_i128()) {
                Ok(instant) => instant,
                Err(error) => {
                    return Err(NativeFailure::Abrupt(temporal_range_exception_from_error(
                        realm, &origin, error,
                    )?));
                }
            };
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
pub(in crate::vm) fn finish_temporal_instant_string(
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
pub(in crate::vm) fn finish_temporal_instant_nanoseconds(
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

pub(in crate::vm) fn finish_temporal_instant_milliseconds(
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

pub(in crate::vm) fn finish_temporal_instant_constructor_wrapper(
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

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the closed Temporal.Instant prototype dispatcher preserves receiver validation and explicit call context"
)]
pub(in crate::vm) fn dispatch_temporal_instant_prototype(
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
        TemporalInstantPrototypeMethod::ToLocaleString => begin_intl_temporal_to_locale_string(
            runtime,
            IntlDateTimeFormatLocaleValue::Instant(instant),
            arguments,
            realm,
            return_to,
            origin.clone(),
            execution_budget,
        ),
        TemporalInstantPrototypeMethod::ToZonedDateTimeISO => {
            let value = arguments.take_first_or_undefined();
            let time_zone =
                temporal_zoned_date_time_time_zone_from_value(runtime, value, realm, origin)?;
            let date_time =
                match ZonedDateTime::try_new(instant.as_i128(), time_zone, Calendar::default()) {
                    Ok(date_time) => date_time,
                    Err(error) => {
                        return Err(NativeFailure::Abrupt(temporal_range_exception_from_error(
                            realm, origin, error,
                        )?));
                    }
                };
            allocate_temporal_zoned_date_time_result(runtime, realm, date_time)
        }
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
pub(in crate::vm) fn advance_temporal_instant_round_options(
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
pub(in crate::vm) fn finish_temporal_instant_round_rounding_increment(
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
pub(in crate::vm) fn finish_temporal_instant_round_rounding_mode(
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
pub(in crate::vm) fn finish_temporal_instant_round_smallest_unit(
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
pub(in crate::vm) fn advance_temporal_instant_difference_options(
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
pub(in crate::vm) fn finish_temporal_instant_difference_largest_unit(
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
pub(in crate::vm) fn finish_temporal_instant_difference_rounding_increment(
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
pub(in crate::vm) fn finish_temporal_instant_difference_rounding_mode(
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

pub(in crate::vm) fn finish_temporal_instant_difference_smallest_unit(
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
pub(in crate::vm) fn advance_temporal_instant_to_string_options(
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
pub(in crate::vm) fn finish_temporal_instant_to_string_fractional_second_digits(
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
pub(in crate::vm) fn finish_temporal_instant_to_string_rounding_mode(
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
pub(in crate::vm) fn finish_temporal_instant_to_string_smallest_unit(
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
