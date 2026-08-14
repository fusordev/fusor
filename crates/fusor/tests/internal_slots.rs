//! Engine-side internal-slot inspection API tests: the CDP inspector reads
//! Promise, Proxy, collection, and binary-view slots through these
//! `Context` accessors, so every accessor is exercised here through real
//! evaluated objects.

use fusor::{ScriptLimits, evaluate_script};
use fusor_runtime::{CollectionInspection, PromiseInspection, Realm, Runtime, RuntimeLimits};

fn evaluate(context: &mut fusor_runtime::Context<'_>, source: &str) -> fusor_runtime::JsValue {
    evaluate_script(context, source, "internal-slots-test.js", ScriptLimits::default())
        .expect("evaluate fixture")
}

fn runtime() -> (Runtime, Realm) {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    (runtime, realm)
}

#[test]
fn promise_inspection_reports_the_specification_state() {
    let (mut runtime, realm) = runtime();
    let mut context = runtime.context(&realm).expect("context");

    let pending = evaluate(&mut context, "new Promise(() => {})");
    assert!(matches!(
        context
            .promise_inspection(&pending)
            .expect("pending inspection"),
        Some(PromiseInspection::Pending)
    ));

    let fulfilled = evaluate(&mut context, "Promise.resolve(41)");
    match context
        .promise_inspection(&fulfilled)
        .expect("fulfilled inspection")
    {
        Some(PromiseInspection::Fulfilled(value)) => {
            assert_eq!(value.as_f64().expect("number").expect("some"), 41.0);
        }
        other => panic!("expected a fulfilled promise, got {other:?}"),
    }

    let rejected = evaluate(&mut context, "Promise.reject(new Error('nope'))");
    match context
        .promise_inspection(&rejected)
        .expect("rejected inspection")
    {
        Some(PromiseInspection::Rejected(reason)) => {
            assert_eq!(reason.kind().expect("reason kind"), fusor_runtime::ValueKind::Object);
        }
        other => panic!("expected a rejected promise, got {other:?}"),
    }

    let plain = evaluate(&mut context, "({})");
    assert!(
        context
            .promise_inspection(&plain)
            .expect("plain inspection")
            .is_none(),
        "ordinary objects are not promises"
    );
}

#[test]
fn proxy_inspection_reports_handler_and_target() {
    let (mut runtime, realm) = runtime();
    let mut context = runtime.context(&realm).expect("context");

    let proxy = evaluate(&mut context, "new Proxy({ x: 1 }, { y: 2 })");
    let inspection = context
        .proxy_inspection(&proxy)
        .expect("proxy inspection")
        .expect("proxy");
    assert!(!inspection.revoked, "a live proxy is not revoked");
    let handler = inspection.handler.expect("handler");
    let target = inspection.target.expect("target");
    assert_eq!(handler.kind().expect("handler kind"), fusor_runtime::ValueKind::Object);
    assert_eq!(target.kind().expect("target kind"), fusor_runtime::ValueKind::Object);
    let target_key = context.property_key("x").expect("key");
    assert_eq!(
        target
            .into_object()
            .expect("target object")
            .get(&mut context, target_key)
            .expect("target property")
            .as_f64()
            .expect("number")
            .expect("some"),
        1.0,
        "the reported target is the real target object"
    );
    let handler_key = context.property_key("y").expect("key");
    assert_eq!(
        handler
            .into_object()
            .expect("handler object")
            .get(&mut context, handler_key)
            .expect("handler property")
            .as_f64()
            .expect("number")
            .expect("some"),
        2.0,
        "the reported handler is the real handler object"
    );

    let revoked = evaluate(
        &mut context,
        "(() => { const pair = Proxy.revocable({}, {}); pair.revoke(); return pair.proxy; })()",
    );
    let inspection = context
        .proxy_inspection(&revoked)
        .expect("revoked inspection")
        .expect("proxy");
    assert!(inspection.revoked, "a revoked proxy reports the flag");
    assert!(inspection.handler.is_none(), "revocation clears the handler");
    assert!(inspection.target.is_none(), "revocation clears the target");

    let plain = evaluate(&mut context, "({})");
    assert!(
        context
            .proxy_inspection(&plain)
            .expect("plain inspection")
            .is_none(),
        "ordinary objects are not proxies"
    );
}

