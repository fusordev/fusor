//! Engine-created error stacks: every interpreter-thrown error (V8 style)
//! carries a `Name: message` header line above its frames, and dynamic
//! import syntax rejections carry the header too.

use fusor::{ScriptLimits, evaluate_script};
use fusor_runtime::{Runtime, RuntimeLimits};

fn evaluate(context: &mut fusor_runtime::Context<'_>, source: &str) -> fusor_runtime::JsValue {
    evaluate_script(
        context,
        source,
        "error-stacks-test.js",
        ScriptLimits::default(),
    )
    .expect("evaluate fixture")
}

fn string_value(value: &fusor_runtime::JsValue) -> String {
    value
        .as_string()
        .expect("string")
        .expect("some")
        .to_utf8_lossy()
        .expect("utf8")
}

#[test]
fn eval_syntax_failures_throw_stack_carrying_syntax_errors() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let stack = evaluate(
        &mut context,
        "(function () { try { eval('@'); } catch (e) { return e.constructor.name + '|' + e.stack; } })()",
    );
    let text = string_value(&stack);
    assert!(
        text.starts_with("SyntaxError|SyntaxError: "),
        "the caught value is a real SyntaxError whose stack carries the V8 header: {text:?}"
    );
    assert!(
        text.contains("    at "),
        "the stack keeps its frame lines: {text:?}"
    );
}

#[test]
fn function_constructor_syntax_failures_throw_stack_carrying_syntax_errors() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let stack = evaluate(
        &mut context,
        "(function () { try { new Function('@'); } catch (e) { return e.constructor.name + '|' + e.stack; } })()",
    );
    let text = string_value(&stack);
    assert!(
        text.starts_with("SyntaxError|SyntaxError: "),
        "the caught value is a real SyntaxError whose stack carries the V8 header: {text:?}"
    );
}

#[test]
fn interpreter_type_errors_carry_the_v8_stack_header() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let stack = evaluate(
        &mut context,
        "(function () { try { null.x; } catch (e) { return e.constructor.name + '|' + e.stack; } })()",
    );
    let text = string_value(&stack);
    assert!(
        text.starts_with("TypeError|TypeError: "),
        "interpreter-thrown TypeErrors carry the V8 header: {text:?}"
    );
    assert!(
        text.contains("    at "),
        "the stack keeps its frame lines: {text:?}"
    );
}
