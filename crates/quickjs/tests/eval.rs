use quickjs::{ScriptEvaluationError, ScriptLimits, evaluate_script};
use quickjs_runtime::{
    ExecutionError, GlobalScriptError, InstallError, JsNumber, Runtime, RuntimeLimits,
    RuntimeResource, ValueKind,
};

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
fn direct_eval_reads_arguments_and_writes_live_lexicals() {
    evaluate(
        "function local(argument){let value=1;eval('value=argument+1');return value;}local(41);",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn strict_direct_eval_writes_the_same_live_lexical_cell() {
    evaluate(
        "function local(){'use strict';let value=1;eval('value=42');return value;}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn direct_eval_preserves_caller_const_assignment_semantics() {
    evaluate(
        "function local(){const value=1;try{eval('value=2');return false;}catch(error){return error.constructor===TypeError&&value===1;}}local();",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn sloppy_direct_eval_ignores_a_named_function_binding_write() {
    evaluate(
        "let named=function self(){eval('self=1');return typeof self;};named();",
        |value| assert_eq!(string(value), "function"),
    );
}

#[test]
fn strict_direct_eval_rejects_a_named_function_binding_write() {
    evaluate(
        "let named=function self(){try{eval('\"use strict\";self=1');return false;}catch(error){return error.constructor===TypeError;}};named();",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn direct_eval_observes_caller_lexical_tdz() {
    evaluate(
        "function local(){try{return eval('value');}catch(error){return error.constructor===ReferenceError;}let value=1;}local();",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn direct_eval_closures_retain_live_caller_cells() {
    evaluate(
        "function local(){let value=1;let read=eval('()=>value');value=42;return read();}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn escaped_direct_eval_closures_retain_caller_cells_after_return() {
    evaluate(
        "function local(){let value=40;return eval('()=>++value');}let increment=local();increment();increment();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn direct_eval_resolves_outer_closures_before_realm_globals() {
    evaluate(
        "var value=1;function outer(value){return function(){return eval('value+1');};}outer(41)();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn direct_eval_unmatched_names_fall_back_to_the_realm_global() {
    evaluate(
        "var realmValue=41;function local(){return eval('realmValue+1');}local();",
        |value| assert!(number(value).strict_equals(JsNumber::from_i32(42))),
    );
}

#[test]
fn sloppy_global_direct_eval_publishes_configurable_vars_and_functions() {
    evaluate(
        "eval('var evalVar=40;function evalFunction(){return 2;}');let descriptor=Object.getOwnPropertyDescriptor(globalThis,'evalVar');evalVar+evalFunction()+'|'+descriptor.configurable;",
        |value| assert_eq!(string(value), "42|true"),
    );
}

#[test]
fn nested_sloppy_global_direct_eval_inherits_the_global_variable_environment() {
    evaluate(
        "eval(\"eval('var nestedEvalVar=42;')\");nestedEvalVar;",
        |value| {
            assert!(number(value).strict_equals(JsNumber::from_i32(42)));
        },
    );
}

#[test]
fn source_strict_global_direct_eval_keeps_var_declarations_local() {
    evaluate(
        "eval(\"'use strict';var strictDirectEvalVar=1;\");typeof strictDirectEvalVar;",
        |value| assert_eq!(string(value), "undefined"),
    );
}

#[test]
fn sloppy_global_direct_eval_var_statement_has_empty_completion() {
    evaluate("eval('var evalOnly;');", |value| {
        assert_eq!(value.kind(), Ok(ValueKind::Undefined));
    });
}

#[test]
fn sloppy_global_direct_eval_rejects_active_block_lexical_collisions() {
    evaluate(
        "{let collision;try{eval('var collision;');false;}catch(error){error.constructor===SyntaxError;}}",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn sloppy_global_direct_eval_rejects_global_lexical_collisions() {
    evaluate(
        "let collision;try{eval('var collision;');false;}catch(error){error.constructor===SyntaxError;}",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn sloppy_global_direct_eval_rejected_properties_throw_type_error() {
    evaluate(
        "Object.preventExtensions(globalThis);try{eval('var unavailable;');false;}catch(error){error.constructor===TypeError;}",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn sloppy_global_direct_eval_function_preflight_is_atomic() {
    evaluate(
        "Object.defineProperty(globalThis,'blocked',{value:1,writable:false,enumerable:false,configurable:false});try{eval('var unpublished;function blocked(){}');}catch(error){}typeof unpublished==='undefined'&&blocked===1;",
        |value| assert_eq!(value.as_boolean(), Ok(Some(true))),
    );
}

#[test]
fn failed_direct_eval_install_rolls_back_promoted_caller_cells() {
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_installed_code(1)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let error = evaluate_script(
        &mut context,
        "function local(){let value=1;return eval('value');}local();",
        "direct-eval-rollback.js",
        ScriptLimits::default(),
    )
    .expect_err("the nested direct-eval installation exceeds the pinned limit");

    assert!(matches!(
        error,
        ScriptEvaluationError::Runtime(GlobalScriptError::Execution(
            ExecutionError::DynamicFunctionInstallation(InstallError::LimitExceeded {
                resource: RuntimeResource::InstalledCode,
                ..
            })
        ))
    ));
    assert_eq!(context.runtime_usage().binding_cells(), 0);
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
