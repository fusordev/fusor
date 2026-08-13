//! ECMA-262 global URI encoding and decoding functions.
//!
//! The behavioral vectors are pinned to `QuickJS` 2026-06-04, while the
//! validity and preservation rules follow the specification's `Encode` and
//! `Decode` abstract operations.

use std::{error::Error, fmt, sync::Arc};

use fusor_bytecode::{VerificationLimits, VerifiedBytecode};
use fusor_compiler::CompilationContext;
use fusor_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use fusor_runtime::{
    Context, DynamicFunctionCompileFailure, ExceptionKind, ExecutionError, ExecutionLimits,
    Function, JsString, JsValue, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource,
    Runtime, RuntimeLimits,
};

#[derive(Debug)]
struct TestCompileError(String);

impl fmt::Display for TestCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for TestCompileError {}

struct TestCompiler;

impl OrdinaryDynamicFunctionCompiler for TestCompiler {
    fn compile(
        &self,
        source: OrdinaryDynamicFunctionSource,
    ) -> Result<Arc<VerifiedBytecode>, DynamicFunctionCompileFailure> {
        let body_text = source.body().to_utf8_lossy().map_err(engine_failure)?;
        let dynamic_source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &[],
            SourceFragment::new(&body_text),
        );
        with_dynamic_function_source(
            dynamic_source,
            FrontendLimits::default(),
            |unit, _prepared| {
                let context = CompilationContext::new_with_source_name(
                    unit,
                    Arc::from("<runtime URI functions>"),
                )
                .map_err(engine_failure)?;
                context
                    .compile_dynamic_function_script(VerificationLimits::default())
                    .map(|tree| Arc::new(tree.verified_bytecode().clone()))
                    .map_err(engine_failure)
            },
        )
        .map_err(engine_failure)?
    }
}

fn engine_failure(error: impl Error + Send + Sync + 'static) -> DynamicFunctionCompileFailure {
    DynamicFunctionCompileFailure::Engine {
        source: Arc::new(TestCompileError(error.to_string())),
    }
}

fn dynamic_function(context: &mut Context<'_>, body: &str) -> Function {
    let authority = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8(body).expect("body"),
        ))
        .expect("dynamic Function authority");
    context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("dynamic Function")
}

fn evaluate<T>(body: &str, project: impl FnOnce(Result<JsValue, ExecutionError>) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, body);
    let result = context.call(&run, &[], ExecutionLimits::default());
    project(result)
}

fn rendered(expression: &str) -> String {
    evaluate(&format!("return String({expression});"), |result| {
        result
            .expect("completed")
            .as_string()
            .expect("live value")
            .expect("String")
            .to_utf8_lossy()
            .expect("UTF-8")
    })
}

fn thrown(body: &str) -> (ExceptionKind, String) {
    evaluate(body, |result| {
        let Err(ExecutionError::Exception(exception)) = result else {
            panic!("expected a JavaScript exception from {body}");
        };
        (
            exception.kind().expect("engine exception kind"),
            exception
                .message()
                .expect("engine exception message")
                .to_utf8_lossy()
                .expect("UTF-8"),
        )
    })
}

fn assert_all(cases: &[(&str, &str)]) {
    for (expression, expected) in cases {
        assert_eq!(rendered(expression), *expected, "{expression}");
    }
}

#[test]
fn encoding_uses_the_rfc_2396_unescaped_sets_and_uppercase_utf8_octets() {
    assert_all(&[
        ("encodeURI(\"AZaz09-_.!~*'()\")", "AZaz09-_.!~*'()"),
        ("encodeURIComponent(\"AZaz09-_.!~*'()\")", "AZaz09-_.!~*'()"),
        ("encodeURI(';/?:@&=+$,#')", ";/?:@&=+$,#"),
        (
            "encodeURIComponent(';/?:@&=+$,#')",
            "%3B%2F%3F%3A%40%26%3D%2B%24%2C%23",
        ),
        ("encodeURI('é')", "%C3%A9"),
        ("encodeURIComponent('😀')", "%F0%9F%98%80"),
        ("encodeURIComponent('\\udbff\\udfff')", "%F4%8F%BF%BF"),
        ("encodeURI('a b')", "a%20b"),
        ("encodeURI('%')", "%25"),
        ("encodeURIComponent('\\0')", "%00"),
    ]);
}

