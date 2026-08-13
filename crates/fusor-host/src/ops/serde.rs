//! JsValue ↔ serde bridge (§5.2).
//!
//! The deserializer consumes a same-runtime [`JsValue`] through the typed
//! serde data model: integers accept only safe-integer whole Number values
//! (any other shape raises a parameter-indexed `TypeError`), `Option` maps
//! `null`/`undefined` to `None`, sequences and tuples read dense Array
//! elements, and structs/maps read own string-keyed properties. The
//! serializer produces a fresh [`JsValue`] (`unit` → `undefined`,
//! `Option::None` → `null`, sequences → Arrays, maps/structs → ordinary
//! objects) while holding a `&mut Context`.

use std::fmt;

use fusor_runtime::{Context, JsNumber, JsString, JsValue};
use serde::de::{self, DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{
    self, SerializeMap, SerializeSeq, SerializeStruct, SerializeTuple, SerializeTupleStruct,
};

/// A deserialization failure with the zero-based parameter index that caused
/// it (§5.2: non-conforming values raise a parameter-indexed `TypeError`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeserializationError {
    /// Zero-based host parameter index.
    pub parameter: usize,
    /// The failure description.
    pub message: String,
}

impl fmt::Display for DeserializationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "parameter {}: {}", self.parameter, self.message)
    }
}

impl std::error::Error for DeserializationError {}

impl de::Error for DeserializationError {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self {
            parameter: 0,
            message: message.to_string(),
        }
    }
}

/// A serialization failure (a JavaScript value the serializer cannot
/// represent, or a runtime failure while building the result).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SerializationError {
    message: String,
}

impl fmt::Display for SerializationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SerializationError {}

impl ser::Error for SerializationError {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}

/// serde deserializer over one same-runtime [`JsValue`] (§5.2).
pub struct JsValueDeserializer<'a, 'c, 'ctx> {
    context: &'c mut Context<'ctx>,
    value: &'a JsValue,
    parameter: usize,
}

impl<'a, 'c, 'ctx> JsValueDeserializer<'a, 'c, 'ctx> {
    /// Wraps one value for typed deserialization.
    #[must_use]
    pub const fn new(
        context: &'c mut Context<'ctx>,
        value: &'a JsValue,
        parameter: usize,
    ) -> Self {
        Self {
            context,
            value,
            parameter,
        }
    }

    fn invalid(&self, expected: &str) -> DeserializationError {
        let actual = self
            .value
            .kind()
            .map(|kind| format!("{kind:?}"))
            .unwrap_or_else(|_| "<stale>".to_owned());
        DeserializationError {
            parameter: self.parameter,
            message: format!("expected {expected}, received {actual}"),
        }
    }

    fn invalid_number(&self, message: &str) -> DeserializationError {
        DeserializationError {
            parameter: self.parameter,
            message: message.to_owned(),
        }
    }
}

macro_rules! deserialize_integer {
    ($method:ident, $visit:ident, $type:ty) => {
        fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            let Some(number) = self
                .value
                .as_number()
                .map_err(|error| de::Error::custom(error))?
            else {
                return Err(self.invalid("a Number"));
            };
            let value = number.as_f64();
            // Safe-integer whole values only (§5.2): no silent truncation.
            if value.fract() != 0.0
                || !(value >= -(2_f64.powi(53)) && value < 2_f64.powi(53))
            {
                return Err(self.invalid_number("Number is not a safe integer"));
            }
            let converted = value as $type;
            if converted as f64 != value {
                return Err(self.invalid_number("Number is outside the target domain"));
            }
            visitor.$visit(converted)
        }
    };
}

