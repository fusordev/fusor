//! `Array.prototype.slice`, `concat`, and `at`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'const o={length:2,0:"a"};\
//!     const r=[1].concat(o); console.log(r.length, r[1]===o);'
//! 2 true
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! [1,2,3].slice(1) => "2,3"      slice(1,2) => "2"     slice(-2) => "2,3"
//! [1,2,3].slice(0,-1) => "1,2"   slice(2,1).length => 0
//! [1,,3].slice(0) => length 3, index 1 absent
//! Array.prototype.slice.call("abc",1) => "b,c"
//! [1,2,3].at(-1) => 3            at(3) => undefined    at(-4) => undefined
//! [1,2,3].at() => 1              at(1.9) => 2          [1,,3].at(1) => undefined
//! [1,2].concat([3,4]) => "1,2,3,4"
//! [1].concat([[2]]) => length 2, index 1 is an Array
//! [1].concat({length:2,0:"a"}) => length 2, index 1 is the object itself
//! Array.prototype.concat.call({length:2,0:"a"},9) => Array, length 2
//! slice reads length before any element: "len|g0|g1"
//! lengths: slice 2, concat 1, at 1
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
                    Arc::from("<runtime Array copiers>"),
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

/// `slice` copies a resolved range into a fresh Array.
#[test]
fn slice_copies_a_resolved_range() {
    assert_all(&[
        ("[1,2,3].slice(1).join()", "2,3"),
        ("[1,2,3].slice(1,2).join()", "2"),
        // Negative endpoints count from the end.
        ("[1,2,3].slice(-2).join()", "2,3"),
        ("[1,2,3].slice(0,-1).join()", "1,2"),
        // Crossed endpoints yield an empty Array rather than swapping.
        ("[1,2,3].slice(2,1).length", "0"),
        ("[1,2,3].slice().join()", "1,2,3"),
        ("[1,2,3].slice(-99,99).join()", "1,2,3"),
        // An absent or `undefined` end runs to the length.
        ("[1,2,3].slice(1,undefined).join()", "2,3"),
        // The result is a real Array, not an array-like.
        ("Array.isArray([1].slice())", "true"),
    ]);
}

/// `at` answers a single element and accepts a negative index.
#[test]
fn at_answers_one_element() {
    assert_all(&[
        ("[1,2,3].at(0)", "1"),
        ("[1,2,3].at(-1)", "3"),
        ("[1,2,3].at(1.9)", "2"),
        // An absent index is `0`.
        ("[1,2,3].at()", "1"),
        // Out of range answers `undefined` rather than throwing.
        ("String([1,2,3].at(3))", "undefined"),
        ("String([1,2,3].at(-4))", "undefined"),
        ("String([].at(0))", "undefined"),
        // A hole reads as `undefined`.
        ("String([1,,3].at(1))", "undefined"),
    ]);
}

/// `concat` spreads only a real Array.
///
/// An array-like becomes a single element, and nesting is not flattened, so the
/// two cases below differ even though both arguments are objects.
#[test]
fn concat_spreads_only_a_real_array() {
    assert_all(&[
        ("[1,2].concat([3,4]).join()", "1,2,3,4"),
        ("[1,2].concat().join()", "1,2"),
        ("Array.isArray([1].concat())", "true"),
        // A nested Array is appended whole, not flattened.
        (
            "(function(){const r=[1].concat([[2]]);return r.length+'|'+Array.isArray(r[1]);})()",
            "2|true",
        ),
        (
            "(function(){const r=[1].concat(2,[3,[4]]);return r.length+'|'+r[3].length;})()",
            "4|1",
        ),
        // An array-like is one element, and it is the same object.
        (
            "(function(){\
                const o={length:2,0:'a'};\
                const r=[1].concat(o);\
                return r.length+'|'+(r[1]===o);\
            })()",
            "2|true",
        ),
        // An array-like receiver is spread, because the receiver is always
        // treated as the first source.
        (
            "(function(){\
                const r=Array.prototype.concat.call({length:2,0:'a'},9);\
                return Array.isArray(r)+'|'+r.length+'|'+(typeof r[0]);\
            })()",
            "true|2|object",
        ),
    ]);
}

/// Holes survive into the copied result.
///
/// An absent source index is skipped rather than written, so the destination
/// keeps a hole and still counts it toward the length.
#[test]
fn holes_survive_into_the_result() {
    assert_all(&[
        (
            "(function(){\
                const r=[1,,3].slice(0);\
                return r.length+'|'+Object.prototype.hasOwnProperty.call(r,1);\
            })()",
            "3|false",
        ),
        (
            "(function(){\
                const r=[1,,3].concat([4]);\
                return r.length+'|'+Object.prototype.hasOwnProperty.call(r,1);\
            })()",
            "4|false",
        ),
    ]);
}

/// The copiers accept any array-like or primitive-String receiver.
#[test]
fn the_copiers_accept_an_array_like_receiver() {
    assert_all(&[
        (
            "(function(){\
                const r=Array.prototype.slice.call({length:2,0:'a',1:'b'});\
                return Array.isArray(r)+'|'+r.join();\
            })()",
            "true|a,b",
        ),
        // A primitive String exposes its indices, so it slices by character.
        ("Array.prototype.slice.call('abc',1).join()", "b,c"),
    ]);
}

/// The length is read once, before any element.
#[test]
fn the_length_is_read_before_any_element() {
    assert_all(&[(
        "(function(){\
            let log='';\
            const o={\
                get length(){log+='len|';return 2;},\
                get 0(){log+='g0|';return 1;},\
                get 1(){log+='g1';return 2;}\
            };\
            Array.prototype.slice.call(o);\
            return log;\
        })()",
        "len|g0|g1",
    )]);
}

/// A nullish receiver is rejected before the length is read.
#[test]
fn a_nullish_receiver_is_rejected_by_the_copiers() {
    for method in ["slice", "concat", "at"] {
        for receiver in ["null", "undefined"] {
            assert_throws(
                &format!("return Array.prototype.{method}.call({receiver});"),
                ExceptionKind::TypeError,
                "cannot convert to object",
            );
        }
    }
}

/// The installed copiers carry the pinned `name`, `length`, and descriptors.
#[test]
fn the_copiers_have_the_pinned_shape() {
    assert_all(&[
        // Only `slice` reports arity 2.
        ("Array.prototype.slice.length", "2"),
        ("Array.prototype.concat.length", "1"),
        ("Array.prototype.at.length", "1"),
        ("Array.prototype.slice.name", "slice"),
        ("Array.prototype.concat.name", "concat"),
        ("Array.prototype.at.name", "at"),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'slice').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'slice').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'slice').configurable",
            "true",
        ),
    ]);
}
