use std::sync::Arc;

use quickjs::{DynamicFunctionLimits, OxcDynamicFunctionCompiler};
use quickjs_bytecode::CompilerExecutableKind;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, SourceFragment, with_dynamic_function_source,
};
use quickjs_runtime::{
    DynamicFunctionCompileFailure, JsString, OrdinaryDynamicFunctionCompiler,
    OrdinaryDynamicFunctionSource,
};

fn string(value: &str) -> JsString {
    JsString::from_utf8(value).expect("test string")
}

fn source(parameters: &[&str], body: &str) -> OrdinaryDynamicFunctionSource {
    let parameters = parameters.iter().copied().map(string).collect::<Vec<_>>();
    OrdinaryDynamicFunctionSource::new(Arc::from(parameters), string(body))
}

#[test]
fn service_returns_the_complete_verified_dynamic_function_graph() {
    let compiler = OxcDynamicFunctionCompiler::new(DynamicFunctionLimits::default());
    let authority = compiler
        .compile(source(
            &["value"],
            "return function nested(){ return value; };",
        ))
        .expect("ordinary dynamic Function");

    assert_eq!(
        authority.root().metadata().executable_kind(),
        CompilerExecutableKind::DynamicFunctionScript
    );
    assert_eq!(authority.functions().len(), 3);
    assert!(
        authority
            .functions()
            .skip(1)
            .all(|function| function.metadata().executable_kind()
                == CompilerExecutableKind::OrdinaryFunction)
    );
}

#[test]
fn parser_failure_uses_the_primary_normalized_diagnostic_as_syntax_message() {
    let body = "return (";
    let frontend_error = with_dynamic_function_source(
        DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &[],
            SourceFragment::new(body),
        ),
        DynamicFunctionLimits::default().frontend(),
        |_unit, _prepared| (),
    )
    .expect_err("malformed wrapper");
    let expected = &frontend_error
        .diagnostics()
        .first()
        .expect("normalized primary diagnostic")
        .message;

    let compiler = OxcDynamicFunctionCompiler::new(DynamicFunctionLimits::default());
    let error = compiler
        .compile(source(&[], body))
        .expect_err("malformed wrapper");
    let DynamicFunctionCompileFailure::Syntax { message } = error else {
        panic!("parser rejection must become Syntax");
    };
    assert_eq!(
        message.to_utf8_lossy().expect("diagnostic UTF-8"),
        *expected
    );
}

#[test]
fn quickjs_profile_failure_is_a_syntax_error() {
    let compiler = OxcDynamicFunctionCompiler::new(DynamicFunctionLimits::default());
    let error = compiler
        .compile(source(&[], "using resource = acquire();"))
        .expect_err("syntax outside the pinned QuickJS profile");

    let DynamicFunctionCompileFailure::Syntax { message } = error else {
        panic!("compatibility-profile rejection must become Syntax");
    };
    assert!(
        message
            .to_utf8_lossy()
            .expect("diagnostic UTF-8")
            .contains("does not support `using` declarations")
    );
}

#[test]
fn direct_eval_remains_an_engine_rejection() {
    let compiler = OxcDynamicFunctionCompiler::new(DynamicFunctionLimits::default());
    let error = compiler
        .compile(source(&[], "return eval('1');"))
        .expect_err("direct eval remains fail closed");

    assert!(error.syntax_message().is_none());
    let detail = error.engine_source().expect("engine source").to_string();
    assert!(detail.contains("compiler-planning"), "{detail}");
    assert!(detail.contains("DirectEval"), "{detail}");
}

#[test]
fn lone_surrogates_are_never_lossily_forwarded_to_oxc() {
    let compiler = OxcDynamicFunctionCompiler::new(DynamicFunctionLimits::default());
    let body = JsString::from_code_units([0xd800]).expect("lone surrogate JavaScript string");
    let error = compiler
        .compile(OrdinaryDynamicFunctionSource::new(Arc::from([]), body))
        .expect_err("Oxc accepts only losslessly representable UTF-8");

    assert!(error.syntax_message().is_none());
    let detail = error.engine_source().expect("engine source").to_string();
    assert!(detail.contains("source-conversion"), "{detail}");
    assert!(detail.contains("U+D800"), "{detail}");
}
