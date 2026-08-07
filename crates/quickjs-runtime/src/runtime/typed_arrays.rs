//! Integer-indexed typed-array allocation, virtual properties, and storage.
//!
//! A typed array owns no element property slots. Its `ArrayBuffer` owns every
//! byte, while the typed-array object retains only the view metadata. This
//! module therefore centralizes the fresh-buffer witness needed by every
//! integer-indexed operation: a resizable buffer may make a fixed view
//! out-of-bounds between two observable user conversions.

use std::sync::Arc;

use super::{
    ForInSnapshot, HeapObject, HeapReference, KeyPhases, ObjectId, ObjectRecord, OwnProperty,
    PropertyKey, PropertyLayout, Runtime, RuntimeResource, StoredValue, TypedArrayState,
    check_execution_limit, stale_heap_reference, usize_to_u64,
};
use crate::{
    ArrayIndex, AtomKind, ExecutionError, JsBigInt, JsNumber,
    conversion::{
        canonical_numeric_index_string, number_to_index, number_to_int8, number_to_int16,
        number_to_int32, number_to_uint8, number_to_uint8_clamp, number_to_uint16,
        number_to_uint32,
    },
    object::{TypedArrayElementType, TypedArrayLength},
};

/// A fresh validation witness for a typed-array view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypedArrayView {
    Detached,
    OutOfBounds,
    InBounds {
        buffer: ObjectId,
        byte_offset: usize,
        length: usize,
        element: TypedArrayElementType,
    },
}

/// How a key is interpreted by the integer-indexed exotic internal methods.
///
/// `Invalid` is intentionally distinct from `Ordinary`: `"-0"`, `"NaN"`,
/// and `"0.5"` are canonical numeric index strings but never ordinary
/// properties of a typed array, including when a prototype has that key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypedArrayPropertyKey {
    Ordinary,
    Invalid,
    Index(usize),
}

/// The typed-array `[[GetOwnProperty]]` result before an ordinary record is
/// consulted. A canonical numeric key remains on this path even when its
/// element is absent because the view is detached, out-of-bounds, or too
/// short.
pub(crate) enum TypedArrayOwnProperty {
    Ordinary,
    IntegerIndexed(Option<OwnProperty>),
}

/// A pre-converted element value supplied by the VM after `ToNumber` or
/// `ToBigInt`. The element-store core never performs JavaScript coercion, so
/// it cannot reorder user code around the fresh bounds witness.
pub(crate) enum TypedArrayElementValue<'a> {
    Number(JsNumber),
    BigInt(&'a JsBigInt),
}

/// Result of an already-converted integer-indexed element store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypedArrayStoreOutcome {
    Stored,
    Missing,
    ContentTypeMismatch,
}

