//! `Object.is`, `Object.hasOwn`, and `Object.getOwnPropertySymbols`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'console.log(Object.is(NaN, NaN),\
//!     Object.is(0, -0), Object.hasOwn({a:1}, "a"), Object.hasOwn({}, "toString"));'
//! true false true false
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! Object.is(NaN, NaN) => true      Object.is(0, -0) => false
//! Object.is(-0, -0) => true        Object.is(1n, 1) => false
//! Object.is() => true (both absent arguments are undefined)
//! Object.is(1) => false            Object.is(undefined) => true
//! Object.hasOwn({a:1}, "a") => true         Object.hasOwn({}, "toString") => false
//! Object.hasOwn([1], "length") => true      Object.hasOwn("ab", "0") => true
//! Object.hasOwn(1, "a") => false
//! Object.hasOwn(null, "a") !! TypeError: cannot convert to object
//! Object.getOwnPropertySymbols({}) => []    on {a:1, [Symbol("q")]:2} => [Symbol(q)]
//! Object.getOwnPropertySymbols(1) => []     on [1] => []
//! Object.getOwnPropertySymbols(null) !! TypeError: cannot convert to object
//! lengths: is 2, hasOwn 2, getOwnPropertySymbols 1
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
                    Arc::from("<runtime Object statics>"),
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

/// `Object.is` is `SameValue`, not strict equality.
#[test]
fn object_is_applies_same_value() {
    assert_all(&[
        ("Object.is(1,1)", "true"),
        ("Object.is(1,2)", "false"),
        // The two places `SameValue` differs from `===`.
        ("Object.is(NaN,NaN)", "true"),
        ("Object.is(0,-0)", "false"),
        ("Object.is(-0,0)", "false"),
        ("Object.is(-0,-0)", "true"),
        ("Object.is(0,0)", "true"),
        ("Object.is('a','a')", "true"),
        ("Object.is('a','b')", "false"),
        // Objects compare by identity, never structurally.
        ("Object.is({},{})", "false"),
        ("(function(){const o={};return Object.is(o,o);})()", "true"),
        (
            "(function(){\
                function f(){}\
                return Object.is(f,f);\
            })()",
            "true",
        ),
        // Absent arguments are `undefined`, so a no-argument call is `true`.
        ("Object.is()", "true"),
        ("Object.is(undefined)", "true"),
        ("Object.is(undefined,undefined)", "true"),
        ("Object.is(null,null)", "true"),
        ("Object.is(null,undefined)", "false"),
        ("Object.is(1)", "false"),
        // A symbol compares by identity, and two symbols sharing a description
        // are distinct.
        ("Object.is(Symbol.iterator,Symbol.iterator)", "true"),
        ("Object.is(Symbol('q'),Symbol('q'))", "false"),
        // A BigInt compares by mathematical value, and never equals a Number.
        ("Object.is(1n,1n)", "true"),
        ("Object.is(1n,1)", "false"),
        ("Object.is(true,true)", "true"),
        ("Object.is(true,1)", "false"),
        // Nothing converts, so a would-be `valueOf` never runs.
        (
            "(function(){\
                let log='';\
                const o={valueOf(){log+='v';return 1;}};\
                const answer=Object.is(o,1);\
                return log+'|'+answer;\
            })()",
            "|false",
        ),
    ]);
}