impl<'de, 'a, 'c, 'ctx> de::Deserializer<'de> for JsValueDeserializer<'a, 'c, 'ctx> {
    type Error = DeserializationError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let kind = self
            .value
            .kind()
            .map_err(|error| de::Error::custom(error))?;
        match kind {
            fusor_runtime::ValueKind::Undefined | fusor_runtime::ValueKind::Null => {
                visitor.visit_unit()
            }
            fusor_runtime::ValueKind::Boolean => self.deserialize_bool(visitor),
            fusor_runtime::ValueKind::Number => visitor.visit_f64(
                self.value
                    .as_number()
                    .map_err(|error| de::Error::custom(error))?
                    .expect("Number kind")
                    .as_f64(),
            ),
            fusor_runtime::ValueKind::String => self.deserialize_string(visitor),
            fusor_runtime::ValueKind::Object | fusor_runtime::ValueKind::Function => {
                // Arrays deserialize as sequences; ordinary objects as maps.
                if is_array(self.context, self.value)? {
                    self.deserialize_seq(visitor)
                } else {
                    self.deserialize_map(visitor)
                }
            }
            fusor_runtime::ValueKind::BigInt | fusor_runtime::ValueKind::Symbol => {
                Err(self.invalid("a serializable value"))
            }
        }
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let Some(value) = self
            .value
            .as_boolean()
            .map_err(|error| de::Error::custom(error))?
        else {
            return Err(self.invalid("a Boolean"));
        };
        visitor.visit_bool(value)
    }

    deserialize_integer!(deserialize_i8, visit_i8, i8);
    deserialize_integer!(deserialize_i16, visit_i16, i16);
    deserialize_integer!(deserialize_i32, visit_i32, i32);
    deserialize_integer!(deserialize_i64, visit_i64, i64);
    deserialize_integer!(deserialize_u8, visit_u8, u8);
    deserialize_integer!(deserialize_u16, visit_u16, u16);
    deserialize_integer!(deserialize_u32, visit_u32, u32);
    deserialize_integer!(deserialize_u64, visit_u64, u64);

    fn deserialize_f32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let Some(number) = self
            .value
            .as_number()
            .map_err(|error| de::Error::custom(error))?
        else {
            return Err(self.invalid("a Number"));
        };
        visitor.visit_f32(number.as_f64() as f32)
    }

    fn deserialize_f64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let Some(number) = self
            .value
            .as_number()
            .map_err(|error| de::Error::custom(error))?
        else {
            return Err(self.invalid("a Number"));
        };
        visitor.visit_f64(number.as_f64())
    }

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_string(visitor)
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_string(visitor)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let Some(value) = self
            .value
            .as_string()
            .map_err(|error| de::Error::custom(error))?
        else {
            return Err(self.invalid("a String"));
        };
        visitor.visit_string(value.to_utf8_lossy().unwrap_or_default())
    }

    fn deserialize_bytes<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        Err(self.invalid("bytes (not supported in v1)"))
    }

    fn deserialize_byte_buf<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        Err(self.invalid("bytes (not supported in v1)"))
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        if matches!(
            self.value
                .kind()
                .map_err(|error| de::Error::custom(error))?,
            fusor_runtime::ValueKind::Undefined | fusor_runtime::ValueKind::Null
        ) {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let object = self
            .value
            .clone()
            .into_object()
            .map_err(|error| de::Error::custom(error))?;
        let length = array_length(self.context, self.value)?;
        visitor.visit_seq(ArrayAccess {
            context: self.context,
            object,
            index: 0,
            length,
            parameter: self.parameter,
        })
    }

    fn deserialize_tuple<V>(self, _length: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _length: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let object = self
            .value
            .clone()
            .into_object()
            .map_err(|error| de::Error::custom(error))?;
        let keys = object_string_keys(self.context, self.value)?;
        visitor.visit_map(ObjectAccess {
            context: self.context,
            object,
            keys,
            next: 0,
            pending: None,
            parameter: self.parameter,
        })
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_map(visitor)
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        // A unit-variant enum round-trips as the variant name String.
        let Some(variant) = self
            .value
            .as_string()
            .map_err(|error| de::Error::custom(error))?
        else {
            return Err(self.invalid("a String enum representation"));
        };
        let variant = variant.to_utf8_lossy().unwrap_or_default();
        visitor.visit_enum(variant.into_deserializer())
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_string(visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }
}

