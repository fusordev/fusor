use quickjs::{
    DynamicFunctionCompilerError, DynamicFunctionConstructionError, DynamicFunctionLimits,
    call_with_dynamic_function_support, construct_dynamic_function,
};
use quickjs_frontend::{DynamicFunctionKind, DynamicFunctionSource, SourceFragment};
use quickjs_runtime::{
    DynamicFunctionScriptError, ExceptionKind, ExecutionError, ExecutionLimits, JsNumber, Runtime,
    RuntimeLimits, ValueKind,
};

fn source<'source>(
    parameters: &'source [SourceFragment<'source>],
    body: &'source str,
) -> DynamicFunctionSource<'source> {
    DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        parameters,
        SourceFragment::new(body),
    )
}

#[test]
fn ordinary_dynamic_function_compiles_the_whole_wrapper_and_executes() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let parameters = [SourceFragment::new("value")];

    let completion = construct_dynamic_function(
        &mut context,
        source(&parameters, "return value;"),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function");
    assert_eq!(
        completion.prepared_source().generated_source(),
        "(function anonymous(value\n) {\nreturn value;\n})"
    );
    let function = completion
        .into_value()
        .into_function()
        .expect("ordinary wrapper completion");
    let seven = context.number(JsNumber::from_i32(7));
    let result = context
        .call(&function, &[seven], ExecutionLimits::default())
        .expect("call constructed function");
    assert!(
        result
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(7))
    );
}

#[test]
fn facade_call_supplies_the_real_oxc_compiler_to_global_function() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = construct_dynamic_function(
        &mut context,
        source(&[], "return new Function('return 9;')();"),
        DynamicFunctionLimits::default(),
    )
    .expect("outer dynamic Function")
    .into_value()
    .into_function()
    .expect("outer function");

    let value = call_with_dynamic_function_support(
        &mut context,
        &run,
        &[],
        DynamicFunctionLimits::default(),
    )
    .expect("nested global Function");

    assert!(
        value
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(9))
    );
}

#[test]
fn facade_wrapper_escape_can_invoke_global_function_during_script_execution() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let value = construct_dynamic_function(
        &mut context,
        source(&[], "}), Function('return 6;')() || (function(){"),
        DynamicFunctionLimits::default(),
    )
    .expect("wrapper escape with nested Function")
    .into_value();

    assert!(
        value
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(6))
    );
}

#[test]
fn wrapper_escape_returns_the_complete_script_completion() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let completion = construct_dynamic_function(
        &mut context,
        source(&[], "}), ({ marker: 1"),
        DynamicFunctionLimits::default(),
    )
    .expect("QuickJS-compatible wrapper escape");

    assert_eq!(
        completion.value().kind().expect("live completion"),
        ValueKind::Object
    );
}

#[test]
fn wrapper_escape_observes_the_constructor_realm_global_receiver() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let body = "}), (function(){}) ? this : (function(){";

    let first = construct_dynamic_function(
        &mut context,
        source(&[], body),
        DynamicFunctionLimits::default(),
    )
    .expect("first escaped Script receiver")
    .into_value()
    .into_object()
    .expect("global object");
    let second = construct_dynamic_function(
        &mut context,
        source(&[], body),
        DynamicFunctionLimits::default(),
    )
    .expect("second escaped Script receiver")
    .into_value()
    .into_object()
    .expect("global object");

    assert!(
        first
            .same_identity(&second)
            .expect("same-runtime object identities")
    );
}

#[test]
fn sloppy_dynamic_function_this_uses_its_constructor_realm_global() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = construct_dynamic_function(
        &mut context,
        source(&[], "return this;"),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function")
    .into_value()
    .into_function()
    .expect("ordinary dynamic function");
    let expected = construct_dynamic_function(
        &mut context,
        source(&[], "}), (function(){}) ? this : (function(){"),
        DynamicFunctionLimits::default(),
    )
    .expect("escaped Script receiver")
    .into_value()
    .into_object()
    .expect("constructor-realm global object");

    let actual = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("sloppy call")
        .into_object()
        .expect("constructor-realm global object");
    assert!(
        actual
            .same_identity(&expected)
            .expect("same-runtime object identities")
    );
}