impl Runtime {
    /// Allocates an integer-indexed typed-array exotic after all observable
    /// constructor conversions and bounds checks have completed.
    #[allow(
        dead_code,
        reason = "the typed-array storage core is committed before its Realm constructors are exposed"
    )]
    pub(crate) fn allocate_typed_array(
        &mut self,
        prototype: HeapReference,
        state: TypedArrayState,
    ) -> Result<ObjectId, ExecutionError> {
        if !self.heap_reference_is_live(prototype) {
            return Err(stale_heap_reference(prototype).into());
        }
        if self.objects.get(state.buffer()).is_none() {
            return Err(crate::EngineFault::StaleHeapEdge {
                edge: "TypedArray backing buffer",
                index: state.buffer().index(),
                generation: state.buffer().generation(),
            }
            .into());
        }
        if self.array_buffer_state(state.buffer())?.is_none() {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "TypedArray backing store is not an ArrayBuffer",
            }
            .into());
        }
        check_execution_limit(
            RuntimeResource::HeapObjects,
            self.limits.max_heap_objects,
            usize_to_u64(self.objects.len()).saturating_add(1),
        )?;
        self.objects
            .try_reserve(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        let object = self
            .insert_heap_object(HeapObject::typed_array(
                ObjectRecord::empty(Some(prototype)),
                state,
            ))
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        self.collection_pending = true;
        Ok(object)
    }

    pub(crate) fn typed_array_state(
        &self,
        object: ObjectId,
    ) -> Result<Option<&TypedArrayState>, crate::EngineFault> {
        self.objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
            .map(HeapObject::typed_array_state)
    }

    /// Implements `IsTypedArrayFixedLength` for the ArrayBuffer-backed view
    /// forms presently supported by the runtime. Length-tracking views and
    /// views backed by resizable buffers cannot be made non-extensible because
    /// their virtual indexed-property set can still change after a resize.
    pub(crate) fn typed_array_is_fixed_length(
        &self,
        object: ObjectId,
    ) -> Result<Option<bool>, crate::EngineFault> {
        let Some(state) = self.typed_array_state(object)? else {
            return Ok(None);
        };
        let buffer = self.array_buffer_state(state.buffer())?.ok_or(
            crate::EngineFault::RuntimeInvariant {
                message: "TypedArray backing buffer lost its ArrayBuffer slots",
            },
        )?;
        Ok(Some(
            matches!(state.length(), TypedArrayLength::Fixed(_)) && !buffer.is_resizable(),
        ))
    }

    /// Takes the current backing-buffer witness used by every typed-array
    /// indexed operation. A length-tracking view observes the largest complete
    /// element prefix after each resizable-buffer change; a fixed view becomes
    /// out of bounds as soon as its full byte range no longer fits.
    pub(crate) fn typed_array_view(
        &self,
        object: ObjectId,
    ) -> Result<TypedArrayView, crate::EngineFault> {
        let state = self.typed_array_state(object)?.copied().ok_or(
            crate::EngineFault::RuntimeInvariant {
                message: "typed-array view lookup received a non-typed-array object",
            },
        )?;
        let buffer = self.array_buffer_state(state.buffer())?.ok_or(
            crate::EngineFault::RuntimeInvariant {
                message: "TypedArray backing buffer lost its ArrayBuffer slots",
            },
        )?;
        if buffer.is_detached() {
            return Ok(TypedArrayView::Detached);
        }
        let byte_length = buffer.byte_length();
        if state.byte_offset() > byte_length {
            return Ok(TypedArrayView::OutOfBounds);
        }
        let element_width = state.element().byte_width();
        let length = match state.length() {
            TypedArrayLength::Auto => {
                byte_length.saturating_sub(state.byte_offset()) / element_width
            }
            TypedArrayLength::Fixed(length) => {
                let Some(view_byte_length) = length.checked_mul(element_width) else {
                    return Ok(TypedArrayView::OutOfBounds);
                };
                let Some(end) = state.byte_offset().checked_add(view_byte_length) else {
                    return Ok(TypedArrayView::OutOfBounds);
                };
                if end > byte_length {
                    return Ok(TypedArrayView::OutOfBounds);
                }
                length
            }
        };
        Ok(TypedArrayView::InBounds {
            buffer: state.buffer(),
            byte_offset: state.byte_offset(),
            length,
            element: state.element(),
        })
    }

    /// Classifies a key for an existing typed array. Symbol and non-canonical
    /// string keys stay ordinary; canonical non-indices are blocked by the
    /// integer-indexed exotic rather than falling through to a prototype.
    pub(crate) fn typed_array_property_key(
        &self,
        object: ObjectId,
        key: &PropertyKey,
    ) -> Result<Option<TypedArrayPropertyKey>, ExecutionError> {
        if self.typed_array_state(object)?.is_none() {
            return Ok(None);
        }
        Ok(Some(typed_array_property_key(key)?))
    }

    /// Resolves the virtual own property of an integer-indexed exotic without
    /// ever materializing the element in the shape table.
    pub(crate) fn typed_array_own_property(
        &self,
        object: ObjectId,
        key: &PropertyKey,
    ) -> Result<TypedArrayOwnProperty, ExecutionError> {
        let Some(key) = self.typed_array_property_key(object, key)? else {
            return Ok(TypedArrayOwnProperty::Ordinary);
        };
        let TypedArrayPropertyKey::Index(index) = key else {
            return Ok(match key {
                TypedArrayPropertyKey::Ordinary => TypedArrayOwnProperty::Ordinary,
                TypedArrayPropertyKey::Invalid => TypedArrayOwnProperty::IntegerIndexed(None),
                TypedArrayPropertyKey::Index(_) => unreachable!("matched above"),
            });
        };
        let property = self
            .typed_array_read_index(object, index)?
            .map(|value| OwnProperty::Data {
                // Typed-array elements are unusual: their own descriptors are
                // configurable even though the integer-indexed exotic `[[Delete]]`
                // operation still refuses to remove an in-bounds element.
                layout: PropertyLayout::data(true, true, true),
                value,
            });
        Ok(TypedArrayOwnProperty::IntegerIndexed(property))
    }

    /// Reads one element after taking a fresh view witness. A detached,
    /// out-of-bounds, or over-length integer index is absent rather than an
    /// error, exactly as `IntegerIndexedElementGet` specifies.
    pub(crate) fn typed_array_read_index(
        &self,
        object: ObjectId,
        index: usize,
    ) -> Result<Option<StoredValue>, crate::EngineFault> {
        let TypedArrayView::InBounds {
            buffer,
            byte_offset,
            length,
            element,
        } = self.typed_array_view(object)?
        else {
            return Ok(None);
        };
        if index >= length {
            return Ok(None);
        }
        let byte_index = typed_array_element_byte_index(byte_offset, index, element)?;
        let data = self
            .array_buffer_state(buffer)?
            .and_then(|state| state.data())
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "typed-array read lost its validated backing store",
            })?;
        typed_array_read_element(data, byte_index, element).map(Some)
    }

    /// Stores a pre-converted element after taking a fresh view witness. The
    /// caller distinguishes `ContentTypeMismatch` so Number and BigInt views
    /// report the ECMAScript `ToNumber`/`ToBigInt` domain error at the VM edge.
    pub(crate) fn typed_array_store_index(
        &mut self,
        object: ObjectId,
        index: usize,
        value: TypedArrayElementValue<'_>,
    ) -> Result<TypedArrayStoreOutcome, crate::EngineFault> {
        let TypedArrayView::InBounds {
            buffer,
            byte_offset,
            length,
            element,
        } = self.typed_array_view(object)?
        else {
            return Ok(TypedArrayStoreOutcome::Missing);
        };
        if index >= length {
            return Ok(TypedArrayStoreOutcome::Missing);
        }
        if element.is_bigint() != matches!(value, TypedArrayElementValue::BigInt(_)) {
            return Ok(TypedArrayStoreOutcome::ContentTypeMismatch);
        }
        let byte_index = typed_array_element_byte_index(byte_offset, index, element)?;
        let bytes = typed_array_write_element(element, value);
        let state = self
            .objects
            .get_mut(buffer)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "typed-array write buffer",
                index: buffer.index(),
                generation: buffer.generation(),
            })?
            .array_buffer_state_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "typed-array write buffer lost ArrayBuffer slots",
            })?;
        let data = state
            .data_mut()
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "typed-array write buffer detached after bounds check",
            })?;
        let end =
            byte_index
                .checked_add(bytes.len())
                .ok_or(crate::EngineFault::RuntimeInvariant {
                    message: "typed-array write byte range overflowed",
                })?;
        let target = data
            .get_mut(byte_index..end)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "typed-array write escaped validated backing-store bounds",
            })?;
        target.copy_from_slice(&bytes);
        Ok(TypedArrayStoreOutcome::Stored)
    }

    /// Builds `[[OwnPropertyKeys]]` for a typed array without materializing
    /// element slots. Virtual indices precede every ordinary string and symbol
    /// key; canonical numeric keys in the ordinary record are excluded because
    /// the integer-indexed exotic owns that namespace even when the element is
    /// currently absent.
    pub(crate) fn try_typed_array_own_key_snapshot(
        &self,
        object: ObjectId,
        phases: KeyPhases,
    ) -> Result<ForInSnapshot, ExecutionError> {
        let length = if phases.includes_indices() {
            match self.typed_array_view(object)? {
                TypedArrayView::InBounds { length, .. } => length,
                TypedArrayView::Detached | TypedArrayView::OutOfBounds => 0,
            }
        } else {
            0
        };
        let ordinary = self
            .object_record(HeapReference::Object(object))?
            .try_own_key_snapshot(None, phases)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::ForInEntries,
                additional: length,
            })?;
        let mut candidates = Vec::new();
        candidates
            .try_reserve(length.saturating_add(ordinary.len()))
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::ForInEntries,
                additional: length.saturating_add(ordinary.len()),
            })?;
        for index in 0..length {
            let index = u32::try_from(index).map_err(|_| ExecutionError::LimitExceeded {
                resource: RuntimeResource::ForInEntries,
                limit: u64::from(u32::MAX),
                observed: usize_to_u64(index),
            })?;
            let index = ArrayIndex::new(index).ok_or(crate::EngineFault::RuntimeInvariant {
                message: "typed-array ownKeys reached the ArrayIndex length sentinel",
            })?;
            candidates.push(crate::object::ForInCandidate::new(
                PropertyKey::from_index(index),
                true,
            ));
        }
        for candidate in ordinary.iter() {
            if typed_array_property_key(candidate.key())? != TypedArrayPropertyKey::Ordinary {
                continue;
            }
            candidates.push(candidate.clone());
        }
        Ok(ForInSnapshot::from_candidates(candidates, 0))
    }
}

