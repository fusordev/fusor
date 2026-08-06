//! Initial `Temporal.Instant` JavaScript boundary over `temporal_rs`.

use temporal_rs::{
    Duration, Instant, Sign, error::ErrorKind as TemporalErrorKind,
    options::ToStringRoundingOptions,
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

    let mut fields = [0_i64; 10];
    for (field, value) in fields.iter_mut().zip(&state.converted) {
        let value = value.as_f64();
        if !value.is_finite() || value.fract() != 0.0 || value.abs() > 9_007_199_254_740_991.0 {
            return temporal_range_error(
                realm,
                origin,
                "Temporal.Duration fields must be finite integral Numbers",
            );
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the preceding safe-integer validation places the value inside i64"
        )]
        {
            *field = value as i64;
        }
    }
    let duration = match Duration::new(
        fields[0],
        fields[1],
        fields[2],
        fields[3],
        fields[4],
        fields[5],
        fields[6],
        fields[7],
        i128::from(fields[8]),
        i128::from(fields[9]),
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

pub(super) fn dispatch_temporal_duration_prototype(
    runtime: &mut Runtime,
    method: TemporalDurationPrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    origin: &JsStackFrame,
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
        TemporalDurationPrototypeMethod::ToString
        | TemporalDurationPrototypeMethod::ToJson
        | TemporalDurationPrototypeMethod::ToLocaleString => Ok(NativeDispatch::Immediate(
            StoredValue::String(JsString::from_utf8(&duration.to_string())?),
        )),
        TemporalDurationPrototypeMethod::ValueOf => temporal_type_error(
            realm,
            origin,
            "Temporal.Duration cannot be converted to a primitive value",
        ),
    }
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
        TemporalInstantPrototypeMethod::ToString | TemporalInstantPrototypeMethod::ToJson => {
            let rendered = match instant.to_ixdtf_string(None, ToStringRoundingOptions::default()) {
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
        TemporalInstantPrototypeMethod::ValueOf => temporal_type_error(
            realm,
            origin,
            "Temporal.Instant cannot be converted to a primitive value",
        ),
    }
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
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    }))
}