/// `Object.hasOwn` is `hasOwnProperty` with the target as its first argument.
#[test]
fn object_has_own_moves_the_receiver_into_an_argument() {
    assert_all(&[
        ("Object.hasOwn({a:1},'a')", "true"),
        ("Object.hasOwn({a:1},'b')", "false"),
        // An inherited property is not an own one.
        ("Object.hasOwn({},'toString')", "false"),
        ("Object.hasOwn(Object.create({a:1}),'a')", "false"),
        // A non-enumerable own property still counts.
        (
            "(function(){\
                const o={};\
                Object.defineProperty(o,'h',{value:1});\
                return Object.hasOwn(o,'h');\
            })()",
            "true",
        ),
        ("Object.hasOwn([1],'0')", "true"),
        // A hole is absent, unlike an explicit `undefined`.
        ("Object.hasOwn([1,,3],'1')", "false"),
        ("Object.hasOwn([1],'length')", "true"),
        // A primitive string exposes its exotic indices and `length`.
        ("Object.hasOwn('ab','0')", "true"),
        ("Object.hasOwn('ab','5')", "false"),
        ("Object.hasOwn('ab','length')", "true"),
        // Another primitive has no own properties at all, but does not throw.
        ("Object.hasOwn(1,'a')", "false"),
        ("Object.hasOwn(true,'a')", "false"),
        // The key converts with `ToPropertyKey`, which can run a `toString`.
        (
            "(function(){\
                let log='';\
                const key={toString(){log+='k';return 'a';}};\
                const answer=Object.hasOwn({a:1},key);\
                return log+'|'+answer;\
            })()",
            "k|true",
        ),
        // An absent key is the string `\"undefined\"`.
        ("Object.hasOwn({undefined:1})", "true"),
        // A symbol key resolves the same way a string one does.
        (
            "(function(){\
                const s=Symbol('q');\
                const o={};\
                o[s]=1;\
                return Object.hasOwn(o,s);\
            })()",
            "true",
        ),
    ]);
    // A nullish target reports the `ToObject` failure, as `hasOwnProperty` does.
    assert_throws(
        "return Object.hasOwn(null,'a');",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
    assert_throws(
        "return Object.hasOwn(undefined,'a');",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
    assert_throws(
        "return Object.hasOwn();",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
}

/// `Object.getOwnPropertySymbols` reports the symbol phase alone.
#[test]
fn object_get_own_property_symbols_reports_only_symbols() {
    assert_all(&[
        ("Object.getOwnPropertySymbols({}).length", "0"),
        // A string-keyed property is never reported, and an index key is not
        // either, which is what separates this from `getOwnPropertyNames`.
        ("Object.getOwnPropertySymbols({a:1,0:2}).length", "0"),
        ("Object.getOwnPropertySymbols([1]).length", "0"),
        // The Symbol itself is reported, so it compares identical.
        (
            "(function(){\
                const s=Symbol('q');\
                const o={a:1};\
                o[s]=2;\
                const symbols=Object.getOwnPropertySymbols(o);\
                return symbols.length+'|'+String(symbols[0])+'|'+(symbols[0]===s);\
            })()",
            "1|Symbol(q)|true",
        ),
        // Two symbols sharing a description stay distinct, and creation order
        // is the reported order.
        (
            "(function(){\
                const first=Symbol('q');\
                const second=Symbol('q');\
                const o={};\
                o[second]=1;\
                o[first]=2;\
                const symbols=Object.getOwnPropertySymbols(o);\
                return symbols.length+'|'+(symbols[0]===second)+'|'+(symbols[1]===first);\
            })()",
            "2|true|true",
        ),
        // A non-enumerable symbol-keyed property is still reported.
        (
            "(function(){\
                const s=Symbol('q');\
                const o={};\
                Object.defineProperty(o,s,{value:1,enumerable:false});\
                return Object.getOwnPropertySymbols(o).length;\
            })()",
            "1",
        ),
        // A well-known symbol is reported like any other.
        (
            "(function(){\
                const o={};\
                o[Symbol.toStringTag]='T';\
                const symbols=Object.getOwnPropertySymbols(o);\
                return symbols.length+'|'+(symbols[0]===Symbol.toStringTag);\
            })()",
            "1|true",
        ),
        // A primitive answers empty rather than throwing, because a boxed
        // wrapper never carries a symbol key.
        ("Object.getOwnPropertySymbols(1).length", "0"),
        ("Object.getOwnPropertySymbols('ab').length", "0"),
        ("Object.getOwnPropertySymbols(true).length", "0"),
        // A fresh Array is returned each time.
        (
            "Object.getOwnPropertySymbols({})!==Object.getOwnPropertySymbols({})",
            "true",
        ),
        ("Array.isArray(Object.getOwnPropertySymbols({}))", "true"),
    ]);
    assert_throws(
        "return Object.getOwnPropertySymbols(null);",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
    assert_throws(
        "return Object.getOwnPropertySymbols();",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
}

/// Each static carries the pinned `name` and `length`.
#[test]
fn each_static_carries_the_pinned_identity() {
    assert_all(&[
        ("Object.is.length", "2"),
        ("Object.hasOwn.length", "2"),
        ("Object.getOwnPropertySymbols.length", "1"),
        ("Object.is.name", "is"),
        ("Object.hasOwn.name", "hasOwn"),
        ("Object.getOwnPropertySymbols.name", "getOwnPropertySymbols"),
        ("typeof Object.is", "function"),
        (
            "Object.getOwnPropertyDescriptor(Object,'is').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Object,'is').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Object,'is').configurable",
            "true",
        ),
    ]);
}
