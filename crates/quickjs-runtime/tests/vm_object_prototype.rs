//! `Object.prototype.toLocaleString`, the `__proto__` accessor pair, and the
//! legacy `__defineGetter__`/`__lookupGetter__` family.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'const d = Object.getOwnPropertyDescriptor(\
//!     Object.prototype, "__proto__");\
//!     console.log(d.get.name, d.set.name, d.enumerable, d.configurable);'
//! get __proto__ set __proto__ false true
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! ({}).toLocaleString() => "[object Object]"; it forwards to the own toString
//! toLocaleString passes no argument along: o.toLocaleString("en") sees undefined
//! ({}).__proto__ === Object.prototype; Object.create(null).__proto__ is undefined
//! o.__proto__ = Array.prototype changes it; = null clears it; = 5 is ignored
//! a frozen receiver !! TypeError: object is not extensible
//! (5).__proto__ === Number.prototype
//! __proto__ is an accessor pair: get/set, non-enumerable, configurable,
//!   named "get __proto__" and "set __proto__" with lengths 0 and 1
//! __defineGetter__ defines an enumerable, configurable accessor and returns
//!   undefined; a non-callable accessor !! TypeError: not a function
//! __lookupGetter__ walks the prototype chain and answers undefined for a data
//!   property or an absent one
//! a nullish receiver !! TypeError: cannot convert to object
//! lengths: toLocaleString 0, __defineGetter__ 2, __lookupGetter__ 1
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
                    Arc::from("<runtime Object prototype>"),
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

/// `Object.prototype.toLocaleString` forwards to the receiver's own `toString`.
#[test]
fn to_locale_string_forwards_to_to_string() {
    assert_all(&[
        ("({}).toLocaleString()", "[object Object]"),
        // The receiver's own `toString` is used, wherever on the chain it is.
        (
            "(function(){\
                let log='';\
                const o={toString(){log+='t';return 'X';}};\
                return o.toLocaleString()+'|'+log;\
            })()",
            "X|t",
        ),
        // No argument is passed along: the locale parameters belong to `Intl`,
        // and the base implementation ignores them.
        (
            "(function(){\
                let seen;\
                const o={toString(locale){seen=locale;return 'X';}};\
                o.toLocaleString('en');\
                return String(seen);\
            })()",
            "undefined",
        ),
        ("Object.prototype.toLocaleString.length", "0"),
        ("Object.prototype.toLocaleString.name", "toLocaleString"),
        (
            "(function(){\
                const d=Object.getOwnPropertyDescriptor(Object.prototype,'toLocaleString');\
                return d.writable+'|'+d.enumerable+'|'+d.configurable;\
            })()",
            "true|false|true",
        ),
    ]);
}

/// The `__proto__` accessor pair reads and writes the prototype.
#[test]
fn the_proto_accessor_pair_reads_and_writes_the_prototype() {
    assert_all(&[
        ("({}).__proto__===Object.prototype", "true"),
        // A null prototype reads as `undefined` through the getter, because the
        // getter answers with the absent slot rather than with `null`.
        ("String(Object.create(null).__proto__)", "undefined"),
        (
            "(function(){\
                const o={};\
                o.__proto__=Array.prototype;\
                return Object.getPrototypeOf(o)===Array.prototype;\
            })()",
            "true",
        ),
        (
            "(function(){\
                const o={};\
                o.__proto__=null;\
                return String(Object.getPrototypeOf(o));\
            })()",
            "null",
        ),
        // A non-object, non-null value is silently ignored, which is what
        // separates the setter from `Object.setPrototypeOf`'s rejection.
        (
            "(function(){\
                const o={};\
                const before=Object.getPrototypeOf(o);\
                o.__proto__=5;\
                return Object.getPrototypeOf(o)===before;\
            })()",
            "true",
        ),
        // A primitive receiver answers with its wrapper's prototype.
        ("(5).__proto__===Number.prototype", "true"),
        // The pair's own shape.
        (
            "(function(){\
                const d=Object.getOwnPropertyDescriptor(Object.prototype,'__proto__');\
                return (typeof d.get)+'|'+(typeof d.set)+'|'+d.enumerable+'|'+d.configurable;\
            })()",
            "function|function|false|true",
        ),
        (
            "(function(){\
                const d=Object.getOwnPropertyDescriptor(Object.prototype,'__proto__');\
                return d.get.name+'|'+d.set.name+'|'+d.get.length+'|'+d.set.length;\
            })()",
            "get __proto__|set __proto__|0|1",
        ),
    ]);
    // A refused change still throws, since the setter is an ordinary strict Set.
    assert_throws(
        "(function(){ const o=Object.freeze({}); o.__proto__=Array.prototype; })()",
        ExceptionKind::TypeError,
        "object is not extensible",
    );
}

