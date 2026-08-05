use quickjs::{ScriptEvaluationError, ScriptLimits, evaluate_script};
use quickjs_runtime::{
    ErrorObjectKind, ExceptionKind, ExecutionError, GlobalScriptError, JsNumber, Runtime,
    RuntimeLimits,
};

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

fn exception_kind(error: ScriptEvaluationError) -> ExceptionKind {
    let ScriptEvaluationError::Runtime(GlobalScriptError::Execution(ExecutionError::Exception(
        exception,
    ))) = error
    else {
        panic!("expected a JavaScript exception");
    };
    exception.kind().expect("engine exception kind")
}

#[test]
fn global_script_executes_as_a_whole_graph_and_retains_object_bindings() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let first = evaluate_script(
        &mut context,
        "var total = 40; function add(value) { return total + value; } add(2);",
        "object-globals.js",
        ScriptLimits::default(),
    )
    .expect("first Script");
    assert!(number(&first).strict_equals(JsNumber::from_i32(42)));

    let second = evaluate_script(
        &mut context,
        "total += 1; add(2);",
        "object-globals-followup.js",
        ScriptLimits::default(),
    )
    .expect("second Script");
    assert!(number(&second).strict_equals(JsNumber::from_i32(43)));
}

#[test]
fn global_anonymous_function_initializers_receive_their_binding_name() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let value = evaluate_script(
        &mut context,
        "var inferred = function () { return inferred.name; }; inferred();",
        "global-inferred-name.js",
        ScriptLimits::default(),
    )
    .expect("global anonymous function initializer");
    assert_eq!(string(&value), "inferred");
}

#[test]
fn global_lexical_bindings_persist_and_are_captured_by_nested_functions() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let first = evaluate_script(
        &mut context,
        "let lexical = 4; const fixed = 6; function read() { return lexical + fixed; } read();",
        "lexical-globals.js",
        ScriptLimits::default(),
    )
    .expect("lexical Script");
    assert!(number(&first).strict_equals(JsNumber::from_i32(10)));

    let second = evaluate_script(
        &mut context,
        "lexical = 7; read();",
        "lexical-globals-followup.js",
        ScriptLimits::default(),
    )
    .expect("lexical follow-up Script");
    assert!(number(&second).strict_equals(JsNumber::from_i32(13)));
}

#[test]
fn global_lexical_access_preserves_tdz_and_immutable_assignment_errors() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let tdz = evaluate_script(
        &mut context,
        "pending; let pending = 1;",
        "tdz.js",
        ScriptLimits::default(),
    )
    .expect_err("TDZ read");
    assert_eq!(exception_kind(tdz), ExceptionKind::ReferenceError);

    let immutable = evaluate_script(
        &mut context,
        "const fixed = 1; fixed = 2;",
        "immutable.js",
        ScriptLimits::default(),
    )
    .expect_err("immutable assignment");
    assert_eq!(exception_kind(immutable), ExceptionKind::TypeError);
}

#[test]
fn global_destructuring_initializes_realm_lexical_cells() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let first = evaluate_script(
        &mut context,
        "let [left, right] = [1, 2]; const { value } = { value: 3 }; left + right + value;",
        "destructuring-globals.js",
        ScriptLimits::default(),
    )
    .expect("destructuring Script");
    assert!(number(&first).strict_equals(JsNumber::from_i32(6)));

    let second = evaluate_script(
        &mut context,
        "left = 4; left + right + value;",
        "destructuring-globals-followup.js",
        ScriptLimits::default(),
    )
    .expect("destructuring follow-up Script");
    assert!(number(&second).strict_equals(JsNumber::from_i32(9)));
}