fn typed_array_property_key(key: &PropertyKey) -> Result<TypedArrayPropertyKey, ExecutionError> {
    if let Some(index) = key.as_index() {
        return Ok(TypedArrayPropertyKey::Index(index.get() as usize));
    }
    let Some(atom) = key.as_atom() else {
        return Ok(TypedArrayPropertyKey::Ordinary);
    };
    if atom.kind() != AtomKind::String {
        return Ok(TypedArrayPropertyKey::Ordinary);
    }
    let string = atom
        .description()
        .ok_or(crate::EngineFault::RuntimeInvariant {
            message: "typed-array string key lost its description",
        })?;
    let Some(number) = canonical_numeric_index_string(string)? else {
        return Ok(TypedArrayPropertyKey::Ordinary);
    };
    let value = number.as_f64();
    if !value.is_finite()
        || value.to_bits() == (-0.0_f64).to_bits()
        || value < 0.0
        || value.trunc() != value
    {
        return Ok(TypedArrayPropertyKey::Invalid);
    }
    let Some(index) = number_to_index(number) else {
        return Ok(TypedArrayPropertyKey::Invalid);
    };
    match usize::try_from(index) {
        Ok(index) => Ok(TypedArrayPropertyKey::Index(index)),
        Err(_) => Ok(TypedArrayPropertyKey::Invalid),
    }
}