#[test]
fn separately_constructed_functions_share_constructor_realm_globals() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let setter = construct_dynamic_function(
        &mut context,
        source(&[], "facadeMarker = 7; return facadeMarker;"),
        DynamicFunctionLimits::default(),
    )
    .expect("global setter")
    .into_value()
    .into_function()
    .expect("setter function");
    let getter = construct_dynamic_function(
        &mut context,
        source(&[], "return facadeMarker;"),
        DynamicFunctionLimits::default(),
    )
    .expect("global getter")
    .into_value()
    .into_function()
    .expect("getter function");

    let set = context
        .call(&setter, &[], ExecutionLimits::default())
        .expect("global write");
    assert!(
        set.as_number()
            .expect("live setter result")
            .expect("numeric setter result")
            .strict_equals(JsNumber::from_i32(7))
    );
    let get = context
        .call(&getter, &[], ExecutionLimits::default())
        .expect("global read");
    assert!(
        get.as_number()
            .expect("live getter result")
            .expect("numeric getter result")
            .strict_equals(JsNumber::from_i32(7))
    );
}

#[test]
fn escaped_program_var_is_instantiated_once_in_the_constructor_realm() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    construct_dynamic_function(
        &mut context,
        source(&[], "}); var escapedVar = 5; (function(){"),
        DynamicFunctionLimits::default(),
    )
    .expect("initialized Program var");
    construct_dynamic_function(
        &mut context,
        source(&[], "}); var escapedVar; (function(){"),
        DynamicFunctionLimits::default(),
    )
    .expect("uninitialized redeclaration preserves the property value");
    let getter = construct_dynamic_function(
        &mut context,
        source(&[], "return escapedVar;"),
        DynamicFunctionLimits::default(),
    )
    .expect("separate construction resolves the persisted var")
    .into_value()
    .into_function()
    .expect("getter function");

    let value = context
        .call(&getter, &[], ExecutionLimits::default())
        .expect("read escaped Program var")
        .as_number()
        .expect("live result")
        .expect("number result");
    assert!(value.strict_equals(JsNumber::from_i32(5)));
}

#[test]
fn escaped_program_lexical_is_private_but_survives_through_a_closure() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let reader = construct_dynamic_function(
        &mut context,
        source(&[], "}); let hidden = 2; (function(){ return hidden;"),
        DynamicFunctionLimits::default(),
    )
    .expect("escaped lexical closure")
    .into_value()
    .into_function()
    .expect("reader function");
    let hidden = context
        .call(&reader, &[], ExecutionLimits::default())
        .expect("captured lexical read")
        .as_number()
        .expect("live result")
        .expect("number result");
    assert!(hidden.strict_equals(JsNumber::from_i32(2)));

    let global_probe = construct_dynamic_function(
        &mut context,
        source(&[], "return typeof hidden;"),
        DynamicFunctionLimits::default(),
    )
    .expect("separate global probe")
    .into_value()
    .into_function()
    .expect("probe function");
    let probe = context
        .call(&global_probe, &[], ExecutionLimits::default())
        .expect("probe result");
    assert_eq!(
        probe
            .as_string()
            .expect("live result")
            .expect("string result")
            .to_utf8_lossy()
            .expect("short ASCII string"),
        "undefined"
    );
}

#[test]
fn escaped_program_function_is_hoisted_and_captures_program_lexicals() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let resolver = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "}); let before = declared(); function declared(){ return cell; } \
             let cell = 8; (function(){ return before;",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect_err("calling the hoisted function before its lexical initializes must throw");
    let DynamicFunctionConstructionError::Runtime {
        source: DynamicFunctionScriptError::Execution(ExecutionError::Exception(tdz)),
        ..
    } = resolver
    else {
        panic!("hoisted call must fail with a JavaScript exception");
    };
    assert_eq!(tdz.kind(), Some(ExceptionKind::ReferenceError));
    assert_eq!(
        tdz.message()
            .expect("engine-created error message")
            .to_utf8_lossy()
            .expect("short ASCII message"),
        "cell is not initialized"
    );

    let resolver = construct_dynamic_function(
        &mut context,
        source(
            &[],
            "}); function declared(){ return cell; } let cell = 8; \
             (function(){ return declared;",
        ),
        DynamicFunctionLimits::default(),
    )
    .expect("hoisted Program function")
    .into_value()
    .into_function()
    .expect("resolver function");
    let declared = context
        .call(&resolver, &[], ExecutionLimits::default())
        .expect("resolve declaration")
        .into_function()
        .expect("declared function");
    let result = context
        .call(&declared, &[], ExecutionLimits::default())
        .expect("declared function captures Program lexical")
        .as_number()
        .expect("live result")
        .expect("number result");
    assert!(result.strict_equals(JsNumber::from_i32(8)));
}

