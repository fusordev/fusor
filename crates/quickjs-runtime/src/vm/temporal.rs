//! Initial `Temporal.Instant` JavaScript boundary over `temporal_rs`.

use temporal_rs::{
    Instant, error::ErrorKind as TemporalErrorKind, options::ToStringRoundingOptions,
};

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

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

pub(super) fn dispatch_temporal_instant_prototype(
    runtime: &Runtime,
    method: TemporalInstantPrototypeMethod,
    realm: RealmId,
    receiver: &StoredValue,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    let instant = require_temporal_instant(runtime, receiver, realm, origin)?;
    match method {
        TemporalInstantPrototypeMethod::EpochMilliseconds => Ok(NativeDispatch::Immediate(
            StoredValue::Number(JsNumber::from_i64(instant.epoch_milliseconds())),
        )),
        TemporalInstantPrototypeMethod::EpochNanoseconds => Ok(NativeDispatch::Immediate(
            StoredValue::BigInt(Arc::new(JsBigInt::from_i128(instant.as_i128()))),
        )),
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