fn typed_array_element_byte_index(
    byte_offset: usize,
    index: usize,
    element: TypedArrayElementType,
) -> Result<usize, crate::EngineFault> {
    let width = element.byte_width();
    let bytes = index
        .checked_mul(width)
        .ok_or(crate::EngineFault::RuntimeInvariant {
            message: "typed-array element byte offset overflowed",
        })?;
    byte_offset
        .checked_add(bytes)
        .ok_or(crate::EngineFault::RuntimeInvariant {
            message: "typed-array element address overflowed",
        })
}

fn typed_array_read_element(
    data: &[u8],
    byte_index: usize,
    element: TypedArrayElementType,
) -> Result<StoredValue, crate::EngineFault> {
    let bytes = |width| typed_array_read_bytes(data, byte_index, width);
    Ok(match element {
        TypedArrayElementType::Int8 => StoredValue::Number(JsNumber::from_i32(i32::from(
            i8::from_ne_bytes([bytes(1)?[0]]),
        ))),
        TypedArrayElementType::Uint8 | TypedArrayElementType::Uint8Clamped => {
            StoredValue::Number(JsNumber::from_i32(i32::from(bytes(1)?[0])))
        }
        TypedArrayElementType::Int16 => {
            let bytes = typed_array_two_bytes(bytes(2)?);
            StoredValue::Number(JsNumber::from_i32(i32::from(i16::from_ne_bytes(bytes))))
        }
        TypedArrayElementType::Uint16 => {
            let bytes = typed_array_two_bytes(bytes(2)?);
            StoredValue::Number(JsNumber::from_i32(i32::from(u16::from_ne_bytes(bytes))))
        }
        TypedArrayElementType::Int32 => {
            let bytes = typed_array_four_bytes(bytes(4)?);
            StoredValue::Number(JsNumber::from_i32(i32::from_ne_bytes(bytes)))
        }
        TypedArrayElementType::Uint32 => {
            let bytes = typed_array_four_bytes(bytes(4)?);
            StoredValue::Number(JsNumber::from_u32(u32::from_ne_bytes(bytes)))
        }
        TypedArrayElementType::BigInt64 => {
            let bytes = typed_array_eight_bytes(bytes(8)?);
            StoredValue::BigInt(Arc::new(JsBigInt::from_i64(i64::from_ne_bytes(bytes))))
        }
        TypedArrayElementType::BigUint64 => {
            let bytes = typed_array_eight_bytes(bytes(8)?);
            StoredValue::BigInt(Arc::new(JsBigInt::from_u64(u64::from_ne_bytes(bytes))))
        }
        TypedArrayElementType::Float16 => {
            let bytes = typed_array_two_bytes(bytes(2)?);
            StoredValue::Number(JsNumber::from_f64(typed_array_f16_to_f64(
                u16::from_ne_bytes(bytes),
            )))
        }
        TypedArrayElementType::Float32 => {
            let bytes = typed_array_four_bytes(bytes(4)?);
            StoredValue::Number(JsNumber::from_f64(f64::from(f32::from_ne_bytes(bytes))))
        }
        TypedArrayElementType::Float64 => {
            let bytes = typed_array_eight_bytes(bytes(8)?);
            StoredValue::Number(JsNumber::from_f64(f64::from_ne_bytes(bytes)))
        }
    })
}