#[test]
fn abrupt_scripts_keep_instantiated_bindings_and_report_exact_source_names() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let first = evaluate_script(
        &mut context,
        "pending; let pending = 1;",
        "abrupt-origin.js",
        ScriptLimits::default(),
    )
    .expect_err("first TDZ read");
    let ScriptEvaluationError::Runtime(GlobalScriptError::Execution(ExecutionError::Exception(
        exception,
    ))) = first
    else {
        panic!("expected first JavaScript exception");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::ReferenceError));
    assert_eq!(exception.source_name(), "abrupt-origin.js");

    let second = evaluate_script(
        &mut context,
        "pending;",
        "abrupt-followup.js",
        ScriptLimits::default(),
    )
    .expect_err("the uninitialized realm binding persists");
    assert_eq!(exception_kind(second), ExceptionKind::ReferenceError);
}

#[test]
fn cross_script_global_declaration_conflicts_are_syntax_errors() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    evaluate_script(
        &mut context,
        "let conflict = 1;",
        "first-declaration.js",
        ScriptLimits::default(),
    )
    .expect("first declaration");
    let error = evaluate_script(
        &mut context,
        "var conflict;",
        "conflicting-declaration.js",
        ScriptLimits::default(),
    )
    .expect_err("conflicting global declaration");
    assert_eq!(exception_kind(error), ExceptionKind::SyntaxError);
}

#[test]
fn realm_lexical_cells_keep_heap_values_alive_across_cycle_collection() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    {
        let mut context = runtime.context(&realm).expect("context");
        evaluate_script(
            &mut context,
            "let held = { answer: 42 };",
            "lexical-root.js",
            ScriptLimits::default(),
        )
        .expect("lexical object declaration");
    }

    let report = runtime.collect_cycles().expect("cycle collection");
    assert_eq!(report.binding_cells(), 0);
    let mut context = runtime.context(&realm).expect("context after collection");
    let value = evaluate_script(
        &mut context,
        "held.answer;",
        "lexical-root-followup.js",
        ScriptLimits::default(),
    )
    .expect("realm lexical survives collection");
    assert!(number(&value).strict_equals(JsNumber::from_i32(42)));
}

#[test]
fn explicit_error_objects_can_be_classified_without_observable_property_access() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let observer_realm = runtime.create_realm().expect("observer realm");
    let exception = {
        let mut context = runtime.context(&realm).expect("context");
        let error = evaluate_script(
            &mut context,
            "throw new EvalError('expected');",
            "explicit-error.js",
            ScriptLimits::default(),
        )
        .expect_err("explicit throw");
        let ScriptEvaluationError::Runtime(GlobalScriptError::Execution(
            ExecutionError::Exception(exception),
        )) = error
        else {
            panic!("expected a JavaScript exception");
        };
        exception
    };
    assert_eq!(exception.kind(), None);
    let observer = runtime.context(&observer_realm).expect("observer context");
    assert_eq!(
        observer
            .error_object_kind(exception.thrown_value().expect("thrown value"))
            .expect("classification"),
        Some(ErrorObjectKind::EvalError)
    );
}

#[test]
fn untagged_templates_apply_tostring_in_substitution_order_without_builtin_lookup() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let value = evaluate_script(
        &mut context,
        r#"
            var order = "";
            var first = {
                toString() { order += "toString"; return "A"; },
                valueOf() { order += "valueOf"; return 1; }
            };
            String.prototype.concat = function () { throw new Error("observable concat"); };
            var rendered = `head:${first}:${(order += "|second", "B")}\n`;
            rendered + "|" + order;
        "#,
        "template-order.js",
        ScriptLimits::default(),
    )
    .expect("untagged template");

    assert_eq!(string(&value), "head:A:B\n|toString|second");
}

#[test]
fn untagged_template_rejects_symbol_substitutions() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let error = evaluate_script(
        &mut context,
        "`${Symbol('description')}`;",
        "template-symbol.js",
        ScriptLimits::default(),
    )
    .expect_err("template ToString(Symbol)");
    assert_eq!(exception_kind(error), ExceptionKind::TypeError);
}
