//! `Array.prototype.with`, `toReversed`, and `toSpliced`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'const a=[1,,3];\
//!     console.log(a.toReversed().join("|"), a.toReversed().length);'
//! 3||1 3
//! ```
//!
//! These are the change-by-copy methods: they answer a fresh dense Array, so a
//! hole in the receiver becomes a present `undefined` element rather than
//! staying a hole. The pinned oracle reads with `JS_TryGetPropertyInt64`,
//! which reports an absent index as `undefined` (`quickjs.c:9115-9142`).
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! [1,2,3].with(1,"x") => "1,x,3"            [1,2,3].with(-1,"x") => "1,2,x"
//! [1,2,3].with(0) => ",2,3"                 [1,,3].with(0,9) => "9,,3" dense
//! [1,2].with(1.7,9) => "1,9"                [1].with(undefined,9) => "9"
//! [1].with(NaN,8) => "8"
//! [1,2].with(5,9) !! RangeError: invalid array index: 5
//! [1,2].with(-3,9) !! RangeError: invalid array index: -1
//! [1].with(1e20,0) !! RangeError: invalid array index: 9223372036854775807
//! [1,,3].toReversed() => "3,,1", length 3, index 1 present
//! Array.prototype.toReversed.call({length:2,0:"a"}) => [,"a"], a real Array
//! [1,2,3].toSpliced(1,1) => "1,3"           [1,2,3].toSpliced() => "1,2,3"
//! [1,2,3].toSpliced(1) => "1"               [1,2,3].toSpliced(0,-5) => "1,2,3"
//! [1,2,3].toSpliced(0,2,"a","b","c") => "a,b,c,3"
//! [1,,3].toSpliced(1,1) => "1,3" dense
//! toSpliced past 2^53-1 !! TypeError: invalid array length
//! order: with reads ascending and never reads the replaced index
//!        toReversed reads descending
//! lengths: with 2, toReversed 0, toSpliced 2
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
                    Arc::from("<runtime Array by-copy>"),
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

/// `with` replaces one element in a fresh dense Array.
#[test]
fn with_replaces_one_element_in_a_fresh_array() {
    assert_all(&[
        ("[1,2,3].with(1,'x').join()", "1,x,3"),
        // A negative index counts from the end.
        ("[1,2,3].with(-1,'x').join()", "1,2,x"),
        // An absent value replaces with `undefined`.
        ("[1,2,3].with(0).join()", ",2,3"),
        // A fractional index truncates toward zero.
        ("[1,2].with(1.7,9).join()", "1,9"),
        // `undefined` and `NaN` convert to index zero.
        ("[1].with(undefined,9).join()", "9"),
        ("[1].with(NaN,8).join()", "8"),
        // The receiver is not mutated, and the result is a real Array.
        (
            "(function(){const a=[1,2];const b=a.with(0,9);return a.join()+'|'+Array.isArray(b);})()",
            "1,2|true",
        ),
        // A hole is replaced by a present `undefined`, and the replaced index
        // stays present.
        (
            "(function(){\
                const r=[1,,3].with(0,9);\
                return r.join()+'|'+Object.prototype.hasOwnProperty.call(r,1)+'|'+r.length;\
            })()",
            "9,,3|true|3",
        ),
        // An array-like receiver answers a real dense Array.
        (
            "(function(){\
                const r=Array.prototype.with.call({length:2,0:'a'},1,'x');\
                return r.join()+'|'+Array.isArray(r);\
            })()",
            "a,x|true",
        ),
    ]);
}

/// An out-of-range `with` index reports the adjusted index.
///
/// The index saturates to `i64` before the negative adjustment, so `1e20`
/// reports `9223372036854775807` (`quickjs.c:41859-41868`).
#[test]
fn with_rejects_an_out_of_range_index() {
    assert_throws(
        "return [1,2].with(5,9);",
        ExceptionKind::RangeError,
        "invalid array index: 5",
    );
    assert_throws(
        "return [1,2].with(-3,9);",
        ExceptionKind::RangeError,
        "invalid array index: -1",
    );
    assert_throws(
        "return [1].with(1e20,0);",
        ExceptionKind::RangeError,
        "invalid array index: 9223372036854775807",
    );
    assert_throws(
        "return [].with(0,0);",
        ExceptionKind::RangeError,
        "invalid array index: 0",
    );
}

/// `toReversed` answers the reversed elements in a fresh dense Array.
#[test]
fn to_reversed_answers_a_dense_reversed_copy() {
    assert_all(&[
        ("[1,2,3].toReversed().join()", "3,2,1"),
        ("[].toReversed().length", "0"),
        // A hole becomes a present `undefined`.
        (
            "(function(){\
                const r=[1,,3].toReversed();\
                return r.join()+'|'+Object.prototype.hasOwnProperty.call(r,1)+'|'+r.length;\
            })()",
            "3,,1|true|3",
        ),
        // The receiver is not mutated.
        (
            "(function(){const a=[1,2];const b=a.toReversed();return a.join()+'|'+(b!==a);})()",
            "1,2|true",
        ),
        // An array-like receiver answers a real Array.
        (
            "(function(){\
                const r=Array.prototype.toReversed.call({length:2,0:'a'});\
                return r.join()+'|'+Array.isArray(r);\
            })()",
            ",a|true",
        ),
    ]);
}

