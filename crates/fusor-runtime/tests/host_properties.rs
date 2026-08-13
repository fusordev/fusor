//! Host-facing object property API (ECMA-262 internal-method semantics).
//!
//! `Object` was previously opaque to embedding hosts: there was no way to read
//! or write properties from Rust. These tests pin the new
//! `Object::get/set/has/delete/define_own_property/own_property_keys` surface
//! against the same semantics JavaScript observes (getters, setters, proxies,
//! and `ValidateAndApplyPropertyDescriptor` invariants included).

use std::sync::Arc;

use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_runtime::{
    AtomKind, Context, DescriptorFields, ExceptionKind, ExecutionError, ExecutionLimits, JsNumber,
    JsString, JsValue, Runtime, RuntimeLimits, ValueKind,
};

fn compile_global_script(source: &str) -> Arc<fusor_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("host-properties.js"))
                    .expect("storage plan");
            let tree = context
                .compile_global_script(fusor_bytecode::VerificationLimits::default())
                .expect("verified Global Script");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

/// Runs a Global Script setup and hands the context (plus the global object
/// as an `Object` handle) to the test body.
fn with_setup<T>(
    source: &str,
    operation: impl FnOnce(&mut Context<'_>, fusor_runtime::Object) -> T,
) -> T {
    let authority = compile_global_script(source);
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    context
        .execute_global_script(authority, ExecutionLimits::default())
        .expect("setup script");
    let global = context
        .global_object()
        .expect("global object")
        .into_object()
        .expect("global is an object");
    operation(&mut context, global)
}

/// Precomputes one string property key. Keys are interned ahead of the
/// property call they feed so each call site holds only one borrow.
fn key(context: &mut Context<'_>, name: &str) -> fusor_runtime::PropertyKey {
    context.property_key(name).expect("string property key")
}

fn assert_number(value: &JsValue, expected: i32) {
    let actual = value
        .as_number()
        .expect("live value")
        .expect("Number value");
    assert!(
        actual.strict_equals(JsNumber::from_i32(expected)),
        "expected {expected}, produced {actual:?}"
    );
}

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String value")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
fn host_get_reads_own_prototype_and_getter_properties() {
    with_setup(
        "var proto = { inherited: 7 };\
         var o = Object.create(proto); o.own = 3;\
         var acc = { get g() { return 42; } };",
        |context, global| {
            let o_key = key(context, "o");
            let acc_key = key(context, "acc");
            let own = key(context, "own");
            let inherited = key(context, "inherited");
            let missing = key(context, "missing");
            let g = key(context, "g");
            let o = global
                .get(context, o_key)
                .expect("get o")
                .into_object()
                .expect("o is an object");
            let acc = global
                .get(context, acc_key)
                .expect("get acc")
                .into_object()
                .expect("acc is an object");

            assert_number(&o.get(context, own).expect("own"), 3);
            assert_number(&o.get(context, inherited).expect("inherited"), 7);
            assert_eq!(
                o.get(context, missing)
                    .expect("missing")
                    .kind()
                    .expect("live kind"),
                ValueKind::Undefined
            );
            assert_number(&acc.get(context, g).expect("getter"), 42);
        },
    );
}

#[test]
fn host_get_on_a_proxy_runs_the_trap() {
    with_setup(
        "var target = { hidden: 1 };\
         var p = new Proxy(target, { get(t, k, r) { return 'trapped:' + k; } });",
        |context, global| {
            let p_key = key(context, "p");
            let anything = key(context, "anything");
            let proxy = global
                .get(context, p_key)
                .expect("get p")
                .into_object()
                .expect("p is an object");

            let value = proxy.get(context, anything).expect("trapped get");
            assert_eq!(text(&value), "trapped:anything");
        },
    );
}

#[test]
fn host_set_writes_data_properties_and_invokes_setters() {
    with_setup(
        "var o = {};\
         var seen = null;\
         var acc = { set s(v) { seen = v; } };",
        |context, global| {
            let o_key = key(context, "o");
            let acc_key = key(context, "acc");
            let seen_key = key(context, "seen");
            let x = key(context, "x");
            let s = key(context, "s");
            let o = global
                .get(context, o_key)
                .expect("get o")
                .into_object()
                .expect("o is an object");
            let acc = global
                .get(context, acc_key)
                .expect("get acc")
                .into_object()
                .expect("acc is an object");

            o.set(context, x.clone(), context.number(JsNumber::from_i32(5)))
                .expect("data write");
            assert_number(&o.get(context, x).expect("read back"), 5);

            acc.set(context, s, context.number(JsNumber::from_i32(9)))
                .expect("setter write");
            assert_number(&global.get(context, seen_key).expect("setter observed"), 9);
        },
    );
}

#[test]
fn host_set_fails_closed_on_a_frozen_data_property() {
    with_setup("var frozen = Object.freeze({ x: 1 });", |context, global| {
        let frozen_key = key(context, "frozen");
        let x = key(context, "x");
        let frozen = global
            .get(context, frozen_key)
            .expect("get frozen")
            .into_object()
            .expect("frozen is an object");

        let result = frozen.set(context, x, context.number(JsNumber::from_i32(2)));
        match result {
            Err(ExecutionError::Exception(exception)) => {
                assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
            }
            other => panic!("expected a TypeError, got {other:?}"),
        }
    });
}

#[test]
fn host_has_reports_own_inherited_and_missing_properties() {
    with_setup(
        "var proto = { inherited: 1 };\
         var o = Object.create(proto); o.own = 2;",
        |context, global| {
            let o_key = key(context, "o");
            let own = key(context, "own");
            let inherited = key(context, "inherited");
            let absent = key(context, "absent");
            let o = global
                .get(context, o_key)
                .expect("get o")
                .into_object()
                .expect("o is an object");

            assert!(o.has(context, own).expect("own present"));
            assert!(o.has(context, inherited).expect("inherited present"));
            assert!(!o.has(context, absent).expect("absent reported"));
        },
    );
}

#[test]
fn host_delete_removes_configurable_and_refuses_non_configurable() {
    with_setup(
        "var d = { a: 1, b: 2 };\
         Object.defineProperty(d, 'b', { configurable: false });",
        |context, global| {
            let d_key = key(context, "d");
            let a = key(context, "a");
            let b = key(context, "b");
            let d = global
                .get(context, d_key)
                .expect("get d")
                .into_object()
                .expect("d is an object");

            assert!(d.delete(context, a.clone()).expect("deletable"));
            assert_eq!(
                d.get(context, a)
                    .expect("deleted read")
                    .kind()
                    .expect("live kind"),
                ValueKind::Undefined
            );
            assert!(!d.delete(context, b.clone()).expect("non-configurable refused"));
            assert_number(&d.get(context, b).expect("still present"), 2);
        },
    );
}

#[test]
fn host_define_own_property_creates_and_enforces_invariants() {
    with_setup("var o = {};", |context, global| {
        let o_key = key(context, "o");
        let x = key(context, "x");
        let o = global
            .get(context, o_key)
            .expect("get o")
            .into_object()
            .expect("o is an object");

        let non_writable = DescriptorFields::<JsValue> {
            value: Some(context.number(JsNumber::from_i32(1))),
            writable: Some(false),
            enumerable: Some(true),
            configurable: Some(false),
            ..DescriptorFields::new()
        }
        .into_descriptor()
        .expect("data descriptor");
        assert!(o.define_own_property(context, x.clone(), non_writable).expect("create non-writable"));

        // A frozen data property accepts a SameValue rewrite as a no-op.
        let same = DescriptorFields::<JsValue> {
            value: Some(context.number(JsNumber::from_i32(1))),
            ..DescriptorFields::new()
        }
        .into_descriptor()
        .expect("data descriptor");
        assert!(o.define_own_property(context, x.clone(), same).expect("SameValue rewrite"));

        // A different value is rejected with the internal-method Boolean result.
        let different = DescriptorFields::<JsValue> {
            value: Some(context.number(JsNumber::from_i32(2))),
            ..DescriptorFields::new()
        }
        .into_descriptor()
        .expect("data descriptor");
        assert!(!o.define_own_property(context, x.clone(), different).expect("rejected definition reports false"));
        assert_number(&o.get(context, x.clone()).expect("unchanged"), 1);

        // The non-writable invariant also blocks host writes.
        match o.set(context, x, context.number(JsNumber::from_i32(3))) {
            Err(ExecutionError::Exception(exception)) => {
                assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
            }
            other => panic!("expected a TypeError, got {other:?}"),
        }
    });
}

#[test]
fn host_define_own_property_installs_and_validates_accessors() {
    with_setup("var o = {};", |context, global| {
        let o_key = key(context, "o");
        let g = key(context, "g");
        let bad = key(context, "bad");
        let o = global
            .get(context, o_key)
            .expect("get o")
            .into_object()
            .expect("o is an object");

        let getter = context
            .create_host_function("getter", |ctx, _call| {
                Ok(ctx.number(JsNumber::from_i32(99)))
            })
            .expect("host function");
        let accessor = DescriptorFields::<JsValue> {
            get: Some(getter.as_value()),
            enumerable: Some(true),
            ..DescriptorFields::new()
        }
        .into_descriptor()
        .expect("accessor descriptor");
        assert!(o.define_own_property(context, g.clone(), accessor).expect("create accessor"));
        assert_number(&o.get(context, g).expect("getter runs"), 99);

        // A non-callable, non-undefined getter is rejected with a TypeError.
        let invalid = DescriptorFields::<JsValue> {
            get: Some(context.number(JsNumber::from_i32(1))),
            ..DescriptorFields::new()
        }
        .into_descriptor()
        .expect("accessor descriptor");
        match o.define_own_property(context, bad, invalid) {
            Err(ExecutionError::Exception(exception)) => {
                assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
            }
            other => panic!("expected a TypeError, got {other:?}"),
        }
    });
}

#[test]
fn host_own_property_keys_reports_spec_order() {
    with_setup(
        "var sym = Symbol('s');\
         var k = {};\
         k.b = 1;\
         k[0] = 2;\
         k.a = 3;\
         k[sym] = 4;",
        |context, global| {
            let k_key = key(context, "k");
            let k = global
                .get(context, k_key)
                .expect("get k")
                .into_object()
                .expect("k is an object");

            let keys = k.own_property_keys(context).expect("own keys");
            assert_eq!(keys.len(), 4);
            assert_eq!(
                keys[0].as_index(),
                fusor_runtime::ArrayIndex::new(0),
                "integer indices come first, ascending"
            );
            assert_eq!(
                keys[1].as_atom().and_then(fusor_runtime::Atom::description),
                Some(&JsString::from_utf8("b").expect("fixture string")),
                "string keys keep creation order"
            );
            assert_eq!(
                keys[2].as_atom().and_then(fusor_runtime::Atom::description),
                Some(&JsString::from_utf8("a").expect("fixture string"))
            );
            assert_eq!(
                keys[3].as_atom().expect("symbol key atom").kind(),
                AtomKind::Symbol,
                "symbol keys come last"
            );
        },
    );
}

#[test]
fn host_own_property_keys_on_a_proxy_runs_the_trap() {
    with_setup(
        "var p = new Proxy({}, { ownKeys() { return ['x', 'y']; } });",
        |context, global| {
            let p_key = key(context, "p");
            let proxy = global
                .get(context, p_key)
                .expect("get p")
                .into_object()
                .expect("p is an object");

            let keys = proxy.own_property_keys(context).expect("trap keys");
            assert_eq!(keys.len(), 2);
            assert_eq!(
                keys[0].as_atom().and_then(fusor_runtime::Atom::description),
                Some(&JsString::from_utf8("x").expect("fixture string"))
            );
            assert_eq!(
                keys[1].as_atom().and_then(fusor_runtime::Atom::description),
                Some(&JsString::from_utf8("y").expect("fixture string"))
            );
        },
    );
}

#[test]
fn host_property_ops_reject_foreign_handles() {
    let mut other_runtime = Runtime::try_new(RuntimeLimits::default()).expect("other runtime");
    let other_realm = other_runtime.create_realm().expect("other realm");
    let mut other_context = other_runtime.context(&other_realm).expect("other context");
    let other_global = other_context
        .global_object()
        .expect("other global")
        .into_object()
        .expect("object");
    let other_value = other_context.number(JsNumber::from_i32(1));

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let global = context
        .global_object()
        .expect("global object")
        .into_object()
        .expect("object");

    let x = key(&mut context, "x");
    assert!(matches!(
        other_global.get(&mut context, x.clone()),
        Err(ExecutionError::Handle(fusor_runtime::HandleError::ForeignRuntime {
            kind: fusor_runtime::HandleKind::Object
        }))
    ));
    assert!(matches!(
        global.set(&mut context, x, other_value),
        Err(ExecutionError::Handle(fusor_runtime::HandleError::ForeignRuntime {
            kind: fusor_runtime::HandleKind::Value
        }))
    ));
}

#[test]
fn context_property_key_constructs_string_index_and_symbol_keys() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let string_key = context.property_key("hello").expect("string key");
    assert_eq!(
        string_key.as_atom().and_then(fusor_runtime::Atom::description),
        Some(&JsString::from_utf8("hello").expect("fixture string"))
    );

    let index_key = context.property_key("0").expect("index key");
    assert_eq!(index_key.as_index(), fusor_runtime::ArrayIndex::new(0));
    assert!(index_key.as_atom().is_none());

    let symbol = context
        .symbol(Some(&JsString::from_utf8("token").expect("description")))
        .expect("symbol value");
    let symbol_key = context
        .property_key_from_value(&symbol)
        .expect("symbol key");
    assert_eq!(
        symbol_key.as_atom().expect("symbol atom").kind(),
        AtomKind::Symbol
    );

    let number_value = context.number(JsNumber::from_i32(1));
    assert!(matches!(
        context.property_key_from_value(&number_value),
        Err(ExecutionError::Handle(fusor_runtime::HandleError::WrongValueKind {
            expected: ValueKind::String,
            actual: ValueKind::Number,
        }))
    ));
}

#[test]
fn context_global_object_roundtrips_with_set_global() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    context
        .set_global("hostGlobal", context.number(JsNumber::from_i32(5)))
        .expect("set_global");
    let host_global = key(&mut context, "hostGlobal");
    let global = context
        .global_object()
        .expect("global object")
        .into_object()
        .expect("object");
    assert_number(&global.get(&mut context, host_global).expect("read back"), 5);
}