/// Reports whether a same-runtime value is an Array exotic (ECMA-262
/// `IsArray`).
fn is_array(context: &Context<'_>, value: &JsValue) -> Result<bool, DeserializationError> {
    context
        .is_array(value)
        .map_err(|error| de::Error::custom(error))
}

/// Reads the dense length of an Array exotic (defaulting to 0 for non-arrays).
fn array_length(context: &mut Context<'_>, value: &JsValue) -> Result<usize, DeserializationError> {
    let object = value
        .clone()
        .into_object()
        .map_err(|error| de::Error::custom(error))?;
    let key = context
        .property_key("length")
        .map_err(|error| de::Error::custom(error))?;
    let length = object
        .get(context, key)
        .map_err(|error| de::Error::custom(error))?;
    Ok(length
        .as_number()
        .ok()
        .flatten()
        .map(|number| number.as_f64() as usize)
        .unwrap_or(0))
}

/// Collects the own string-keyed property keys of an ordinary object.
fn object_string_keys(
    context: &mut Context<'_>,
    value: &JsValue,
) -> Result<Vec<fusor_runtime::PropertyKey>, DeserializationError> {
    let object = value
        .clone()
        .into_object()
        .map_err(|error| de::Error::custom(error))?;
    let keys = object
        .own_property_keys(context)
        .map_err(|error| de::Error::custom(error))?;
    Ok(keys
        .into_iter()
        .filter(|key| {
            key.as_atom()
                .is_some_and(|atom| atom.kind() == fusor_runtime::AtomKind::String)
        })
        .collect())
}

struct ArrayAccess<'c, 'ctx> {
    context: &'c mut Context<'ctx>,
    object: fusor_runtime::Object,
    index: usize,
    length: usize,
    parameter: usize,
}

impl<'de, 'c, 'ctx> SeqAccess<'de> for ArrayAccess<'c, 'ctx> {
    type Error = DeserializationError;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        if self.index >= self.length {
            return Ok(None);
        }
        let key = self
            .context
            .property_key(&self.index.to_string())
            .map_err(|error| de::Error::custom(error))?;
        let value = self
            .object
            .get(self.context, key)
            .map_err(|error| de::Error::custom(error))?;
        self.index += 1;
        seed.deserialize(JsValueDeserializer::new(
            self.context,
            &value,
            self.parameter,
        ))
        .map(Some)
    }
}

struct ObjectAccess<'c, 'ctx> {
    context: &'c mut Context<'ctx>,
    object: fusor_runtime::Object,
    keys: Vec<fusor_runtime::PropertyKey>,
    next: usize,
    pending: Option<JsValue>,
    parameter: usize,
}

impl<'de, 'c, 'ctx> MapAccess<'de> for ObjectAccess<'c, 'ctx> {
    type Error = DeserializationError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        if self.next >= self.keys.len() {
            return Ok(None);
        }
        let key = &self.keys[self.next];
        self.next += 1;
        let name = key
            .as_atom()
            .and_then(fusor_runtime::Atom::description)
            .map(|name| name.to_utf8_lossy().unwrap_or_default())
            .unwrap_or_default();
        let value = self
            .object
            .get(self.context, key.clone())
            .map_err(|error| de::Error::custom(error))?;
        self.pending = Some(value);
        seed.deserialize(name.into_deserializer()).map(Some)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let value = self.pending.take().ok_or_else(|| {
            DeserializationError {
                parameter: self.parameter,
                message: "map value requested without a key".to_owned(),
            }
        })?;
        seed.deserialize(JsValueDeserializer::new(
            self.context,
            &value,
            self.parameter,
        ))
    }
}

/// serde serializer producing fresh same-runtime [`JsValue`]s (§5.2).
pub struct JsValueSerializer<'a, 'ctx> {
    context: &'a mut Context<'ctx>,
}

impl<'a, 'ctx> JsValueSerializer<'a, 'ctx> {
    /// Creates a serializer writing into this context.
    #[must_use]
    pub const fn new(context: &'a mut Context<'ctx>) -> Self {
        Self { context }
    }
}