fn typed_array_write_element(
    element: TypedArrayElementType,
    value: TypedArrayElementValue<'_>,
) -> Vec<u8> {
    match (element, value) {
        (TypedArrayElementType::Int8, TypedArrayElementValue::Number(value)) => {
            number_to_int8(value).to_ne_bytes().to_vec()
        }
        (TypedArrayElementType::Uint8, TypedArrayElementValue::Number(value)) => {
            vec![number_to_uint8(value)]
        }
        (TypedArrayElementType::Uint8Clamped, TypedArrayElementValue::Number(value)) => {
            vec![number_to_uint8_clamp(value)]
        }
        (TypedArrayElementType::Int16, TypedArrayElementValue::Number(value)) => {
            number_to_int16(value).to_ne_bytes().to_vec()
        }
        (TypedArrayElementType::Uint16, TypedArrayElementValue::Number(value)) => {
            number_to_uint16(value).to_ne_bytes().to_vec()
        }
        (TypedArrayElementType::Int32, TypedArrayElementValue::Number(value)) => {
            number_to_int32(value).to_ne_bytes().to_vec()
        }
        (TypedArrayElementType::Uint32, TypedArrayElementValue::Number(value)) => {
            number_to_uint32(value).to_ne_bytes().to_vec()
        }
        (TypedArrayElementType::Float16, TypedArrayElementValue::Number(value)) => {
            typed_array_f64_to_f16(value.as_f64())
                .to_ne_bytes()
                .to_vec()
        }
        (TypedArrayElementType::Float32, TypedArrayElementValue::Number(value)) => {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "Float32Array write intentionally rounds an ECMAScript Number to IEEE binary32"
            )]
            let value = value.as_f64() as f32;
            value.to_ne_bytes().to_vec()
        }
        (TypedArrayElementType::Float64, TypedArrayElementValue::Number(value)) => {
            value.as_f64().to_ne_bytes().to_vec()
        }
        (TypedArrayElementType::BigInt64, TypedArrayElementValue::BigInt(value)) => {
            (value.low_u64_twos_complement() as i64)
                .to_ne_bytes()
                .to_vec()
        }
        (TypedArrayElementType::BigUint64, TypedArrayElementValue::BigInt(value)) => {
            value.low_u64_twos_complement().to_ne_bytes().to_vec()
        }
        (element, value) => unreachable!(
            "typed-array content-domain check accepts only matching element values: {element:?}, {}",
            match value {
                TypedArrayElementValue::Number(_) => "Number",
                TypedArrayElementValue::BigInt(_) => "BigInt",
            }
        ),
    }
}

fn typed_array_read_bytes(
    data: &[u8],
    byte_index: usize,
    width: usize,
) -> Result<&[u8], crate::EngineFault> {
    let end = byte_index
        .checked_add(width)
        .ok_or(crate::EngineFault::RuntimeInvariant {
            message: "typed-array read byte range overflowed",
        })?;
    data.get(byte_index..end)
        .ok_or(crate::EngineFault::RuntimeInvariant {
            message: "typed-array read escaped validated backing-store bounds",
        })
}

fn typed_array_two_bytes(bytes: &[u8]) -> [u8; 2] {
    let mut result = [0; 2];
    result.copy_from_slice(bytes);
    result
}

fn typed_array_four_bytes(bytes: &[u8]) -> [u8; 4] {
    let mut result = [0; 4];
    result.copy_from_slice(bytes);
    result
}

fn typed_array_eight_bytes(bytes: &[u8]) -> [u8; 8] {
    let mut result = [0; 8];
    result.copy_from_slice(bytes);
    result
}