#[test]
fn decoding_preserves_only_complete_uri_reserved_ascii_escapes() {
    assert_all(&[
        (
            "decodeURI('%3B%2F%3F%3A%40%26%3D%2B%24%2C%23')",
            "%3B%2F%3F%3A%40%26%3D%2B%24%2C%23",
        ),
        (
            "decodeURIComponent('%3B%2F%3F%3A%40%26%3D%2B%24%2C%23')",
            ";/?:@&=+$,#",
        ),
        ("decodeURI('%2f')", "%2f"),
        ("decodeURIComponent('%2f')", "/"),
        ("decodeURI('%C3%A9')", "é"),
        ("decodeURIComponent('%F0%9F%98%80')", "😀"),
        (
            "(function(){const value=decodeURIComponent('%F4%8F%BF%BF');return value.length+','+value.charCodeAt(0)+','+value.charCodeAt(1);})()",
            "2,56319,57343",
        ),
        ("decodeURI('plain')", "plain"),
        ("decodeURI('\\ud800').charCodeAt(0)", "55296"),
        ("decodeURIComponent('%00').charCodeAt(0)", "0"),
    ]);
}

#[test]
fn malformed_surrogates_and_utf8_throw_uri_error() {
    for (source, message) in [
        ("return encodeURI('\\ud800');", "expecting surrogate pair"),
        ("return encodeURI('\\udc00');", "invalid character"),
        (
            "return encodeURIComponent('\\ud800x');",
            "expecting surrogate pair",
        ),
        ("return decodeURI('%');", "expecting hex digit"),
        ("return decodeURIComponent('%G0');", "expecting hex digit"),
        ("return decodeURI('%E2x');", "expecting %"),
        ("return decodeURI('%C0%80');", "malformed UTF-8"),
        ("return decodeURI('%E0%80%80');", "malformed UTF-8"),
        ("return decodeURI('%F0%80%80%80');", "malformed UTF-8"),
        ("return decodeURI('%ED%A0%80');", "malformed UTF-8"),
        ("return decodeURI('%F4%90%80%80');", "malformed UTF-8"),
        ("return decodeURI('%E2%28%A1');", "malformed UTF-8"),
        ("return decodeURI('%80');", "malformed UTF-8"),
    ] {
        assert_eq!(
            (ExceptionKind::UriError, message.to_owned()),
            thrown(source)
        );
    }
    assert_all(&[(
        "(function(){try{decodeURI('%C0%80');}catch(error){return error instanceof URIError;}})()",
        "true",
    )]);
}

#[test]
fn uri_arguments_use_resumable_tostring_and_propagate_abrupt_completion() {
    assert_all(&[
        ("encodeURI()", "undefined"),
        ("decodeURIComponent()", "undefined"),
        ("encodeURI(BigInt(10))", "10"),
        (
            "(function(){let log='';const value={toString(){log+='s';return 'a b';},valueOf(){log+='v';return 1;}};return encodeURI(value)+'|'+log;})()",
            "a%20b|s",
        ),
        (
            "(function(){try{decodeURI({toString(){throw 41;}});}catch(error){return error===41;}})()",
            "true",
        ),
        (
            "(function(){try{encodeURIComponent(Symbol('x'));}catch(error){return error instanceof TypeError;}})()",
            "true",
        ),
    ]);
}

#[test]
fn uri_function_identities_descriptors_and_constructor_status_are_exact() {
    assert_all(&[
        ("decodeURI.name+','+decodeURI.length", "decodeURI,1"),
        (
            "decodeURIComponent.name+','+decodeURIComponent.length",
            "decodeURIComponent,1",
        ),
        ("encodeURI.name+','+encodeURI.length", "encodeURI,1"),
        (
            "encodeURIComponent.name+','+encodeURIComponent.length",
            "encodeURIComponent,1",
        ),
        (
            "(function(){const d=Object.getOwnPropertyDescriptor(this,'encodeURI');return d.value===encodeURI&&d.writable&&!d.enumerable&&d.configurable;})()",
            "true",
        ),
        (
            "(function(){try{new encodeURI('x');}catch(error){return error instanceof TypeError;}})()",
            "true",
        ),
    ]);
}

#[test]
fn uri_scans_consume_shared_instruction_fuel() {
    for (function, input) in [
        ("encodeURI", "é".repeat(1_000)),
        ("decodeURIComponent", "%41".repeat(1_000)),
    ] {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let mut context = runtime.context(&realm).expect("context");
        let body = format!("return {function}('{input}');");
        let run = dynamic_function(&mut context, &body);
        let result = context.call(
            &run,
            &[],
            ExecutionLimits::default().with_instruction_fuel(100),
        );
        assert!(
            matches!(
                result,
                Err(ExecutionError::InstructionLimitExceeded { limit: 100, .. })
            ),
            "{function} must charge its input scan"
        );
    }
}