macro_rules! serialize_number {
    ($method:ident, $visit:ident, $cast:ty, $from:ident) => {
        fn $method(self, value: $visit) -> Result<Self::Ok, Self::Error> {
            Ok(self.context.number(JsNumber::$from(value as $cast)))
        }
    };
}

impl<'a, 'ctx> ser::Serializer for JsValueSerializer<'a, 'ctx> {
    type Ok = JsValue;
    type Error = SerializationError;

    type SerializeSeq = ValueSequence<'a, 'ctx>;
    type SerializeTuple = ValueSequence<'a, 'ctx>;
    type SerializeTupleStruct = ValueSequence<'a, 'ctx>;
    type SerializeTupleVariant = ValueSequence<'a, 'ctx>;
    type SerializeMap = ValueMap<'a, 'ctx>;
    type SerializeStruct = ValueMap<'a, 'ctx>;
    type SerializeStructVariant = ValueMap<'a, 'ctx>;

    fn serialize_bool(self, value: bool) -> Result<Self::Ok, Self::Error> {
        Ok(self.context.boolean(value))
    }

    serialize_number!(serialize_i8, i8, i32, from_i32);
    serialize_number!(serialize_i16, i16, i32, from_i32);
    serialize_number!(serialize_i32, i32, i32, from_i32);
    serialize_number!(serialize_i64, i64, i64, from_i64);
    serialize_number!(serialize_u8, u8, u32, from_u32);
    serialize_number!(serialize_u16, u16, u32, from_u32);
    serialize_number!(serialize_u32, u32, u32, from_u32);

    fn serialize_u64(self, value: u64) -> Result<Self::Ok, Self::Error> {
        if value > 9_007_199_254_740_991 {
            return Err(ser::Error::custom(
                "u64 outside the safe-integer domain",
            ));
        }
        Ok(self.context.number(JsNumber::from_f64(value as f64)))
    }

    fn serialize_f32(self, value: f32) -> Result<Self::Ok, Self::Error> {
        Ok(self.context.number(JsNumber::from_f64(f64::from(value))))
    }

    fn serialize_f64(self, value: f64) -> Result<Self::Ok, Self::Error> {
        Ok(self.context.number(JsNumber::from_f64(value)))
    }

    fn serialize_char(self, value: char) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(&value.to_string())
    }

    fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
        let string =
            JsString::from_utf8(value).map_err(|error| ser::Error::custom(error))?;
        Ok(self.context.string(string))
    }

    fn serialize_bytes(self, _value: &[u8]) -> Result<Self::Ok, Self::Error> {
        Err(ser::Error::custom(
            "bytes are not supported in v1",
        ))
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.context.null())
    }

    fn serialize_some<T>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.context.undefined())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        self.serialize_unit()
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(variant)
    }

    fn serialize_newtype_struct<T>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        Err(ser::Error::custom(
            "newtype enum variants are not supported",
        ))
    }

    fn serialize_seq(self, _length: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Ok(ValueSequence {
            context: self.context,
            elements: Vec::new(),
        })
    }

    fn serialize_tuple(self, _length: usize) -> Result<Self::SerializeTuple, Self::Error> {
        self.serialize_seq(None)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        length: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        self.serialize_seq(Some(length))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Err(ser::Error::custom(
            "tuple enum variants are not supported",
        ))
    }

    fn serialize_map(self, _length: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Ok(ValueMap {
            context: self.context,
            entries: Vec::new(),
        })
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        self.serialize_map(None)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Err(ser::Error::custom(
            "struct enum variants are not supported",
        ))
    }
}

/// Sequence-building state: collects element values and materializes an
/// Array at the end.
pub struct ValueSequence<'a, 'ctx> {
    context: &'a mut Context<'ctx>,
    elements: Vec<JsValue>,
}

impl<'a, 'ctx> SerializeSeq for ValueSequence<'a, 'ctx> {
    type Ok = JsValue;
    type Error = SerializationError;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        let element = value.serialize(JsValueSerializer::new(self.context))?;
        self.elements.push(element);
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.context
            .new_array(self.elements)
            .map_err(|error| ser::Error::custom(error))
    }
}

