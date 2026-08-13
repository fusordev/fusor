use quickjs::{ScriptLimits, evaluate_script};
use quickjs_runtime::{ExecutionLimits, Runtime, RuntimeLimits};

fn string(value: &quickjs_runtime::JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

fn number(value: &quickjs_runtime::JsValue) -> f64 {
    value
        .as_number()
        .expect("live value")
        .expect("Number")
        .as_f64()
}

fn engine() -> (Runtime, quickjs_runtime::Realm) {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    (runtime, realm)
}

#[test]
fn call_function_runs_natives_that_need_nested_calls() {
    let (mut runtime, realm) = engine();
    let mut context = runtime.context(&realm).expect("context");
    let to_string = evaluate_script(
        &mut context,
        "Object.prototype.toString",
        "call-function-test.js",
        ScriptLimits::default(),
    )
    .expect("intrinsic lookup")
    .into_function()
    .expect("function");
    let receiver = evaluate_script(
        &mut context,
        "new Uint8Array(2)",
        "call-function-test.js",
        ScriptLimits::default(),
    )
    .expect("typed array allocation");
    let result = context
        .call_function(&to_string, receiver, vec![], ExecutionLimits::default())
        .expect("Object.prototype.toString over a typed array");
    assert_eq!(string(&result), "[object Uint8Array]");
}

#[test]
fn call_function_runs_accessor_getters() {
    let (mut runtime, realm) = engine();
    let mut context = runtime.context(&realm).expect("context");
    evaluate_script(
        &mut context,
        "globalThis.__call_function_target = ({ get answer() { return 42; } })",
        "call-function-test.js",
        ScriptLimits::default(),
    )
    .expect("target object");
    let receiver = evaluate_script(
        &mut context,
        "globalThis.__call_function_target",
        "call-function-test.js",
        ScriptLimits::default(),
    )
    .expect("target lookup");
    let getter = evaluate_script(
        &mut context,
        "Object.getOwnPropertyDescriptor(globalThis.__call_function_target, 'answer').get",
        "call-function-test.js",
        ScriptLimits::default(),
    )
    .expect("getter lookup")
    .into_function()
    .expect("function");
    let result = context
        .call_function(&getter, receiver, vec![], ExecutionLimits::default())
        .expect("accessor getter call");
    assert_eq!(number(&result), 42.0);
}

#[test]
fn execute_compiled_script_runs_a_compiled_authority() {
    let (mut runtime, realm) = engine();
    let mut context = runtime.context(&realm).expect("context");
    let compiled = quickjs::compile_script("40 + 2", "compiled-test.js", ScriptLimits::default())
        .expect("compilation");
    let result = quickjs::execute_compiled_script(&mut context, &compiled, ScriptLimits::default())
        .expect("execution");
    assert_eq!(number(&result), 42.0);
}

#[test]
fn has_global_declarations_detects_top_level_bindings() {
    let limits = ScriptLimits::default();
    for (source, expected) in [
        ("let x = 1", true),
        ("class A {}", true),
        ("function f() {}", true),
        ("var v", true),
        ("const k = 2", true),
        ("1 + 1", false),
        ("x = 5", false),
        ("{ let y = 1 }", false),
        ("if (x) { let y = 1 }", false),
        ("for (let i = 0; i < 1; i++) {}", false),
        ("(() => { let z })()", false),
    ] {
        let actual = quickjs::has_global_declarations(source, limits)
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(actual, expected, "has_global_declarations({source:?})");
    }
}
