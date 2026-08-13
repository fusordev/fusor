//! `Number.prototype.toFixed`, `toExponential`, and `toPrecision`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'console.log((1.005).toFixed(2),\
//!     (1.55).toFixed(1), (8.575).toFixed(2), (123.456).toPrecision(4));'
//! 1.00 1.6 8.57 123.5
//! ```
//!
//! The rounding is the interesting part: all three round the *exact* value the
//! binary64 holds, not its shortest decimal spelling. `(1.005).toFixed(2)` is
//! `"1.00"` because the stored value is just below 1.005, while
//! `(1.55).toFixed(1)` is `"1.6"` because that one is just above. An
//! implementation that formatted the shortest spelling would round both up.

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
                    Arc::from("<runtime Number formats>"),
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

/// Every rendering matches the pinned oracle exactly.
///
/// The table is a direct transcript of the oracle run, kept in one test so the
/// exact-rounding cases sit beside the ordinary ones they contradict.
#[test]
fn the_renderings_match_the_oracle() {
    assert_all(&[
        ("(1.005).toFixed(2)", "1.00"),
        ("(1.5).toFixed(0)", "2"),
        ("(2.5).toFixed(0)", "3"),
        ("(-1.5).toFixed(0)", "-2"),
        ("(0.5).toFixed(0)", "1"),
        ("(1.45).toFixed(1)", "1.4"),
        ("(1.55).toFixed(1)", "1.6"),
        ("(0).toFixed(2)", "0.00"),
        ("(-0).toFixed(2)", "0.00"),
        ("(1.25).toFixed(1)", "1.3"),
        ("(1.35).toFixed(1)", "1.4"),
        ("(8.575).toFixed(2)", "8.57"),
        ("(123.456).toFixed(0)", "123"),
        ("(1e-7).toFixed(2)", "0.00"),
        ("(0.000001).toFixed(2)", "0.00"),
        ("(1.999).toFixed(2)", "2.00"),
        ("(9.995).toFixed(2)", "9.99"),
        ("(-9.995).toFixed(2)", "-9.99"),
        ("(1e21).toFixed(2)", "1e+21"),
        ("(NaN).toFixed(2)", "NaN"),
        ("(Infinity).toFixed(2)", "Infinity"),
        ("(-Infinity).toFixed(2)", "-Infinity"),
        ("(1).toFixed(0)", "1"),
        ("(123.456).toFixed()", "123"),
        ("(1).toFixed(undefined)", "1"),
        ("Number.prototype.toFixed.length", "1"),
        ("Number.prototype.toFixed.call(new Number(1.5),0)", "2"),
        ("(123.456).toExponential(2)", "1.23e+2"),
        ("(1).toExponential(0)", "1e+0"),
        ("(1).toExponential(2)", "1.00e+0"),
        ("(0).toExponential(2)", "0.00e+0"),
        ("(0).toExponential(0)", "0e+0"),
        ("(-1.5).toExponential(1)", "-1.5e+0"),
        ("(1e21).toExponential(2)", "1.00e+21"),
        ("(1.005).toExponential(2)", "1.00e+0"),
        ("(NaN).toExponential(2)", "NaN"),
        ("(Infinity).toExponential(2)", "Infinity"),
        ("(1e-7).toExponential(3)", "1.000e-7"),
        ("(12345).toExponential(2)", "1.23e+4"),
        ("(0.0001).toExponential(2)", "1.00e-4"),
        ("(1.5).toExponential(0)", "2e+0"),
        ("(2.5).toExponential(0)", "3e+0"),
        ("(123.456).toExponential()", "1.23456e+2"),
        ("(NaN).toExponential(101)", "NaN"),
        ("Number.prototype.toExponential.length", "1"),
        ("(123.456).toPrecision(4)", "123.5"),
        ("(123.456).toPrecision(2)", "1.2e+2"),
        ("(1).toPrecision(1)", "1"),
        ("(0).toPrecision(1)", "0"),
        ("(0.000001).toPrecision(2)", "0.0000010"),
        ("(1e21).toPrecision(3)", "1.00e+21"),
        ("(NaN).toPrecision(2)", "NaN"),
        ("(Infinity).toPrecision(2)", "Infinity"),
        ("(1.5).toPrecision(1)", "2"),
        ("(2.5).toPrecision(1)", "3"),
        ("(-1.5).toPrecision(1)", "-2"),
        ("(12345).toPrecision(2)", "1.2e+4"),
        ("(0.00001).toPrecision(3)", "0.0000100"),
        ("(10).toPrecision(1)", "1e+1"),
        ("(11).toPrecision(1)", "1e+1"),
        ("(17).toPrecision(1)", "2e+1"),
        ("(100).toPrecision(2)", "1.0e+2"),
        ("(1.2345e27).toPrecision(21)", "1.23449999999999996184e+27"),
        ("(1e21).toPrecision(21)", "1.00000000000000000000e+21"),
        ("(1e-21).toPrecision(1)", "1e-21"),
        ("(1e-21).toPrecision(16)", "9.999999999999999e-22"),
        ("(1e-21).toPrecision(21)", "9.99999999999999907537e-22"),
        ("(123.456).toPrecision()", "123.456"),
        ("(1).toPrecision(undefined)", "1"),
        ("(NaN).toPrecision(0)", "NaN"),
        ("(NaN).toPrecision(101)", "NaN"),
        ("Number.prototype.toPrecision.length", "1"),
        ("Number.prototype.toFixed.name", "toFixed"),
        (
            "Object.getOwnPropertyDescriptor(Number.prototype,'toFixed').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Number.prototype,'toFixed').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Number.prototype,'toFixed').configurable",
            "true",
        ),
    ]);
}

/// An out-of-range digit count and a non-Number receiver are rejected.
///
/// Only `toFixed` validates its count before short-circuiting a non-finite
/// value, which is why `(NaN).toFixed(101)` throws while
/// `(NaN).toExponential(101)` and `(NaN).toPrecision(101)` are both `"NaN"`.
#[test]
fn out_of_range_counts_and_bad_receivers_are_rejected() {
    assert_throws(
        "return (1).toFixed(101);",
        ExceptionKind::RangeError,
        "invalid number of digits",
    );
    assert_throws(
        "return (1).toFixed(-1);",
        ExceptionKind::RangeError,
        "invalid number of digits",
    );
    assert_throws(
        "return (NaN).toFixed(101);",
        ExceptionKind::RangeError,
        "invalid number of digits",
    );
    assert_throws(
        "return (1).toExponential(101);",
        ExceptionKind::RangeError,
        "invalid number of digits",
    );
    assert_throws(
        "return (1).toExponential(-1);",
        ExceptionKind::RangeError,
        "invalid number of digits",
    );
    assert_throws(
        "return (1).toPrecision(0);",
        ExceptionKind::RangeError,
        "invalid number of digits",
    );
    assert_throws(
        "return (1).toPrecision(101);",
        ExceptionKind::RangeError,
        "invalid number of digits",
    );
    assert_throws(
        "return Number.prototype.toFixed.call('1',2);",
        ExceptionKind::TypeError,
        "not a number",
    );
}