impl<'a, 'ctx> SerializeTuple for ValueSequence<'a, 'ctx> {
    type Ok = JsValue;
    type Error = SerializationError;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        SerializeSeq::end(self)
    }
}

impl<'a, 'ctx> ser::SerializeTupleVariant for ValueSequence<'a, 'ctx> {
    type Ok = JsValue;
    type Error = SerializationError;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        SerializeSeq::end(self)
    }
}

impl<'a, 'ctx> SerializeTupleStruct for ValueSequence<'a, 'ctx> {
    type Ok = JsValue;
    type Error = SerializationError;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        SerializeSeq::end(self)
    }
}

/// Map/struct-building state: collects (key, value) pairs and materializes
/// an ordinary object at the end.
pub struct ValueMap<'a, 'ctx> {
    context: &'a mut Context<'ctx>,
    entries: Vec<(JsString, JsValue)>,
}

impl<'a, 'ctx> SerializeMap for ValueMap<'a, 'ctx> {
    type Ok = JsValue;
    type Error = SerializationError;

    fn serialize_key<T>(&mut self, key: &T) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        let serialized = key.serialize(KeySerializer)?;
        self.entries.push((serialized, self.context.undefined()));
        Ok(())
    }

    fn serialize_value<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        let serialized = value.serialize(JsValueSerializer::new(self.context))?;
        if let Some(entry) = self.entries.last_mut() {
            entry.1 = serialized;
        }
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        let object = self
            .context
            .new_object()
            .map_err(|error| ser::Error::custom(error))?
            .into_object()
            .map_err(|error| ser::Error::custom(error))?;
        for (name, value) in self.entries {
            let key = self
                .context
                .property_key(&name.to_utf8_lossy().unwrap_or_default())
                .map_err(|error| ser::Error::custom(error))?;
            object
                .set(self.context, key, value)
                .map_err(|error| ser::Error::custom(error))?;
        }
        Ok(object.as_value())
    }
}

impl<'a, 'ctx> ser::SerializeStructVariant for ValueMap<'a, 'ctx> {
    type Ok = JsValue;
    type Error = SerializationError;

    fn serialize_field<T>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        SerializeMap::serialize_key(self, &key)?;
        SerializeMap::serialize_value(self, value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        SerializeMap::end(self)
    }
}

impl<'a, 'ctx> SerializeStruct for ValueMap<'a, 'ctx> {
    type Ok = JsValue;
    type Error = SerializationError;

    fn serialize_field<T>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        SerializeMap::serialize_key(self, &key)?;
        SerializeMap::serialize_value(self, value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        SerializeMap::end(self)
    }
}

/// Serializes map/struct keys: string keys only (§5.2).
struct KeySerializer;

impl ser::Serializer for KeySerializer {
    type Ok = JsString;
    type Error = SerializationError;

    type SerializeSeq = KeySequence;
    type SerializeTuple = KeySequence;
    type SerializeTupleStruct = KeySequence;
    type SerializeTupleVariant = KeySequence;
    type SerializeMap = KeySequence;
    type SerializeStruct = KeySequence;
    type SerializeStructVariant = KeySequence;

    fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
        JsString::from_utf8(value).map_err(|error| ser::Error::custom(error))
    }

    fn serialize_char(self, value: char) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(&value.to_string())
    }

    fn serialize_i8(self, value: i8) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(&value.to_string())
    }

    fn serialize_i16(self, value: i16) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(&value.to_string())
    }

    fn serialize_i32(self, value: i32) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(&value.to_string())
    }

    fn serialize_i64(self, value: i64) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(&value.to_string())
    }

    fn serialize_u8(self, value: u8) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(&value.to_string())
    }

    fn serialize_u16(self, value: u16) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(&value.to_string())
    }

    fn serialize_u32(self, value: u32) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(&value.to_string())
    }

    fn serialize_u64(self, value: u64) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(&value.to_string())
    }

    fn serialize_f32(self, value: f32) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(&value.to_string())
    }

    fn serialize_f64(self, value: f64) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(&value.to_string())
    }

    fn serialize_bool(self, value: bool) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(if value { "true" } else { "false" })
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        Err(ser::Error::custom(
            "map keys must be strings in v1",
        ))
    }

    fn serialize_unit_struct(self, name: &'static str) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(name)
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(variant)
    }

    fn serialize_newtype_struct<T>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        value.serialize(self)
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        Err(ser::Error::custom(
            "map keys must be strings in v1",
        ))
    }

    fn serialize_some<T>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        Err(ser::Error::custom(
            "enum keys are not supported",
        ))
    }

    fn serialize_seq(self, _length: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Err(ser::Error::custom(
            "map keys must be strings in v1",
        ))
    }

    fn serialize_tuple(self, _length: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Err(ser::Error::custom(
            "map keys must be strings in v1",
        ))
    }

    fn serialize_tuple_struct(
        self,
        name: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Ok(KeySequence(self.serialize_str(name)?))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Err(ser::Error::custom(
            "enum keys are not supported",
        ))
    }

    fn serialize_map(self, _length: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Err(ser::Error::custom(
            "map keys must be strings in v1",
        ))
    }

    fn serialize_struct(
        self,
        name: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Ok(KeySequence(self.serialize_str(name)?))
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Err(ser::Error::custom(
            "enum keys are not supported",
        ))
    }

    fn serialize_bytes(self, _value: &[u8]) -> Result<Self::Ok, Self::Error> {
        Err(ser::Error::custom(
            "bytes are not supported in v1",
        ))
    }
}

/// Placeholder sequence for key serialization (always rejected).
pub struct KeySequence(JsString);

impl SerializeSeq for KeySequence {
    type Ok = JsString;
    type Error = SerializationError;

    fn serialize_element<T>(&mut self, _value: &T) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        Err(ser::Error::custom(
            "map keys must be strings in v1",
        ))
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.0)
    }
}

impl SerializeTuple for KeySequence {
    type Ok = JsString;
    type Error = SerializationError;

    fn serialize_element<T>(&mut self, _value: &T) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        Err(ser::Error::custom(
            "map keys must be strings in v1",
        ))
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.0)
    }
}

impl SerializeMap for KeySequence {
    type Ok = JsString;
    type Error = SerializationError;

    fn serialize_key<T>(&mut self, _key: &T) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        Err(ser::Error::custom(
            "map keys must be strings in v1",
        ))
    }

    fn serialize_value<T>(&mut self, _value: &T) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        Err(ser::Error::custom(
            "map keys must be strings in v1",
        ))
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.0)
    }
}

impl SerializeStruct for KeySequence {
    type Ok = JsString;
    type Error = SerializationError;

    fn serialize_field<T>(
        &mut self,
        _key: &'static str,
        _value: &T,
    ) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        Err(ser::Error::custom(
            "map keys must be strings in v1",
        ))
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.0)
    }
}

impl ser::SerializeTupleVariant for KeySequence {
    type Ok = JsString;
    type Error = SerializationError;

    fn serialize_field<T>(&mut self, _value: &T) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        Err(ser::Error::custom(
            "map keys must be strings in v1",
        ))
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.0)
    }
}

impl ser::SerializeStructVariant for KeySequence {
    type Ok = JsString;
    type Error = SerializationError;

    fn serialize_field<T>(
        &mut self,
        _key: &'static str,
        _value: &T,
    ) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        Err(ser::Error::custom(
            "map keys must be strings in v1",
        ))
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.0)
    }
}

impl SerializeTupleStruct for KeySequence {
    type Ok = JsString;
    type Error = SerializationError;

    fn serialize_field<T>(&mut self, _value: &T) -> Result<(), Self::Error>
    where
        T: serde::Serialize + ?Sized,
    {
        Err(ser::Error::custom(
            "map keys must be strings in v1",
        ))
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.0)
    }
}
