use fusor::{ScriptLimits, evaluate_script};
use fusor_runtime::{Runtime, RuntimeLimits};

#[test]
fn context_reports_proxy_objects() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let proxy = evaluate_script(
        &mut context,
        "new Proxy({}, {})",
        "proxy-introspection-test.js",
        ScriptLimits::default(),
    )
    .expect("proxy allocation")
    .into_object()
    .expect("object");
    assert!(
        context
            .object_is_proxy(&proxy.as_value())
            .expect("proxy check"),
        "Proxy instances are reported"
    );

    let plain = evaluate_script(
        &mut context,
        "({})",
        "proxy-introspection-test.js",
        ScriptLimits::default(),
    )
    .expect("object literal")
    .into_object()
    .expect("object");
    assert!(
        !context
            .object_is_proxy(&plain.as_value())
            .expect("proxy check"),
        "ordinary objects are not proxies"
    );

    let proxy_of_array = evaluate_script(
        &mut context,
        "new Proxy([], {})",
        "proxy-introspection-test.js",
        ScriptLimits::default(),
    )
    .expect("array proxy")
    .into_object()
    .expect("object");
    assert!(
        context
            .object_is_proxy(&proxy_of_array.as_value())
            .expect("proxy check"),
        "proxies over arrays are still proxies"
    );
}
