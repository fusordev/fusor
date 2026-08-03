//! `Object.fromEntries`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'const o = Object.fromEntries([["a",1],["b",2]]);\
//!     console.log(o.a, o.b, Object.getPrototypeOf(o) === Object.prototype);'
//! 1 2 true
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! Object.fromEntries([["a",1],["b",2]]) => {a:1, b:2}; a repeated key overwrites
//! the result inherits Object.prototype and each property is w/e/c
//! a numeric key becomes its decimal string; a symbol key stays a symbol
//! an object key runs its toString, after the entry's value has been read
//! per entry: index 0, then index 1
//! [[]] => one property named "undefined"; [["a"]] => a is undefined
//! a non-object entry !! TypeError: not an object
//! a non-iterable argument !! TypeError: value is not iterable
//! a rejected entry, and a throwing key getter, both close the iterator
//! lengths: fromEntries 1
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
                    Arc::from("<runtime Object fromEntries>"),
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

/// `Object.fromEntries` builds one own property per drained entry.
#[test]
fn object_from_entries_defines_one_property_per_entry() {
    assert_all(&[
        (
            "(function(){\
                const o=Object.fromEntries([['a',1],['b',2]]);\
                return o.a+','+o.b;\
            })()",
            "1,2",
        ),
        // A repeated key overwrites the earlier entry.
        ("Object.fromEntries([['a',1],['a',2]]).a", "2"),
        ("Reflect.ownKeys(Object.fromEntries([])).length", "0"),
        // The result is an ordinary object, and each property is fully mutable.
        (
            "Object.getPrototypeOf(Object.fromEntries([]))===Object.prototype",
            "true",
        ),
        (
            "(function(){\
                const d=Object.getOwnPropertyDescriptor(Object.fromEntries([['a',1]]),'a');\
                return d.writable+'|'+d.enumerable+'|'+d.configurable;\
            })()",
            "true|true|true",
        ),
        // A numeric key becomes its decimal string, and a symbol key stays one.
        ("Object.fromEntries([[0,'z']])[0]", "z"),
        (
            "Reflect.ownKeys(Object.fromEntries([[0,'z']])).join(',')",
            "0",
        ),
        (
            "(function(){\
                const s=Symbol('q');\
                return Object.fromEntries([[s,1]])[s];\
            })()",
            "1",
        ),
        // An absent index reads as `undefined`, so a short entry still defines
        // a property named `\"undefined\"`.
        (
            "Reflect.ownKeys(Object.fromEntries([[]])).join(',')",
            "undefined",
        ),
        ("String(Object.fromEntries([['a']]).a)", "undefined"),
        // A fresh object is returned each time.
        ("Object.fromEntries([])!==Object.fromEntries([])", "true"),
    ]);
}

/// Each entry is read index by index, and its key converts last.
#[test]
fn each_entry_is_read_in_index_order() {
    assert_all(&[
        // Index `0` then index `1`, both of which can enter an accessor.
        (
            "(function(){\
                let log='';\
                const entry={};\
                Object.defineProperty(entry,'0',{get(){log+='k';return 'a';},configurable:true});\
                Object.defineProperty(entry,'1',{get(){log+='v';return 1;},configurable:true});\
                const o=Object.fromEntries([entry]);\
                return log+'|'+o.a;\
            })()",
            "kv|1",
        ),
        // The key's `ToPropertyKey` runs after both reads.
        (
            "(function(){\
                let log='';\
                const key={toString(){log+='t';return 'a';}};\
                const entry={};\
                Object.defineProperty(entry,'0',{get(){log+='k';return key;},configurable:true});\
                Object.defineProperty(entry,'1',{get(){log+='v';return 1;},configurable:true});\
                const o=Object.fromEntries([entry]);\
                return log+'|'+o.a;\
            })()",
            "kvt|1",
        ),
        // Entries are visited in iteration order.
        (
            "(function(){\
                let log='';\
                function entry(name){\
                    const pair={};\
                    Object.defineProperty(pair,'0',{get(){log+=name;return name;},configurable:true});\
                    Object.defineProperty(pair,'1',{value:1,configurable:true});\
                    return pair;\
                }\
                Object.fromEntries([entry('a'),entry('b')]);\
                return log;\
            })()",
            "ab",
        ),
    ]);
}

/// A rejected entry or a throwing read closes the iterator.
#[test]
fn an_abrupt_exit_closes_the_iterator() {
    assert_all(&[
        // A non-object entry is rejected, and `return` runs before the throw
        // propagates.
        (
            "(function(){\
                let closed=false;\
                const iterable={};\
                function iterate(){\
                    return {\
                        next(){return {done:false,value:1};},\
                        return(){closed=true;return {};}\
                    };\
                }\
                iterable[Symbol.iterator]=iterate;\
                try { Object.fromEntries(iterable); } catch (thrown) { return String(closed); }\
                return 'not thrown';\
            })()",
            "true",
        ),
        // A throwing key getter closes it too, and the original throw survives.
        (
            "(function(){\
                let closed=false;\
                const marker={};\
                const entry={};\
                Object.defineProperty(entry,'0',{get(){throw marker;},configurable:true});\
                const iterable={};\
                function iterate(){\
                    return {\
                        next(){return {done:false,value:entry};},\
                        return(){closed=true;return {};}\
                    };\
                }\
                iterable[Symbol.iterator]=iterate;\
                try { Object.fromEntries(iterable); } catch (thrown) {\
                    return closed+'|'+(thrown===marker);\
                }\
                return 'not thrown';\
            })()",
            "true|true",
        ),
        // An iterator that finishes on its own is never closed.
        (
            "(function(){\
                let closed=false;\
                let sent=false;\
                const iterable={};\
                function iterate(){\
                    return {\
                        next(){\
                            if (sent) { return {done:true,value:undefined}; }\
                            sent=true;\
                            return {done:false,value:['a',1]};\
                        },\
                        return(){closed=true;return {};}\
                    };\
                }\
                iterable[Symbol.iterator]=iterate;\
                const o=Object.fromEntries(iterable);\
                return closed+'|'+o.a;\
            })()",
            "false|1",
        ),
    ]);
    // A non-object entry reports the pinned message.
    assert_throws(
        "return Object.fromEntries(['ab']);",
        ExceptionKind::TypeError,
        "not an object",
    );
    assert_throws(
        "return Object.fromEntries([1]);",
        ExceptionKind::TypeError,
        "not an object",
    );
    assert_throws(
        "return Object.fromEntries([null]);",
        ExceptionKind::TypeError,
        "not an object",
    );
    // A non-iterable argument is rejected before any entry is read.
    assert_throws(
        "return Object.fromEntries(1);",
        ExceptionKind::TypeError,
        "value is not iterable",
    );
}

/// `Object.fromEntries` carries the pinned `name` and `length`.
#[test]
fn object_from_entries_carries_the_pinned_identity() {
    assert_all(&[
        ("Object.fromEntries.length", "1"),
        ("Object.fromEntries.name", "fromEntries"),
        (
            "Object.getOwnPropertyDescriptor(Object,'fromEntries').enumerable",
            "false",
        ),
    ]);
}