/// `toSpliced` answers the spliced elements in a fresh dense Array.
#[test]
fn to_spliced_answers_a_dense_spliced_copy() {
    assert_all(&[
        ("[1,2,3].toSpliced(1,1).join()", "1,3"),
        // No arguments copy the receiver.
        ("[1,2,3].toSpliced().join()", "1,2,3"),
        // A lone start removes everything from it.
        ("[1,2,3].toSpliced(1).join()", "1"),
        // A negative delete count removes nothing.
        ("[1,2,3].toSpliced(0,-5).join()", "1,2,3"),
        ("[1,2,3].toSpliced(0,2,'a','b','c').join()", "a,b,c,3"),
        ("[1,2,3].toSpliced(-2,1).join()", "1,3"),
        // Holes become present `undefined` elements.
        (
            "(function(){\
                const r=[1,,3].toSpliced(1,1);\
                return r.join()+'|'+Object.prototype.hasOwnProperty.call(r,1)+'|'+r.length;\
            })()",
            "1,3|true|2",
        ),
        // The receiver is not mutated, and the result is a real Array.
        (
            "(function(){\
                const a=[1,2,3];\
                const b=a.toSpliced(1,1,'x');\
                return a.join()+'|'+b.join()+'|'+Array.isArray(b);\
            })()",
            "1,2,3|1,x,3|true",
        ),
        // An array-like receiver answers a real dense Array.
        (
            "(function(){\
                const r=Array.prototype.toSpliced.call({length:3,0:'a',2:'c'},1,1,'x');\
                return r.join()+'|'+r.length+'|'+Array.isArray(r);\
            })()",
            "a,x,c|3|true",
        ),
    ]);
}

/// A `toSpliced` result past the maximum length is rejected.
#[test]
fn to_spliced_rejects_an_over_long_result() {
    assert_throws(
        "return Array.prototype.toSpliced.call({length:9007199254740991},0,0,'x');",
        ExceptionKind::TypeError,
        "invalid array length",
    );
}

/// The observable read order is fixed, and every read can enter an accessor.
#[test]
fn the_observable_read_order_matches_the_oracle() {
    assert_all(&[
        // `with` reads ascending and never reads the replaced index: the getter
        // at index 0 is not called.
        (
            "(function(){\
                let log='';\
                const o={length:2};\
                Object.defineProperty(o,0,{get(){log+='g0|';return 'a';},configurable:true});\
                Object.defineProperty(o,1,{get(){log+='g1|';return 'b';},configurable:true});\
                const r=Array.prototype.with.call(o,0,'x');\
                return log+'|'+r.join();\
            })()",
            "g1||x,b",
        ),
        // `toReversed` reads descending: the higher getter logs first.
        (
            "(function(){\
                let log='';\
                const o={length:2};\
                Object.defineProperty(o,0,{get(){log+='g0|';return 'a';},configurable:true});\
                Object.defineProperty(o,1,{get(){log+='g1|';return 'b';},configurable:true});\
                const r=Array.prototype.toReversed.call(o);\
                return log+'|'+r.join();\
            })()",
            "g1|g0||b,a",
        ),
        // `toSpliced` reads the head ascending, then the tail ascending, with
        // the removed window never read.
        (
            "(function(){\
                let log='';\
                const o={length:4};\
                for(const i of [0,1,2,3])\
                    Object.defineProperty(o,i,{get(){log+='g'+i+'|';return 'v'+i;},configurable:true});\
                const r=Array.prototype.toSpliced.call(o,1,2,'x');\
                return log+'|'+r.join();\
            })()",
            "g0|g3||v0,x,v3",
        ),
    ]);
}

/// A nullish receiver is rejected before the length is read.
#[test]
fn a_nullish_receiver_is_rejected() {
    for method in ["with", "toReversed", "toSpliced"] {
        for receiver in ["null", "undefined"] {
            assert_throws(
                &format!("return Array.prototype.{method}.call({receiver},0,1);"),
                ExceptionKind::TypeError,
                "cannot convert to object",
            );
        }
    }
}

/// The installed methods carry the pinned `name`, `length`, and descriptors.
#[test]
fn the_methods_have_the_pinned_shape() {
    assert_all(&[
        ("Array.prototype.with.length", "2"),
        ("Array.prototype.toReversed.length", "0"),
        ("Array.prototype.toSpliced.length", "2"),
        ("Array.prototype.with.name", "with"),
        ("Array.prototype.toReversed.name", "toReversed"),
        ("Array.prototype.toSpliced.name", "toSpliced"),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'with').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'toReversed').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'toSpliced').configurable",
            "true",
        ),
    ]);
}
