//! `Array.prototype.reduce` and `reduceRight`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'console.log([1,2,3].reduce((a,v)=>a+v),\
//!     [1,2,3].reduceRight((a,v)=>a+"-"+v), [1,2].reduce((a,v)=>a+v, undefined));'
//! 6 3-2-1 NaN
//! ```
//!
//! These differ from the other callback methods in two ways. The callback takes
//! four arguments, with the accumulator first, and its result becomes the next
//! accumulator. And an absent initial value is distinct from an explicit
//! `undefined` one: the former seeds from the first *present* element, so an
//! empty or all-holes array reports `TypeError: empty array`, while the latter is
//! simply the accumulator.

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
                    Arc::from("<runtime Array reductions>"),
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

/// `reduce` folds left to right, seeding from the first element when no initial
/// value is given.
///
/// An explicit `undefined` initial value is *not* absent: it becomes the
/// accumulator, so `[1,2].reduce((a,v)=>a+v, undefined)` is `NaN` rather than
/// `3`.
#[test]
fn reduce_folds_left_to_right() {
    assert_all(&[
        ("[1,2,3].reduce(function(a,v){return a+v;})", "6"),
        ("[1,2,3].reduce(function(a,v){return a+v;},10)", "16"),
        ("[7].reduce(function(a,v){return a+v;})", "7"),
        ("[].reduce(function(a,v){return a+v;},5)", "5"),
        (
            "(function(){let out='';[1,2].reduce(function(a,v,i,arr){out+=a+'/'+v+'/'+i+'/'+arr.length+';';return v;});return out;})()",
            "1/2/1/2;",
        ),
        (
            "String([1].reduce(function(a){return a;},undefined))",
            "undefined",
        ),
        ("[1,2].reduce(function(a,v){return a+v;},undefined)", "NaN"),
    ]);
}

/// `reduceRight` visits the indices in descending order.
#[test]
fn reduce_right_folds_in_reverse() {
    assert_all(&[
        (
            "[1,2,3].reduceRight(function(a,v){return a+'-'+v;})",
            "3-2-1",
        ),
        (
            "[1,2].reduceRight(function(a,v){return a+'-'+v;},'z')",
            "z-2-1",
        ),
        (
            "(function(){let out='';[1,2,3].reduceRight(function(a,v){out+=v;return a;});return out;})()",
            "21",
        ),
    ]);
}

/// A hole is skipped: the callback never sees it, and it seeds the accumulator
/// only when it is present.
#[test]
fn holes_are_skipped_by_the_reductions() {
    assert_all(&[
        (
            "(function(){let n=0;[1,,3].reduce(function(a,v){n++;return a;});return n;})()",
            "1",
        ),
        ("[,2,3].reduce(function(a,v){return a+v;})", "5"),
    ]);
}

/// The reductions accept any array-like receiver and read its length once.
#[test]
fn the_reductions_accept_an_array_like_receiver() {
    assert_all(&[
        (
            "Array.prototype.reduce.call({length:2,0:1,1:2},function(a,v){return a+v;})",
            "3",
        ),
        (
            "(function(){let n=0;const o={get length(){n++;return 2;},0:1,1:2};Array.prototype.reduce.call(o,function(a,v){return a+v;});return n;})()",
            "1",
        ),
    ]);
}

/// Both reductions report arity 1 with the pinned descriptors.
#[test]
fn the_reductions_have_the_pinned_shape() {
    assert_all(&[
        ("Array.prototype.reduce.length", "1"),
        ("Array.prototype.reduceRight.length", "1"),
        ("Array.prototype.reduce.name", "reduce"),
        ("Array.prototype.reduceRight.name", "reduceRight"),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'reduce').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'reduce').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'reduce').configurable",
            "true",
        ),
    ]);
}

/// An empty fold, a bad callback, and a nullish receiver are all rejected.
///
/// `TypeError: empty array` is reported only when there is no initial value and
/// no present element to seed from.
#[test]
fn an_unseedable_fold_or_bad_argument_is_rejected() {
    assert_throws(
        "return [].reduce(function(a,v){return a+v;});",
        ExceptionKind::TypeError,
        "empty array",
    );
    assert_throws(
        "return [].reduceRight(function(a,v){return a+v;});",
        ExceptionKind::TypeError,
        "empty array",
    );
    assert_throws(
        "return [,,].reduce(function(a){return a;});",
        ExceptionKind::TypeError,
        "empty array",
    );
    assert_throws(
        "return [1].reduce(1);",
        ExceptionKind::TypeError,
        "not a function",
    );
    assert_throws(
        "return [1].reduce();",
        ExceptionKind::TypeError,
        "not a function",
    );
    assert_throws(
        "return Array.prototype.reduce.call(null,function(){});",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
}
