//! The `String` code-unit factories, pinned to the specification.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'console.log(String.fromCharCode(65601),\
//!     String.fromCodePoint(0x1F600).length);'
//! A 2
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! fromCharCode(104,105) => "hi"      fromCharCode() => ""
//! fromCharCode(65.9) => "A"          fromCharCode(65601) => "A"
//! fromCharCode(-1).charCodeAt(0) => 65535
//! fromCharCode(65536).charCodeAt(0) => 0
//! fromCharCode(NaN).charCodeAt(0) => 0
//! fromCharCode(Infinity).charCodeAt(0) => 0
//! fromCharCode(1.9).charCodeAt(0) => 1
//! fromCodePoint(65,66) => "AB"       fromCodePoint() => ""
//! fromCodePoint(0).length => 1       fromCodePoint(0x1F600).length => 2
//! fromCodePoint(0x10FFFF).length => 2
//! fromCodePoint(0xD800).charCodeAt(0) => 55296
//! fromCodePoint(0x1F600).codePointAt(0) => 128512
//! fromCodePoint(1.5) !! RangeError: invalid code point
//! fromCodePoint(0x110000) !! RangeError: invalid code point
//! fromCodePoint(-1) !! RangeError: invalid code point
//! fromCodePoint(NaN) !! RangeError: invalid code point
//! fromCodePoint(Infinity) !! RangeError: invalid code point
//! fromCharCode.length => 1           fromCodePoint.length => 1
//! ```

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
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
                    Arc::from("<runtime String statics>"),
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

/// Evaluates `expression` and renders the result with `String()`.
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

/// Returns the thrown exception's kind and message.
fn thrown(body: &str) -> (ExceptionKind, String) {
    evaluate(body, |result| {
        let Err(ExecutionError::Exception(exception)) = result else {
            panic!("expected a JavaScript throw from {body}");
        };
        let kind = exception.kind().expect("engine exception kind");
        let message = exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("UTF-8");
        (kind, message)
    })
}

fn assert_throws(body: &str, kind: ExceptionKind, message: &str) {
    assert_eq!((kind, message.to_owned()), thrown(body), "{body}");
}

/// Asserts a table of `expression => rendered result` pairs.
fn assert_all(cases: &[(&str, &str)]) {
    for (expression, expected) in cases {
        assert_eq!(rendered(expression), *expected, "{expression}");
    }
}

/// `String.fromCharCode` applies `ToUint16`, so it wraps instead of failing.
///
/// This is the whole difference from `fromCodePoint`: every argument is reduced
/// modulo 2^16, which is why `String.fromCharCode(65601)` is `"A"`.
#[test]
fn from_char_code_wraps_each_argument_to_a_code_unit() {
    assert_all(&[
        ("String.fromCharCode(104,105)", "hi"),
        ("String.fromCharCode(65)", "A"),
        ("String.fromCharCode()", ""),
        ("String.fromCharCode(104,105).length", "2"),
        // The argument truncates toward zero before wrapping.
        ("String.fromCharCode(65.9)", "A"),
        // 65601 is 65 + 2^16, so it wraps onto the same code unit.
        ("String.fromCharCode(65601)", "A"),
        ("String.fromCharCode(-1).charCodeAt(0)", "65535"),
        ("String.fromCharCode(65536).charCodeAt(0)", "0"),
        ("String.fromCharCode(NaN).charCodeAt(0)", "0"),
        ("String.fromCharCode(Infinity).charCodeAt(0)", "0"),
        ("String.fromCharCode(1.9).charCodeAt(0)", "1"),
        // An object argument converts through `valueOf`.
        ("String.fromCharCode({valueOf(){return 65;}})", "A"),
    ]);
}

/// `String.fromCodePoint` builds a surrogate pair for a supplementary value.
#[test]
fn from_code_point_encodes_supplementary_values_as_surrogate_pairs() {
    assert_all(&[
        ("String.fromCodePoint(65)", "A"),
        ("String.fromCodePoint(65,66)", "AB"),
        ("String.fromCodePoint()", ""),
        ("String.fromCodePoint(0).length", "1"),
        ("String.fromCodePoint(-0).length", "1"),
        // A supplementary code point occupies two UTF-16 code units.
        ("String.fromCodePoint(0x1F600).length", "2"),
        ("String.fromCodePoint(0x10FFFF).length", "2"),
        // The pair round-trips through `codePointAt`.
        ("String.fromCodePoint(0x1F600).codePointAt(0)", "128512"),
        // A lone surrogate is a valid code point and is emitted unchanged.
        ("String.fromCodePoint(0xD800).charCodeAt(0)", "55296"),
        ("String.fromCodePoint({valueOf(){return 65;}})", "A"),
    ]);
}

/// `String.fromCodePoint` rejects anything that is not an exact code point.
///
/// Unlike `fromCharCode`, it never wraps: a fractional, negative, non-finite, or
/// out-of-range argument is a `RangeError`.
#[test]
fn from_code_point_rejects_a_non_code_point() {
    for argument in [
        "1.5",
        "0x110000",
        "-1",
        "NaN",
        "Infinity",
        "-Infinity",
        // A later invalid argument still rejects the whole call.
        "65,1.5",
    ] {
        assert_throws(
            &format!("return String.fromCodePoint({argument});"),
            ExceptionKind::RangeError,
            "invalid code point",
        );
    }
}

/// The factories carry the pinned `name`, `length`, and descriptors.
#[test]
fn the_factories_have_the_pinned_shape() {
    assert_all(&[
        // Both report arity 1 even though both are variadic.
        ("String.fromCharCode.length", "1"),
        ("String.fromCodePoint.length", "1"),
        ("String.fromCharCode.name", "fromCharCode"),
        ("String.fromCodePoint.name", "fromCodePoint"),
        (
            "Object.getOwnPropertyDescriptor(String,'fromCharCode').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(String,'fromCharCode').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(String,'fromCharCode').configurable",
            "true",
        ),
    ]);
}