fn typed_array_f16_to_f64(bits: u16) -> f64 {
    let sign = if bits >> 15 == 0 { 1.0 } else { -1.0 };
    let exponent = (bits >> 10) & 0x1f;
    let fraction = bits & 0x03ff;
    match exponent {
        0 if fraction == 0 => 0.0_f64.copysign(sign),
        0 => sign * f64::from(fraction) * 2.0_f64.powi(-24),
        0x1f if fraction == 0 => f64::INFINITY.copysign(sign),
        0x1f => f64::NAN.copysign(sign),
        exponent => {
            sign * (1.0 + f64::from(fraction) / 1024.0) * 2.0_f64.powi(i32::from(exponent) - 15)
        }
    }
}

fn typed_array_f64_to_f16(value: f64) -> u16 {
    let sign = u16::try_from((value.to_bits() >> 63) << 15)
        .expect("one-bit sign shifted into a u16 always fits");
    if value.is_nan() {
        return sign | 0x7e00;
    }
    if value.is_infinite() {
        return sign | 0x7c00;
    }
    let magnitude = value.abs();
    if magnitude == 0.0 {
        return sign;
    }
    let rounded = typed_array_f16round(magnitude);
    if rounded.is_infinite() {
        return sign | 0x7c00;
    }
    if rounded < 2.0_f64.powi(-14) {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a rounded binary16 subnormal significand lies in 1..1023"
        )]
        let fraction = (rounded * 2.0_f64.powi(24)).round() as u16;
        return sign | fraction;
    }
    let exponent = rounded.log2().floor() as i32;
    let significand = rounded / 2.0_f64.powi(exponent);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the rounded binary16 significand fits the 10-bit fraction field"
    )]
    let fraction = ((significand - 1.0) * 1024.0).round() as u16;
    let encoded_exponent =
        u16::try_from(exponent + 15).expect("normal binary16 exponent lies in 1..=30");
    sign | (encoded_exponent << 10) | (fraction & 0x03ff)
}