#[test]
fn collection_inspection_reports_map_and_set_entries() {
    let (mut runtime, realm) = runtime();
    let mut context = runtime.context(&realm).expect("context");

    let map = evaluate(&mut context, "new Map([['a', 1], ['b', 2]])");
    match context
        .collection_inspection(&map)
        .expect("map inspection")
        .expect("map")
    {
        CollectionInspection::Entries(entries) => {
            assert_eq!(entries.len(), 2, "both map entries are reported in order");
            assert_eq!(
                entries[0]
                    .0
                    .as_string()
                    .expect("key")
                    .expect("some")
                    .to_utf8_lossy()
                    .expect("utf8"),
                "a"
            );
            assert_eq!(entries[0].1.as_f64().expect("number").expect("some"), 1.0);
            assert_eq!(
                entries[1]
                    .0
                    .as_string()
                    .expect("key")
                    .expect("some")
                    .to_utf8_lossy()
                    .expect("utf8"),
                "b"
            );
            assert_eq!(entries[1].1.as_f64().expect("number").expect("some"), 2.0);
        }
        other => panic!("expected map entries, got {other:?}"),
    }

    let set = evaluate(&mut context, "new Set([7, 8])");
    match context
        .collection_inspection(&set)
        .expect("set inspection")
        .expect("set")
    {
        CollectionInspection::Values(values) => {
            assert_eq!(values.len(), 2, "both set values are reported in order");
            assert_eq!(values[0].as_f64().expect("number").expect("some"), 7.0);
            assert_eq!(values[1].as_f64().expect("number").expect("some"), 8.0);
        }
        other => panic!("expected set values, got {other:?}"),
    }

    let weak_map = evaluate(&mut context, "new WeakMap([[{}, 9]])");
    match context
        .collection_inspection(&weak_map)
        .expect("weak map inspection")
        .expect("weak map")
    {
        CollectionInspection::Entries(entries) => {
            assert_eq!(entries.len(), 1, "the weak map entry is reported");
            assert_eq!(entries[0].0.kind().expect("key kind"), fusor_runtime::ValueKind::Object);
            assert_eq!(entries[0].1.as_f64().expect("number").expect("some"), 9.0);
        }
        other => panic!("expected weak map entries, got {other:?}"),
    }

    let weak_set = evaluate(&mut context, "new WeakSet([{}])");
    match context
        .collection_inspection(&weak_set)
        .expect("weak set inspection")
        .expect("weak set")
    {
        CollectionInspection::Values(values) => {
            assert_eq!(values.len(), 1, "the weak set value is reported");
            assert_eq!(values[0].kind().expect("value kind"), fusor_runtime::ValueKind::Object);
        }
        other => panic!("expected weak set values, got {other:?}"),
    }

    let plain = evaluate(&mut context, "({})");
    assert!(
        context
            .collection_inspection(&plain)
            .expect("plain inspection")
            .is_none(),
        "ordinary objects are not collections"
    );
}