/// The legacy accessor definers install an enumerable, configurable accessor.
#[test]
fn the_legacy_definers_install_an_enumerable_accessor() {
    assert_all(&[
        // Unlike `Object.defineProperty`, the flags default to `true`, and only
        // the addressed half is supplied.
        (
            "(function(){\
                const o={};\
                o.__defineGetter__('a',function(){return 7;});\
                const d=Object.getOwnPropertyDescriptor(o,'a');\
                return o.a+'|'+d.enumerable+'|'+d.configurable+'|'+String(d.set);\
            })()",
            "7|true|true|undefined",
        ),
        (
            "(function(){\
                let log='';\
                const o={};\
                o.__defineSetter__('a',function(v){log+=v;});\
                o.a=5;\
                return log;\
            })()",
            "5",
        ),
        // The key converts with `ToPropertyKey`, which can run a `toString`.
        (
            "(function(){\
                let log='';\
                const key={toString(){log+='k';return 'a';}};\
                const o={};\
                o.__defineGetter__(key,function(){return 1;});\
                return log+'|'+o.a;\
            })()",
            "k|1",
        ),
        // A primitive receiver has no slot to define into, and completes.
        (
            "String((5).__defineGetter__('a',function(){return 1;}))",
            "undefined",
        ),
        (
            "(function(){\
                return Object.prototype.__defineGetter__.length+'|'+Object.prototype.__lookupGetter__.length;\
            })()",
            "2|1",
        ),
        ("Object.prototype.__defineGetter__.name", "__defineGetter__"),
    ]);
    // The accessor is validated before the key converts.
    assert_throws(
        "return ({}).__defineGetter__('a',1);",
        ExceptionKind::TypeError,
        "not a function",
    );
    assert_throws(
        "return Object.prototype.__defineGetter__.call(null,'a',function(){});",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
    assert_throws(
        "return Object.prototype.__defineGetter__.call(Object.freeze({}),'a',function(){});",
        ExceptionKind::TypeError,
        "object is not extensible",
    );
}

/// The legacy lookups walk the prototype chain for one accessor half.
#[test]
fn the_legacy_lookups_walk_the_prototype_chain() {
    assert_all(&[
        (
            "(function(){\
                const o={get a(){return 1;}};\
                return typeof o.__lookupGetter__('a');\
            })()",
            "function",
        ),
        // An inherited accessor is found too.
        (
            "(function(){\
                const parent={get a(){return 1;}};\
                return typeof Object.create(parent).__lookupGetter__('a');\
            })()",
            "function",
        ),
        // A data property answers `undefined` rather than its value.
        ("String(({a:1}).__lookupGetter__('a'))", "undefined"),
        ("String(({}).__lookupGetter__('a'))", "undefined"),
        // Only the addressed half is reported.
        (
            "(function(){\
                const o={set a(v){}};\
                return typeof o.__lookupSetter__('a');\
            })()",
            "function",
        ),
        (
            "(function(){\
                const o={get a(){return 1;}};\
                return String(o.__lookupSetter__('a'));\
            })()",
            "undefined",
        ),
        // A primitive receiver answers through its wrapper prototype's chain.
        ("String((5).__lookupGetter__('a'))", "undefined"),
    ]);
    assert_throws(
        "return Object.prototype.__lookupGetter__.call(null,'a');",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
}
