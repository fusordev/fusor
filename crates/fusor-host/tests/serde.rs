//! JsValue ↔ serde round-trip matrix (§5.2).

use std::collections::HashMap;

use fusor_host::ops::{DeserializationError, JsValueDeserializer, JsValueSerializer};
use fusor_runtime::{Context, JsNumber, JsString, Runtime, RuntimeLimits, ValueKind};
use serde::{Deserialize, Serialize};

fn with_context<T>(operation: impl FnOnce(&mut Context<'_>) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    operation(&mut context)
}

fn number(context: &mut Context<'_>, value: f64) -> fusor_runtime::JsValue {
    context.number(JsNumber::from_f64(value))
}

fn string(context: &mut Context<'_>, value: &str) -> fusor_runtime::JsValue {
    context.string(JsString::from_utf8(value).expect("text"))
}

fn deserialize<'de, T: Deserialize<'de>>(
    context: &mut Context<'_>,
    value: &fusor_runtime::JsValue,
    parameter: usize,
) -> Result<T, DeserializationError> {
    T::deserialize(JsValueDeserializer::new(context, value, parameter))
}

fn serialize<T: Serialize>(
    context: &mut Context<'_>,
    value: &T,
) -> Result<fusor_runtime::JsValue, fusor_host::ops::SerializationError> {
    value.serialize(JsValueSerializer::new(context))
}

#[test]
fn primitives_round_trip() {
    with_context(|context| {
        let boolean = context.boolean(true);
        assert_eq!(deserialize::<bool>(context, &boolean, 0).expect("bool"), true);

        let forty_two = number(context, 42.0);
        assert_eq!(deserialize::<i32>(context, &forty_two, 0).expect("i32"), 42);

        let max_safe = number(context, 9_007_199_254_740_991.0);
        assert_eq!(
            deserialize::<u64>(context, &max_safe, 0).expect("u64"),
            9_007_199_254_740_991
        );

        let one_and_half = number(context, 1.5);
        assert_eq!(deserialize::<f64>(context, &one_and_half, 0).expect("f64"), 1.5);

        let hello = string(context, "hello");
        assert_eq!(
            deserialize::<String>(context, &hello, 0).expect("string"),
            "hello"
        );

        let null = context.null();
        assert_eq!(
            deserialize::<Option<String>>(context, &null, 0).expect("null option"),
            None
        );

        let undefined = context.undefined();
        assert_eq!(
            deserialize::<Option<String>>(context, &undefined, 0)
                .expect("undefined option"),
            None
        );

        let kept = string(context, "kept");
        assert_eq!(
            deserialize::<Option<String>>(context, &kept, 0).expect("some"),
            Some("kept".to_owned())
        );
    });
}

#[test]
fn integers_reject_non_safe_shapes_with_the_parameter_index() {
    with_context(|context| {
        let fractional = number(context, 1.5);
        let error = deserialize::<i32>(context, &fractional, 3).expect_err("fractional");
        assert_eq!(error.parameter, 3);
        assert!(error.message.contains("safe integer"), "{error}");

        let oversized = number(context, 2_f64.powi(53));
        assert!(deserialize::<u64>(context, &oversized, 0).is_err());

        let wrong_kind = string(context, "not a number");
        let error = deserialize::<i32>(context, &wrong_kind, 1).expect_err("string as int");
        assert_eq!(error.parameter, 1);
        assert!(error.message.contains("Number"), "{error}");

        // Out-of-domain narrowing is rejected, not truncated.
        let too_large = number(context, 300.0);
        assert!(deserialize::<u8>(context, &too_large, 0).is_err());
    });
}

#[test]
fn sequences_and_tuples_read_dense_arrays() {
    with_context(|context| {
        let one = number(context, 1.0);
        let two = number(context, 2.0);
        let three = number(context, 3.0);
        let array = context.new_array(vec![one, two, three]).expect("array");
        assert_eq!(
            deserialize::<Vec<i32>>(context, &array, 0).expect("vec"),
            vec![1, 2, 3]
        );
        let x = string(context, "x");
        let seven = number(context, 7.0);
        let tuple = context.new_array(vec![x, seven]).expect("array");
        let (name, value) = deserialize::<(String, i32)>(context, &tuple, 0).expect("tuple");
        assert_eq!(name, "x");
        assert_eq!(value, 7);
    });
}

#[test]
fn maps_and_structs_read_own_string_keys() {
    #[derive(Debug, PartialEq, Deserialize, Serialize)]
    struct Payload {
        name: String,
        count: u32,
    }

    with_context(|context| {
        let object = context.new_object().expect("object");
        let object = object.into_object().expect("object handle");
        let name = context.property_key("name").expect("key");
        let count = context.property_key("count").expect("key");
        let fusor = string(context, "fusor");
        object.set(context, name, fusor).expect("set");
        let nine = number(context, 9.0);
        object.set(context, count, nine).expect("set");

        let payload = deserialize::<Payload>(context, &object.as_value(), 0).expect("struct");
        assert_eq!(
            payload,
            Payload {
                name: "fusor".to_owned(),
                count: 9,
            }
        );

        // A homogeneous numeric object reads as a map.
        let numeric = context.new_object().expect("object");
        let numeric = numeric.into_object().expect("object handle");
        let count = context.property_key("count").expect("key");
        let nine = number(context, 9.0);
        numeric.set(context, count, nine).expect("set");
        let map = deserialize::<HashMap<String, i32>>(context, &numeric.as_value(), 0)
            .expect("map");
        assert_eq!(map.get("count"), Some(&9));

        // Round trip: serialize the struct back and re-read it.
        let serialized = serialize(context, &payload).expect("serialize");
        assert_eq!(serialized.kind().expect("live"), ValueKind::Object);
        assert_eq!(
            deserialize::<Payload>(context, &serialized, 0).expect("re-read"),
            payload
        );
    });
}

#[test]
fn the_serializer_produces_spec_shapes() {
    with_context(|context| {
        assert_eq!(
            serialize(context, &()).expect("unit").kind().expect("live"),
            ValueKind::Undefined
        );
        assert_eq!(
            serialize(context, &None::<i32>).expect("none").kind().expect("live"),
            ValueKind::Null
        );
        assert_eq!(
            serialize(context, &true).expect("bool").kind().expect("live"),
            ValueKind::Boolean
        );
        assert_eq!(
            serialize(context, &"text")
                .expect("str")
                .kind()
                .expect("live"),
            ValueKind::String
        );
        let list = serialize(context, &vec![1_i32, 2, 3]).expect("seq");
        assert_eq!(
            deserialize::<Vec<i32>>(context, &list, 0).expect("re-read"),
            vec![1, 2, 3]
        );

        #[derive(Serialize)]
        struct Record {
            right: String,
        }
        let record = serialize(
            context,
            &Record {
                right: "r".to_owned(),
            },
        )
        .expect("struct");
        let re_read = deserialize::<HashMap<String, String>>(context, &record, 0).expect("map");
        assert_eq!(re_read.get("right"), Some(&"r".to_owned()));
    });
}

#[test]
fn enums_round_trip_as_unit_variant_names() {
    #[derive(Debug, PartialEq, Deserialize, Serialize)]
    enum Mode {
        Read,
        Write,
    }

    with_context(|context| {
        let serialized = serialize(context, &Mode::Write).expect("variant");
        assert_eq!(
            deserialize::<Mode>(context, &serialized, 0).expect("variant"),
            Mode::Write
        );
    });
}
