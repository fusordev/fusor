#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;
use temporal_rs::{Temporal, TemporalResult, TimeZone};

/// Dispatches the six `%Temporal.Now%` methods against one host-clock sample.
///
/// Explicit time-zone arguments reuse the shared `TimeZone` slot conversion;
/// omitted arguments let `temporal_rs` resolve the host's system zone.
pub(in crate::vm) fn dispatch_temporal_now(
    runtime: &mut Runtime,
    method: TemporalNowMethod,
    realm: RealmId,
    mut arguments: CallArguments,
    origin: &JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        TemporalNowMethod::Instant => {
            let instant = temporal_now_kernel(Temporal::utc_now().instant(), realm, origin)?;
            allocate_temporal_instant_result(runtime, realm, instant)
        }
        TemporalNowMethod::PlainDateIso => {
            let time_zone = temporal_now_time_zone(runtime, &mut arguments, realm, origin)?;
            let date = temporal_now_kernel(
                Temporal::local_now().plain_date_iso(time_zone),
                realm,
                origin,
            )?;
            allocate_temporal_plain_date_result(runtime, realm, date)
        }
        TemporalNowMethod::PlainDateTimeIso => {
            let time_zone = temporal_now_time_zone(runtime, &mut arguments, realm, origin)?;
            let date_time = temporal_now_kernel(
                Temporal::local_now().plain_date_time_iso(time_zone),
                realm,
                origin,
            )?;
            allocate_temporal_plain_date_time_result(runtime, realm, date_time)
        }
        TemporalNowMethod::PlainTimeIso => {
            let time_zone = temporal_now_time_zone(runtime, &mut arguments, realm, origin)?;
            let time = temporal_now_kernel(
                Temporal::local_now().plain_time_iso(time_zone),
                realm,
                origin,
            )?;
            allocate_temporal_plain_time_result(runtime, realm, time)
        }
        TemporalNowMethod::TimeZoneId => {
            let time_zone = temporal_now_kernel(Temporal::local_now().time_zone(), realm, origin)?;
            let identifier = temporal_now_kernel(time_zone.identifier(), realm, origin)?;
            Ok(NativeDispatch::Immediate(StoredValue::String(
                JsString::from_utf8(&identifier)?,
            )))
        }
        TemporalNowMethod::ZonedDateTimeIso => {
            let time_zone = temporal_now_time_zone(runtime, &mut arguments, realm, origin)?;
            let date_time = temporal_now_kernel(
                Temporal::local_now().zoned_date_time_iso(time_zone),
                realm,
                origin,
            )?;
            allocate_temporal_zoned_date_time_result(runtime, realm, date_time)
        }
    }
}

fn temporal_now_time_zone(
    runtime: &Runtime,
    arguments: &mut CallArguments,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Option<TimeZone>, NativeFailure> {
    let value = arguments.take_first_or_undefined();
    if matches!(value, StoredValue::Undefined) {
        return Ok(None);
    }
    temporal_zoned_date_time_time_zone_from_value(runtime, value, realm, origin).map(Some)
}

fn temporal_now_kernel<T>(
    result: TemporalResult<T>,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<T, NativeFailure> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => Err(NativeFailure::Abrupt(temporal_exception_from_error(
            realm, origin, error,
        )?)),
    }
}
