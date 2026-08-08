use quickjs::{ScriptLimits, evaluate_script};
use quickjs_runtime::{JsNumber, Runtime, RuntimeLimits, ValueKind};

fn evaluate<T>(source: &str, inspect: impl FnOnce(&quickjs_runtime::JsValue) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let value = evaluate_script(
        &mut context,
        source,
        "indirect-eval.js",
        ScriptLimits::default(),
    )
    .expect("Script evaluation");
    inspect(&value)
}

fn number(value: &quickjs_runtime::JsValue) -> JsNumber {
    value.as_number().expect("live value").expect("Number")
}

fn string(value: &quickjs_runtime::JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
fn indirect_eval_returns_non_string_arguments_unchanged() {
    evaluate("(0, eval)(41) + 1;", |value| {
        assert!(number(value).strict_equals(JsNumber::from_i32(42)));
    });
}

#[test]
fn eval_intrinsic_has_the_standard_global_descriptor() {
    evaluate(
        "let descriptor = Object.getOwnPropertyDescriptor(globalThis, 'eval'); typeof eval === 'function' && eval.length === 1 && eval.name === 'eval' && descriptor.writable && !descriptor.enumerable && descriptor.configurable;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn closed_direct_eval_returns_the_script_completion() {
    evaluate(
        "function local(){return eval('let answer=40+2;answer;');} local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn indirect_eval_without_an_argument_returns_undefined() {
    evaluate("(0, eval)();", |value| {
        assert_eq!(value.kind(), Ok(ValueKind::Undefined));
    });
}

#[test]
fn indirect_eval_returns_the_script_completion() {
    evaluate("(0, eval)(\"1; 40 + 2;\");", |value| {
        assert!(number(value).strict_equals(JsNumber::from_i32(42)));
    });
}

#[test]
fn indirect_eval_returns_primitive_expression_completions() {
    evaluate(
        "var x; (0, eval)(\"x = 1\") + '|' + (0, eval)(\"1\") + '|' + (0, eval)(\"'1'\") + '|' + (x = 1, (0, eval)(\"++x\"));",
        |value| assert_eq!(string(value), "1|1|1|2"),
    );
}

#[test]
fn indirect_eval_resolves_against_the_realm_global_environment() {
    evaluate(
        "var marker = 1; function local() { let marker = 2; return (0, eval)(\"marker\"); } local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(1))),
    );
}

#[test]
fn sloppy_indirect_eval_publishes_vars_but_not_lexicals() {
    evaluate(
        "(0, eval)(\"var evalVar = 1; let evalLexical = 2; evalVar + evalLexical;\"); evalVar + '|' + typeof evalLexical;",
        |value| assert_eq!(string(value), "1|undefined"),
    );
}

#[test]
fn strict_indirect_eval_keeps_var_declarations_local() {
    evaluate(
        "(0, eval)(\"'use strict'; var strictEvalVar = 1; strictEvalVar;\"); typeof strictEvalVar;",
        |value| assert_eq!(string(value), "undefined"),
    );
}

#[test]
fn sloppy_indirect_eval_publishes_function_declarations() {
    evaluate(
        "(0, eval)(\"function answer() { return 42; }\"); answer();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn indirect_eval_closures_keep_eval_lexicals_alive() {
    evaluate(
        "let closure = (0, eval)(\"let captured = 42; () => captured;\"); closure();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn indirect_eval_syntax_errors_are_catchable_javascript_exceptions() {
    evaluate(
        "try { (0, eval)(\"let = ;\"); false; } catch (error) { error instanceof SyntaxError; }",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn indirect_eval_global_lexical_collisions_throw_syntax_error() {
    evaluate(
        "let collision; try { (0, eval)(\"var collision;\"); false; } catch (error) { error.constructor === SyntaxError; }",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn indirect_eval_rejected_global_properties_throw_type_error() {
    evaluate(
        "Object.preventExtensions(globalThis); try { (0, eval)(\"var unavailable;\"); false; } catch (error) { error.constructor === TypeError; }",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}