#[test]
fn named_anonymous_binding_is_initialized_to_the_constructed_function() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let completion = construct_dynamic_function(
        &mut context,
        source(&[], "return anonymous;"),
        DynamicFunctionLimits::default(),
    )
    .expect("dynamic Function");
    let function = completion
        .into_value()
        .into_function()
        .expect("function completion");
    let returned = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("self read")
        .into_function()
        .expect("self function");
    assert!(function.same_identity(&returned).expect("same runtime"));
}

#[test]
fn direct_eval_remains_fail_closed_without_installing_code() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let before = runtime.usage();
    {
        let mut context = runtime.context(&realm).expect("context");
        let error = construct_dynamic_function(
            &mut context,
            source(&[], "return eval('1');"),
            DynamicFunctionLimits::default(),
        )
        .expect_err("direct eval remains unsupported");
        assert!(matches!(
            &error,
            DynamicFunctionConstructionError::Compiler {
                source: DynamicFunctionCompilerError::Planning(_),
                ..
            }
        ));
        assert!(error.prepared_source().is_some());
    }
    assert_eq!(runtime.usage(), before);
}

#[test]
fn syntax_failure_retains_the_exact_wrapper_without_installing_code() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let before = runtime.usage();
    {
        let mut context = runtime.context(&realm).expect("context");
        let error = construct_dynamic_function(
            &mut context,
            source(&[], "return ("),
            DynamicFunctionLimits::default(),
        )
        .expect_err("malformed body");
        assert!(matches!(
            &error,
            DynamicFunctionConstructionError::Frontend(_)
        ));
        assert_eq!(
            error
                .prepared_source()
                .expect("parser failure retains wrapper")
                .generated_source(),
            "(function anonymous(\n) {\nreturn (\n})"
        );
    }
    assert_eq!(runtime.usage(), before);
}

#[test]
fn runtime_failure_retains_the_wrapper_and_releases_internal_script_state() {
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_public_roots(0)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let before = runtime.usage();
    {
        let mut context = runtime.context(&realm).expect("context");
        let error = construct_dynamic_function(
            &mut context,
            source(&[], "return 1;"),
            DynamicFunctionLimits::default(),
        )
        .expect_err("public completion root exceeds the runtime limit");
        assert!(matches!(
            &error,
            DynamicFunctionConstructionError::Runtime { .. }
        ));
        assert_eq!(
            error
                .prepared_source()
                .expect("runtime failure retains wrapper")
                .generated_source(),
            "(function anonymous(\n) {\nreturn 1;\n})"
        );
    }

    runtime
        .collect_cycles()
        .expect("collect unpublished wrapper");
    assert_eq!(runtime.usage(), before);
}

#[test]
fn nonordinary_dynamic_function_families_are_rejected_before_parsing() {
    for kind in [
        DynamicFunctionKind::GeneratorFunction,
        DynamicFunctionKind::AsyncFunction,
        DynamicFunctionKind::AsyncGeneratorFunction,
    ] {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let mut context = runtime.context(&realm).expect("context");
        let input = DynamicFunctionSource::new(kind, &[], SourceFragment::new(""));
        assert!(matches!(
            construct_dynamic_function(
                &mut context,
                input,
                DynamicFunctionLimits::default()
            ),
            Err(DynamicFunctionConstructionError::UnsupportedKind { kind: actual })
                if actual == kind
        ));
    }
}