#[test]
fn array_buffer_inspection_reports_length_and_bytes() {
    let (mut runtime, realm) = runtime();
    let mut context = runtime.context(&realm).expect("context");

    let buffer = evaluate(&mut context, "new ArrayBuffer(16)");
    let inspection = context
        .array_buffer_inspection(&buffer)
        .expect("buffer inspection")
        .expect("buffer");
    assert_eq!(inspection.byte_length, 16);
    assert!(!inspection.detached);
    assert!(!inspection.shared);
    assert_eq!(inspection.max_byte_length, None, "fixed-length buffers have no maximum");
    assert_eq!(
        context
            .array_buffer_bytes(&buffer, 4)
            .expect("buffer bytes")
            .expect("data"),
        vec![0, 0, 0, 0],
        "the byte read is bounded by the limit"
    );

    let sized = evaluate(&mut context, "new Uint8Array([1, 2, 3]).buffer");
    let bytes = context
        .array_buffer_bytes(&sized, 100)
        .expect("sized bytes")
        .expect("data");
    assert_eq!(bytes, vec![1, 2, 3]);

    let plain = evaluate(&mut context, "({})");
    assert!(
        context
            .array_buffer_inspection(&plain)
            .expect("plain inspection")
            .is_none(),
        "ordinary objects are not array buffers"
    );
}

#[test]
fn typed_array_and_data_view_inspection_report_the_view_slots() {
    let (mut runtime, realm) = runtime();
    let mut context = runtime.context(&realm).expect("context");

    let typed = evaluate(&mut context, "new Uint8Array([1, 2, 3])");
    let inspection = context
        .typed_array_inspection(&typed)
        .expect("typed inspection")
        .expect("typed array");
    assert_eq!(inspection.element_name, "Uint8Array");
    assert_eq!(inspection.length, Some(3));
    assert_eq!(inspection.byte_length, Some(3));
    assert_eq!(inspection.byte_offset, 0);
    assert_eq!(
        inspection.buffer.kind().expect("buffer kind"),
        fusor_runtime::ValueKind::Object
    );

    let view = evaluate(&mut context, "new DataView(new ArrayBuffer(8), 2, 4)");
    let inspection = context
        .data_view_inspection(&view)
        .expect("view inspection")
        .expect("data view");
    assert_eq!(inspection.byte_offset, 2);
    assert_eq!(inspection.byte_length, Some(4));
    assert_eq!(
        inspection.buffer.kind().expect("buffer kind"),
        fusor_runtime::ValueKind::Object
    );

    let plain = evaluate(&mut context, "({})");
    assert!(
        context
            .typed_array_inspection(&plain)
            .expect("typed plain")
            .is_none(),
        "ordinary objects are not typed arrays"
    );
    assert!(
        context
            .data_view_inspection(&plain)
            .expect("view plain")
            .is_none(),
        "ordinary objects are not data views"
    );
}

#[test]
fn date_value_and_boxed_primitive_report_the_hidden_payloads() {
    let (mut runtime, realm) = runtime();
    let mut context = runtime.context(&realm).expect("context");

    let date = evaluate(&mut context, "new Date(1234)");
    assert_eq!(
        context
            .date_value(&date)
            .expect("date inspection")
            .expect("date")
            .as_f64(),
        1234.0
    );

    let number = evaluate(&mut context, "new Number(5)");
    assert_eq!(
        context
            .boxed_primitive(&number)
            .expect("number inspection")
            .expect("boxed")
            .as_f64()
            .expect("number")
            .expect("some"),
        5.0
    );

    let boolean = evaluate(&mut context, "new Boolean(true)");
    assert_eq!(
        context
            .boxed_primitive(&boolean)
            .expect("boolean inspection")
            .expect("boxed")
            .as_boolean()
            .expect("boolean")
            .expect("some"),
        true
    );

    let string = evaluate(&mut context, "new String('x')");
    assert_eq!(
        context
            .boxed_primitive(&string)
            .expect("string inspection")
            .expect("boxed")
            .as_string()
            .expect("string")
            .expect("some")
            .to_utf8_lossy()
            .expect("utf8"),
        "x"
    );

    let plain = evaluate(&mut context, "({})");
    assert!(
        context.date_value(&plain).expect("plain date").is_none(),
        "ordinary objects are not dates"
    );
    assert!(
        context
            .boxed_primitive(&plain)
            .expect("plain boxed")
            .is_none(),
        "ordinary objects are not boxed primitives"
    );
}
