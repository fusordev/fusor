//! `Array.prototype.indexOf`, `lastIndexOf`, and `includes`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'console.log([NaN].indexOf(NaN),\
//!     [NaN].includes(NaN), [1,,3].indexOf(undefined), [1,,3].includes(undefined));'
//! -1 true -1 true
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! [1,2,3].indexOf(2) => 1        indexOf(9) => -1      indexOf(2,2) => -1
//! [1,2,3].indexOf(3,-1) => 2     indexOf(1,-99) => 0   indexOf(1,99) => -1
//! [1,2,1].lastIndexOf(1) => 2    lastIndexOf(1,1) => 0 lastIndexOf(1,-99) => -1
//! [1,2,3].includes(2) => true    includes(2,2) => false
//! [NaN].indexOf(NaN) => -1       [NaN].includes(NaN) => true
//! [-0].indexOf(0) => 0           [0].indexOf(-0) => 0
//! [1,,3].indexOf(undefined) => -1
//! [1,,3].includes(undefined) => true
//! [1,2,3].indexOf('2') => -1
//! Array.prototype.indexOf.call('abc','b') => 1
//! Array.prototype.indexOf.call({length:-1,0:1},1) => -1
//! two matching getters, indexOf runs only the first => "0:1"
//! length is read exactly once => 1
//! lastIndexOf visits indices in descending order => "2,1,0"
//! indexOf.length => 1   lastIndexOf.length => 1   includes.length => 1
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
                    Arc::from("<runtime Array search>"),
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

fn evaluate_with_limits<T>(
    body: &str,
    limits: ExecutionLimits,
    project: impl FnOnce(Result<JsValue, ExecutionError>) -> T,
) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, body);
    let result = context.call(&run, &[], limits);
    project(result)
}

