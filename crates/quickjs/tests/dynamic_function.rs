use quickjs::{
    DynamicFunctionCompilerError, DynamicFunctionConstructionError, DynamicFunctionLimits,
    construct_dynamic_function,
};
use quickjs_frontend::{DynamicFunctionKind, DynamicFunctionSource, SourceFragment};
use quickjs_runtime::{ExecutionLimits, JsNumber, Runtime, RuntimeLimits, ValueKind};

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
