//! `Object.groupBy`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'const g = Object.groupBy([1,2,3,4],\
//!     v => v % 2 ? "odd" : "even");\
//!     console.log(g.odd.join(","), g.even.join(","), Object.getPrototypeOf(g));'
//! 1,3 2,4 null
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! Object.groupBy([1,2,3,4], parity) => {odd:[1,3], even:[2,4]}
//! the result has a NULL prototype, so a group key cannot collide with an
//!   inherited property
//! each group is a fresh base Array, and each property is writable/enumerable/
//!   configurable
//! the callback receives (item, index) and its result becomes the key
//! a returned object key runs its toString; a number becomes its decimal string;
//!   a symbol stays a symbol
//! groups appear in first-use order
//! a non-callable callback !! TypeError: not a function, before the iterable is
//!   probed
//! a non-iterable argument !! TypeError: value is not iterable
//! a throwing callback closes the iterator
//! lengths: groupBy 2
//! ```
//!
//! One expectation below is the specification's rather than the oracle's: a
//! *strict* callback must receive `undefined` as its `this`, which the oracle's
//! own `forEach` and `map` honor but its `groupBy` does not. See PORTING.md.
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
                    Arc::from("<runtime Object groupBy>"),
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

/// `Object.groupBy` collects each item into the group its callback names.
#[test]
fn object_group_by_collects_items_into_named_groups() {
    assert_all(&[
        (
            "(function(){\
                const g=Object.groupBy([1,2,3,4],function(v){return v%2?'odd':'even';});\
                return g.odd.join(',')+'|'+g.even.join(',');\
            })()",
            "1,3|2,4",
        ),
        // The result has a null prototype, so a group key can never collide
        // with an inherited property.
        (
            "String(Object.getPrototypeOf(Object.groupBy([],function(){return 'k';})))",
            "null",
        ),
        (
            "Reflect.ownKeys(Object.groupBy([],function(){return 'k';})).length",
            "0",
        ),
        // Each group is a fresh base Array.
        (
            "(function(){\
                const g=Object.groupBy([1],function(){return 'k';});\
                return Array.isArray(g.k)+'|'+(Object.getPrototypeOf(g.k)===Array.prototype);\
            })()",
            "true|true",
        ),
        // Each group property is fully mutable.
        (
            "(function(){\
                const g=Object.groupBy([1],function(){return 'k';});\
                const d=Object.getOwnPropertyDescriptor(g,'k');\
                return d.writable+'|'+d.enumerable+'|'+d.configurable;\
            })()",
            "true|true|true",
        ),
        // Groups appear in first-use order, not sorted.
        (
            "(function(){\
                const g=Object.groupBy([1,2,3],function(v){return v===2?'b':'a';});\
                return Object.keys(g).join(',');\
            })()",
            "a,b",
        ),
        // A primitive `String` is iterable, so its characters group.
        (
            "(function(){\
                const g=Object.groupBy('ab',function(v){return v;});\
                return g.a.join('')+'|'+g.b.join('');\
            })()",
            "a|b",
        ),
    ]);
}

/// The callback receives each item with its index, and its result is the key.
#[test]
fn the_callback_names_each_group() {
    assert_all(&[
        (
            "(function(){\
                const log=[];\
                Object.groupBy(['a','b'],function(v,i){log.push(v+':'+i);return 'k';});\
                return log.join(',');\
            })()",
            "a:0,b:1",
        ),
        // The key converts with `ToPropertyKey`, which can run a `toString`.
        (
            "(function(){\
                const g=Object.groupBy([1],function(){\
                    return {toString(){return 'z';}};\
                });\
                return Object.keys(g).join(',');\
            })()",
            "z",
        ),
        // A number becomes its decimal string; a symbol stays a symbol.
        (
            "(function(){\
                const g=Object.groupBy([1],function(){return 0;});\
                return Object.keys(g).join(',');\
            })()",
            "0",
        ),
        (
            "(function(){\
                const s=Symbol('q');\
                const g=Object.groupBy([1],function(){return s;});\
                return g[s].length+'|'+Reflect.ownKeys(g).length;\
            })()",
            "1|1",
        ),
        // A strict callback receives `undefined` as its `this`, exactly as
        // `Array.prototype.forEach`'s does.
        (
            "(function(){\
                let seen;\
                Object.groupBy([1],function(){'use strict';seen=this;return 'k';});\
                return String(seen);\
            })()",
            "undefined",
        ),
        (
            "(function(){\
                let seen;\
                [1].forEach(function(){'use strict';seen=this;});\
                return String(seen);\
            })()",
            "undefined",
        ),
    ]);
    // The callback is validated before the iterable is probed.
    assert_throws(
        "return Object.groupBy([1],1);",
        ExceptionKind::TypeError,
        "not a function",
    );
    assert_throws(
        "return Object.groupBy(1,function(){return 'k';});",
        ExceptionKind::TypeError,
        "value is not iterable",
    );
}

/// A throwing callback closes the iterator.
#[test]
fn a_throwing_callback_closes_the_iterator() {
    assert_all(&[(
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
            try {\
                Object.groupBy(iterable,function(){throw new TypeError('cb');});\
            } catch (thrown) {\
                return closed+'|'+thrown.message;\
            }\
            return 'not thrown';\
        })()",
        "true|cb",
    )]);
}

/// `Object.groupBy` carries the pinned `name` and `length`.
#[test]
fn object_group_by_carries_the_pinned_identity() {
    assert_all(&[
        ("Object.groupBy.length", "2"),
        ("Object.groupBy.name", "groupBy"),
        (
            "Object.getOwnPropertyDescriptor(Object,'groupBy').enumerable",
            "false",
        ),
    ]);
}