fn evaluate<T>(body: &str, project: impl FnOnce(Result<JsValue, ExecutionError>) -> T) -> T {
    evaluate_with_limits(body, ExecutionLimits::default(), project)
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

/// The searches agree on ordinary values and on a clamped start position.
#[test]
fn the_searches_match_the_oracle_on_ordinary_values() {
    assert_all(&[
        ("[1,2,3].indexOf(2)", "1"),
        ("[1,2,3].indexOf(9)", "-1"),
        ("[1,2,3].indexOf(2,2)", "-1"),
        // A negative position counts from the end and then clamps to zero.
        ("[1,2,3].indexOf(3,-1)", "2"),
        ("[1,2,3].indexOf(1,-99)", "0"),
        ("[1,2,3].indexOf(1,99)", "-1"),
        // An absent or `undefined` position starts at the beginning.
        ("[1,2,3].indexOf(2,undefined)", "1"),
        ("[1,2,3].indexOf(2,1.9)", "1"),
        ("[].indexOf(1)", "-1"),
        ("[1,2,1].lastIndexOf(1)", "2"),
        ("[1,2,1].lastIndexOf(1,1)", "0"),
        ("[1,2,1].lastIndexOf(1,-1)", "2"),
        // A position before the start leaves nothing in range.
        ("[1,2,1].lastIndexOf(1,-99)", "-1"),
        ("[1,2,1].lastIndexOf(9)", "-1"),
        ("[].lastIndexOf(1)", "-1"),
        ("[1,2,3].includes(2)", "true"),
        ("[1,2,3].includes(9)", "false"),
        ("[1,2,3].includes(2,2)", "false"),
        ("[1,2,3].includes(3,-1)", "true"),
        ("[].includes(1)", "false"),
        // The comparison never coerces, so a String never equals a Number.
        ("[1,2,3].indexOf('2')", "-1"),
        ("['a','b'].indexOf('b')", "1"),
        ("[true].includes(true)", "true"),
    ]);
}

/// `includes` uses `SameValueZero` while the index searches use strict equality.
///
/// That is the only reason `includes` exists alongside `indexOf`: it can find a
/// `NaN`. Both treat the two signed zeros as equal.
#[test]
fn includes_finds_nan_while_index_of_cannot() {
    assert_all(&[
        ("[NaN].indexOf(NaN)", "-1"),
        ("[NaN].lastIndexOf(NaN)", "-1"),
        ("[NaN].includes(NaN)", "true"),
        ("[-0].indexOf(0)", "0"),
        ("[0].indexOf(-0)", "0"),
        ("[-0].includes(0)", "true"),
    ]);
}

/// The index searches skip a hole while `includes` reads one as `undefined`.
///
/// `indexOf` and `lastIndexOf` test `HasProperty` before reading, so a missing
/// index cannot match; `includes` reads every index in range.
#[test]
fn the_index_searches_skip_holes_while_includes_reads_them() {
    assert_all(&[
        ("[1,,3].indexOf(undefined)", "-1"),
        ("[1,,3].lastIndexOf(undefined)", "-1"),
        ("[1,,3].includes(undefined)", "true"),
        // An array-like with no index properties behaves the same way.
        (
            "Array.prototype.includes.call({length:3},undefined)",
            "true",
        ),
        ("Array.prototype.indexOf.call({length:3},undefined)", "-1"),
    ]);
}

/// Direct sparse Arrays may skip consecutive holes quickly, but a getter can
/// still install an inherited index that the following iteration must see.
#[test]
fn sparse_searches_preserve_getter_installed_inherited_indices() {
    assert_all(&[(
        "(function(){\
            const source=[1,,3];\
            Object.defineProperty(source,0,{get:function(){\
                Array.prototype[1]='inherited';return 1;\
            }});\
            return source.indexOf(3);\
        })()",
        "2",
    )]);
}

/// The legacy Test262 `lastIndexOf` coverage performs several searches through
/// a sparse 123,457-slot Array. These finite scans must complete with the CI
/// runner's instruction budget without weakening Proxy or inherited-index
/// semantics.
#[test]
fn sparse_searches_complete_with_test262_instruction_fuel() {
    evaluate_with_limits(
        "return (function(){\
            const value=[];\
            value[100]=1;\
            value[99999]='';\
            value[10]={};\
            value[5555]=5.5;\
            value[123456]='str';\
            value[5]=Infinity;\
            return [\
                value.lastIndexOf(1),value.lastIndexOf(''),\
                value.lastIndexOf('str'),value.lastIndexOf(5.5),\
                value.lastIndexOf(Infinity),value.lastIndexOf(true),\
                value.lastIndexOf(5),value.lastIndexOf('str1'),\
                value.lastIndexOf(null),value.lastIndexOf({})\
            ].join('|');\
        })();",
        ExecutionLimits::default().with_instruction_fuel(50_000_000),
        |result| {
            let result = result.expect("finite sparse search completed");
            let result = result
                .as_string()
                .expect("live value")
                .expect("String")
                .to_utf8_lossy()
                .expect("UTF-8");
            assert_eq!(result, "100|99999|123456|5555|5|-1|-1|-1|-1|-1");
        },
    );
}

/// The searches accept any array-like receiver.
#[test]
fn the_searches_accept_an_array_like_receiver() {
    assert_all(&[
        ("Array.prototype.indexOf.call('abc','b')", "1"),
        (
            "Array.prototype.indexOf.call({length:2,0:'x',1:'y'},'y')",
            "1",
        ),
        // A negative length is clamped to zero by `ToLength`.
        ("Array.prototype.indexOf.call({length:-1,0:1},1)", "-1"),
    ]);
}

/// The loop stops at the first match and reads `length` exactly once.
#[test]
fn the_loop_stops_at_the_first_match() {
    assert_all(&[
        // Two matching getters, but only the first one runs.
        (
            "(function(){\
                let n=0;\
                const a={length:3};\
                Object.defineProperty(a,0,{get(){n++;return 5;}});\
                Object.defineProperty(a,1,{get(){n++;return 5;}});\
                const r=Array.prototype.indexOf.call(a,5);\
                return r+':'+n;\
            })()",
            "0:1",
        ),
        // The length is read once, before any element.
        (
            "(function(){\
                let n=0;\
                const a={get length(){n++;return 2;},0:1,1:2};\
                Array.prototype.indexOf.call(a,2);\
                return n;\
            })()",
            "1",
        ),
        // `lastIndexOf` visits the indices in descending order.
        (
            "(function(){\
                let log='';\
                const a={length:3};\
                for(let i=0;i<3;i=i+1){\
                    Object.defineProperty(a,i,{get(){log=log+i;return i;}});\
                }\
                Array.prototype.lastIndexOf.call(a,0);\
                return log;\
            })()",
            "210",
        ),
    ]);
}

/// `indexOf` performs `HasProperty` before `Get` at each visited Proxy index.
#[test]
fn index_of_uses_proxy_has_and_get() {
    assert_all(&[(
        "(function(){\
            let log='';\
            const target={length:2,1:'x'};\
            const proxy=new Proxy(target,{\
                get:function(t,k){log+='g'+k+';';return t[k];},\
                has:function(t,k){log+='h'+k+';';return k in t;}\
            });\
            const result=Array.prototype.indexOf.call(proxy,'x');\
            return log+'|'+result;\
        })()",
        "glength;h0;h1;g1;|1",
    )]);
}

/// A nullish receiver is rejected before the length is read.
#[test]
fn a_nullish_receiver_is_rejected() {
    for method in ["indexOf", "lastIndexOf", "includes"] {
        for receiver in ["null", "undefined"] {
            assert_throws(
                &format!("return Array.prototype.{method}.call({receiver}, 1);"),
                ExceptionKind::TypeError,
                "cannot convert to object",
            );
        }
    }
}

/// The installed searches carry the pinned `name`, `length`, and descriptors.
#[test]
fn the_searches_have_the_pinned_shape() {
    assert_all(&[
        ("Array.prototype.indexOf.length", "1"),
        ("Array.prototype.lastIndexOf.length", "1"),
        ("Array.prototype.includes.length", "1"),
        ("Array.prototype.indexOf.name", "indexOf"),
        ("Array.prototype.lastIndexOf.name", "lastIndexOf"),
        ("Array.prototype.includes.name", "includes"),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'indexOf').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'indexOf').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'indexOf').configurable",
            "true",
        ),
    ]);
}
