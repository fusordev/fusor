//! `Object.assign`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'const log = [];\
//!     const source = {get a(){log.push("ga"); return 1}, get b(){log.push("gb"); return 2}};\
//!     const target = {set a(v){log.push("sa")}, set b(v){log.push("sb")}};\
//!     Object.assign(target, source); console.log(log.join(""));'
//! gasagbsb
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! Object.assign(o,{a:1},{b:2}) returns o with both copied; a later source wins
//! a nullish source is skipped; a nullish target !! cannot convert to object
//! Object.assign({}, "ab") => {0:"a", 1:"b"}     Object.assign({}, 1) => {}
//! Object.assign(1,{a:2}) returns a Number wrapper carrying a
//! symbol keys are copied; non-enumerable keys are not
//! reads and writes interleave per key: ga, sa, gb, sb
//! Object.assign(Object.freeze({}),{a:1}) !! TypeError: object is not extensible
//! a read-only target property !! TypeError: 'a' is read-only, after the getter ran
//! Object.assign(Object.freeze({})) and (…, null) both succeed, returning it
//! Object.assign([1,2,3],{length:1}) truncates; {length:-1} !! invalid array length
//! lengths: assign 2
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
                    Arc::from("<runtime Object assign>"),
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

/// `Object.assign` copies each source's own enumerable properties onto the
/// target and returns it.
#[test]
fn object_assign_copies_every_source_onto_the_target() {
    assert_all(&[
        // The target itself is returned, not a copy.
        (
            "(function(){\
                const target={};\
                const answer=Object.assign(target,{a:1},{b:2});\
                return (answer===target)+'|'+target.a+','+target.b;\
            })()",
            "true|1,2",
        ),
        // Sources apply in argument order, so a later one wins.
        ("Object.assign({},{a:1},{a:2}).a", "2"),
        // A nullish source contributes nothing and does not throw.
        (
            "(function(){\
                const o=Object.assign({a:1},null,undefined,{b:2});\
                return o.a+','+o.b;\
            })()",
            "1,2",
        ),
        // With no source at all the target is simply returned.
        (
            "(function(){\
                const target={a:1};\
                return (Object.assign(target)===target)+'|'+target.a;\
            })()",
            "true|1",
        ),
        // A primitive source is read through the properties its wrapper would
        // expose, so a `String` contributes its characters and a Number none.
        (
            "(function(){\
                const o=Object.assign({},'ab');\
                return o[0]+','+o[1]+'|'+String(o.length);\
            })()",
            "a,b|undefined",
        ),
        ("Reflect.ownKeys(Object.assign({},1)).length", "0"),
        // A symbol key is copied, unlike in `Object.keys`' projection.
        (
            "(function(){\
                const s=Symbol('q');\
                const source={};\
                source[s]=1;\
                return Object.assign({},source)[s];\
            })()",
            "1",
        ),
        // A non-enumerable own property is skipped.
        (
            "(function(){\
                const source={};\
                Object.defineProperty(source,'h',{value:1});\
                source.v=2;\
                const o=Object.assign({},source);\
                return String(o.h)+'|'+o.v;\
            })()",
            "undefined|2",
        ),
        // An array source contributes its indices; a hole is absent.
        (
            "(function(){\
                const o=Object.assign({},[1,,3]);\
                return o[0]+','+String(o[1])+','+o[2];\
            })()",
            "1,undefined,3",
        ),
        // A primitive target is boxed and the wrapper is the result.
        (
            "(function(){\
                const answer=Object.assign(1,{a:2});\
                return (typeof answer)+'|'+answer.a+'|'+(answer instanceof Number);\
            })()",
            "object|2|true",
        ),
    ]);
    assert_throws(
        "return Object.assign(null,{});",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
    assert_throws(
        "return Object.assign();",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
}

/// A source read and a target write interleave per key.
#[test]
fn each_source_read_pairs_with_its_target_write() {
    assert_all(&[
        // The pinned order is per key, not per source: `get a`, `set a`, then
        // `get b`, `set b`.
        (
            "(function(){\
                let log='';\
                const source={get a(){log+='ga';return 1;},get b(){log+='gb';return 2;}};\
                const target={set a(v){log+='sa';},set b(v){log+='sb';}};\
                Object.assign(target,source);\
                return log;\
            })()",
            "gasagbsb",
        ),
        // Sources are walked in argument order.
        (
            "(function(){\
                let log='';\
                const first={get a(){log+='1';return 1;}};\
                const second={get b(){log+='2';return 2;}};\
                Object.assign({},first,second);\
                return log;\
            })()",
            "12",
        ),
        // A source getter runs exactly once per key.
        (
            "(function(){\
                let log='';\
                const source={get a(){log+='g';return 1;}};\
                const o=Object.assign({},source);\
                return log+'|'+o.a;\
            })()",
            "g|1",
        ),
        // A target setter receives the value and the target as its `this`.
        (
            "(function(){\
                let log='';\
                const target={set a(v){log+='s'+v+':'+(this===target);}};\
                Object.assign(target,{a:5});\
                return log;\
            })()",
            "s5:true",
        ),
        // The enumerable attribute is re-tested against the live source, so a
        // getter that deletes or hides a later key removes it from the copy,
        // while a key it adds is never visited.
        (
            "(function(){\
                const source={get a(){delete source.b;return 1;},b:2};\
                const o=Object.assign({},source);\
                return o.a+'|'+String(o.b);\
            })()",
            "1|undefined",
        ),
        (
            "(function(){\
                const source={get a(){Object.defineProperty(source,'b',{enumerable:false});return 1;},b:2};\
                const o=Object.assign({},source);\
                return o.a+'|'+String(o.b);\
            })()",
            "1|undefined",
        ),
        (
            "(function(){\
                const source={get a(){source.z=9;return 1;}};\
                const o=Object.assign({},source);\
                return o.a+'|'+String(o.z);\
            })()",
            "1|undefined",
        ),
    ]);
}

/// Each write is a strict `Set`, so a refusal throws rather than being dropped.
#[test]
fn a_refused_write_throws_rather_than_being_dropped() {
    // A non-extensible target cannot gain a property.
    assert_throws(
        "return Object.assign(Object.freeze({}),{a:1});",
        ExceptionKind::TypeError,
        "object is not extensible",
    );
    // A read-only target property refuses, and the source getter has already
    // run by then.
    assert_throws(
        "(function(){\
            const target={};\
            Object.defineProperty(target,'a',{value:1,writable:false});\
            Object.assign(target,{a:2});\
        })()",
        ExceptionKind::TypeError,
        "'a' is read-only",
    );
    // An array target's `length` keeps its resumable conversion, so an
    // out-of-domain length still reports a `RangeError`.
    assert_throws(
        "return Object.assign([1,2,3],{length:-1});",
        ExceptionKind::RangeError,
        "invalid array length",
    );
    assert_all(&[
        // A frozen target with nothing to copy still succeeds, because the
        // refusal belongs to the write rather than to the conversion.
        (
            "(function(){\
                const target=Object.freeze({});\
                return Object.assign(target)===target;\
            })()",
            "true",
        ),
        (
            "(function(){\
                const target=Object.freeze({});\
                return Object.assign(target,null)===target;\
            })()",
            "true",
        ),
        // A converted array `length` truncates, and the walk continues into the
        // next source afterwards.
        (
            "(function(){\
                const a=[1,2,3];\
                Object.assign(a,{length:1});\
                return a.length;\
            })()",
            "1",
        ),
        (
            "(function(){\
                const a=[1,2,3];\
                Object.assign(a,{length:{valueOf(){return 1;}}});\
                return a.length;\
            })()",
            "1",
        ),
        (
            "(function(){\
                const a=[1,2,3];\
                Object.assign(a,{length:1},{z:9});\
                return a.length+'|'+a.z;\
            })()",
            "1|9",
        ),
    ]);
}

/// `Object.assign` carries the pinned `name` and `length`.
#[test]
fn object_assign_carries_the_pinned_identity() {
    assert_all(&[
        ("Object.assign.length", "2"),
        ("Object.assign.name", "assign"),
        (
            "Object.getOwnPropertyDescriptor(Object,'assign').enumerable",
            "false",
        ),
    ]);
}