fn typed_array_f16round(value: f64) -> f64 {
    if value >= 65520.0 {
        return f64::INFINITY;
    }
    if value < 2.0_f64.powi(-24) / 2.0 {
        return 0.0;
    }
    let exponent = value.log2().floor() as i32;
    let step = if exponent < -14 {
        2.0_f64.powi(-24)
    } else {
        2.0_f64.powi(exponent - 10)
    };
    let scaled = value / step;
    let floor = scaled.floor();
    let fraction = scaled - floor;
    let rounded = if fraction > 0.5 || (fraction == 0.5 && (floor as u64 & 1) == 1) {
        floor + 1.0
    } else {
        floor
    };
    rounded * step
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RuntimeLimits, object::TypedArrayLength};

    fn typed_array(
        runtime: &mut Runtime,
        element: TypedArrayElementType,
        byte_length: usize,
        offset: usize,
        length: TypedArrayLength,
    ) -> ObjectId {
        let realm = runtime.create_realm().expect("realm");
        let realm_id = realm.0.id;
        let prototype = HeapReference::Object(
            runtime
                .realm_array_buffer_prototype(realm_id)
                .expect("ArrayBuffer prototype"),
        );
        let buffer = runtime
            .allocate_array_buffer(prototype, byte_length, Some(byte_length.saturating_add(8)))
            .expect("resizable buffer");
        runtime
            .allocate_typed_array(
                prototype,
                TypedArrayState::new(buffer, offset, length, element),
            )
            .expect("typed array")
    }

    #[test]
    fn fixed_and_length_tracking_views_take_fresh_resizable_buffer_witnesses() {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let realm_id = realm.0.id;
        let prototype = HeapReference::Object(
            runtime
                .realm_array_buffer_prototype(realm_id)
                .expect("ArrayBuffer prototype"),
        );
        let buffer = runtime
            .allocate_array_buffer(prototype, 8, Some(12))
            .expect("resizable buffer");
        let fixed = runtime
            .allocate_typed_array(
                prototype,
                TypedArrayState::new(
                    buffer,
                    2,
                    TypedArrayLength::Fixed(3),
                    TypedArrayElementType::Uint16,
                ),
            )
            .expect("fixed view");
        let tracking = runtime
            .allocate_typed_array(
                prototype,
                TypedArrayState::new(
                    buffer,
                    2,
                    TypedArrayLength::Auto,
                    TypedArrayElementType::Uint16,
                ),
            )
            .expect("tracking view");

        assert!(matches!(
            runtime.typed_array_view(fixed),
            Ok(TypedArrayView::InBounds { length: 3, .. })
        ));
        assert!(matches!(
            runtime.typed_array_view(tracking),
            Ok(TypedArrayView::InBounds { length: 3, .. })
        ));

        runtime
            .resize_array_buffer(buffer, 5)
            .expect("resizable shrink");
        assert!(matches!(
            runtime.typed_array_view(fixed),
            Ok(TypedArrayView::OutOfBounds)
        ));
        assert!(matches!(
            runtime.typed_array_view(tracking),
            Ok(TypedArrayView::InBounds { length: 1, .. })
        ));
    }

    #[test]
    fn typed_array_element_storage_covers_number_bigint_and_float16_domains() {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let number = typed_array(
            &mut runtime,
            TypedArrayElementType::Uint8Clamped,
            2,
            0,
            TypedArrayLength::Fixed(2),
        );
        assert_eq!(
            runtime
                .typed_array_store_index(
                    number,
                    0,
                    TypedArrayElementValue::Number(JsNumber::from_f64(2.5)),
                )
                .expect("store"),
            TypedArrayStoreOutcome::Stored
        );
        assert!(matches!(
            runtime.typed_array_read_index(number, 0),
            Ok(Some(StoredValue::Number(value))) if value.as_f64() == 2.0
        ));
        assert_eq!(
            runtime
                .typed_array_store_index(
                    number,
                    0,
                    TypedArrayElementValue::BigInt(&JsBigInt::from_i64(1)),
                )
                .expect("store"),
            TypedArrayStoreOutcome::ContentTypeMismatch
        );

        let bigint = typed_array(
            &mut runtime,
            TypedArrayElementType::BigInt64,
            8,
            0,
            TypedArrayLength::Fixed(1),
        );
        let source = JsBigInt::from_i64(-1);
        assert_eq!(
            runtime
                .typed_array_store_index(bigint, 0, TypedArrayElementValue::BigInt(&source))
                .expect("store"),
            TypedArrayStoreOutcome::Stored
        );
        assert!(matches!(
            runtime.typed_array_read_index(bigint, 0),
            Ok(Some(StoredValue::BigInt(value))) if value.to_i64() == Some(-1)
        ));

        let half = typed_array(
            &mut runtime,
            TypedArrayElementType::Float16,
            2,
            0,
            TypedArrayLength::Fixed(1),
        );
        assert_eq!(
            runtime
                .typed_array_store_index(
                    half,
                    0,
                    TypedArrayElementValue::Number(JsNumber::from_f64(1.5)),
                )
                .expect("store"),
            TypedArrayStoreOutcome::Stored
        );
        assert!(matches!(
            runtime.typed_array_read_index(half, 0),
            Ok(Some(StoredValue::Number(value))) if value.as_f64() == 1.5
        ));
    }

    #[test]
    fn typed_array_property_keys_distinguish_canonical_numeric_exotics() {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let array = typed_array(
            &mut runtime,
            TypedArrayElementType::Uint8,
            2,
            0,
            TypedArrayLength::Fixed(2),
        );
        for (text, expected) in [
            ("0", TypedArrayPropertyKey::Index(0)),
            ("1.0", TypedArrayPropertyKey::Ordinary),
            ("00", TypedArrayPropertyKey::Ordinary),
            ("-0", TypedArrayPropertyKey::Invalid),
            ("0.5", TypedArrayPropertyKey::Invalid),
            ("NaN", TypedArrayPropertyKey::Invalid),
            ("Infinity", TypedArrayPropertyKey::Invalid),
        ] {
            let key = runtime
                .property_key_from_string(&crate::JsString::from_utf8(text).expect("key"))
                .expect("property key");
            assert_eq!(
                runtime
                    .typed_array_property_key(array, &key)
                    .expect("classification"),
                Some(expected),
                "{text}"
            );
        }
    }

    #[test]
    fn typed_array_element_metadata_covers_all_ecmascript_element_domains() {
        assert_eq!(TypedArrayElementType::ALL.len(), 12);
        for element in TypedArrayElementType::ALL {
            assert!(matches!(element.byte_width(), 1 | 2 | 4 | 8));
            assert_eq!(
                element.is_bigint(),
                matches!(
                    element,
                    TypedArrayElementType::BigInt64 | TypedArrayElementType::BigUint64
                )
            );
        }
    }
}
