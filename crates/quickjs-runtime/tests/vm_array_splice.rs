//! `Array.prototype.splice`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'const a=[1,2,3];\
//!     const r=a.splice(1,1,8,9); console.log(r.join(), a.join());'
//! 2 1,8,9,3
//! ```
//!
//! `splice` is both a copier and a mutator: it collects every removed element
//! into a fresh Array before anything shifts, so a getter cannot observe a
//! half-shifted array. The tail then moves by `insertions - removed`, walked from
//! whichever end keeps a source from being overwritten before it is read.

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
                    Arc::from("<runtime Array splice>"),
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

/// `splice` returns the removed elements and closes the gap.
///
/// An absent `deleteCount` removes everything from `start`, while an absent
/// `start` removes nothing at all. A negative `start` counts from the end and a
/// non-positive count removes nothing.
#[test]
fn splice_removes_and_returns_the_removed_elements() {
    assert_all(&[
        (
            "(function(){const a=[1,2,3];const r=a.splice(1,1);return r.join()+'|'+a.join();})()",
            "2|1,3",
        ),
        (
            "(function(){const a=[1,2,3];const r=a.splice(1);return r.join()+'|'+a.join();})()",
            "2,3|1",
        ),
        (
            "(function(){const a=[1,2,3];const r=a.splice();return r.length+'|'+a.join();})()",
            "0|1,2,3",
        ),
        (
            "(function(){const a=[1,2,3];const r=a.splice(-1,1);return r.join()+'|'+a.join();})()",
            "3|1,2",
        ),
        (
            "(function(){const a=[1,2,3];const r=a.splice(0,99);return r.join()+'|'+a.join()+'|'+a.length;})()",
            "1,2,3||0",
        ),
        (
            "(function(){const a=[1,2,3];const r=a.splice(1,-1);return r.length+'|'+a.join();})()",
            "0|1,2,3",
        ),
        (
            "(function(){const a=[1,2,3];const r=a.splice(99,1);return r.length+'|'+a.join();})()",
            "0|1,2,3",
        ),
        ("Array.isArray([1].splice(0,1))", "true"),
    ]);
}

/// Insertions land in the gap, growing or shrinking the array as needed.
///
/// The tail is shifted in the direction that keeps a source from being
/// overwritten before it is read: growing walks it from the end, shrinking from
/// the front.
#[test]
fn splice_inserts_and_resizes() {
    assert_all(&[
        (
            "(function(){const a=[1,2,3];const r=a.splice(1,0,9);return r.length+'|'+a.join();})()",
            "0|1,9,2,3",
        ),
        (
            "(function(){const a=[1,2,3];const r=a.splice(1,1,8,9);return r.join()+'|'+a.join();})()",
            "2|1,8,9,3",
        ),
        (
            "(function(){const a=[1,2];const r=a.splice(1,0,7,8,9);return r.length+'|'+a.join()+'|'+a.length;})()",
            "0|1,7,8,9,2|5",
        ),
    ]);
}

/// Holes survive into both the result and the shifted remainder.
#[test]
fn splice_preserves_holes() {
    assert_all(&[
        (
            "(function(){const a=[1,,3];const r=a.splice(0,2);return r.length+'|'+Object.prototype.hasOwnProperty.call(r,1);})()",
            "2|false",
        ),
        (
            "(function(){const a=[1,,3,4];a.splice(0,1);return a.join()+'|'+Object.prototype.hasOwnProperty.call(a,0);})()",
            ",3,4|false",
        ),
    ]);
}

/// `splice` accepts any array-like receiver and writes its length back.
#[test]
fn splice_accepts_an_array_like_receiver() {
    assert_all(&[(
        "(function(){const o={length:3,0:'a',1:'b',2:'c'};const r=Array.prototype.splice.call(o,1,1);return r.join()+'|'+o.length+'|'+o[1];})()",
        "b|2|c",
    )]);
}

/// `splice` reports arity 2 with the pinned descriptors.
#[test]
fn splice_has_the_pinned_shape() {
    assert_all(&[
        ("Array.prototype.splice.length", "2"),
        ("Array.prototype.splice.name", "splice"),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'splice').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'splice').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'splice').configurable",
            "true",
        ),
    ]);
}

/// A nullish receiver is rejected before the length is read.
#[test]
fn a_nullish_splice_receiver_is_rejected() {
    assert_throws(
        "return Array.prototype.splice.call(null,0);",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
    assert_throws(
        "return Array.prototype.splice.call(undefined,0);",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
}
