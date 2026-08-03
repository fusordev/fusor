//! `Object.values`, `Object.entries`, and `Object.getOwnPropertyDescriptors`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'console.log(Object.values({b:2,a:1}),\
//!     Object.entries("ab").map(p => p.join(":")).join(","));'
//! [ 2, 1 ] 0:a,1:b
//! ```
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! Object.values({b:2,a:1}) => [2,1]        Object.values({b:1,2:2,0:3}) => [3,2,1]
//! Object.values([1,,3]) => [1,3]           Object.values("ab") => ["a","b"]
//! Object.values(1) => []                   Object.values(null) !! cannot convert to object
//! non-enumerable and symbol keys are skipped by values and entries
//! Object.entries({a:1})[0] is a fresh two-element Array
//! a getter is entered per key, in the snapshot's order: log "ab"
//! a key added during the walk is not visited; one deleted or hidden is skipped
//! Object.getOwnPropertyDescriptors includes non-enumerable and symbol keys:
//!   {b:1, 0:2, [Symbol("q")]:3, h (non-enumerable)} => 0,b,h,Symbol(q)
//! an accessor contributes get/set and the getter is never called
//! each descriptor entry is writable/enumerable/configurable
//! Object.getOwnPropertyDescriptors("ab") => 0,1,length with 0 non-writable
//! lengths: values 1, entries 1, getOwnPropertyDescriptors 1
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
                    Arc::from("<runtime Object listings>"),
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

/// `Object.values` reads each own enumerable string key.
#[test]
fn object_values_reads_every_enumerable_own_key() {
    assert_all(&[
        ("Object.values({b:2,a:1}).join(',')", "2,1"),
        ("Object.values({}).length", "0"),
        // Ascending indices precede the string keys, which follow in creation
        // order.
        ("Object.values({b:1,2:2,0:3}).join(',')", "3,2,1"),
        // An inherited property is not an own one.
        ("Object.values(Object.create({a:1})).length", "0"),
        // A non-enumerable own property is skipped.
        (
            "(function(){\
                const o={};\
                Object.defineProperty(o,'h',{value:1});\
                o.v=2;\
                return Object.values(o).join(',');\
            })()",
            "2",
        ),
        // A symbol key is never listed, which is what separates this from
        // `Reflect.ownKeys`.
        (
            "(function(){\
                const s=Symbol('q');\
                const o={};\
                o[s]=1;\
                o.a=2;\
                return Object.values(o).join(',');\
            })()",
            "2",
        ),
        // An array's holes are absent and its `length` is not enumerable.
        ("Object.values([1,,3]).join(',')", "1,3"),
        ("Object.values([1,2]).join(',')", "1,2"),
        // A primitive `String` contributes its characters, not its `length`.
        ("Object.values('ab').join(',')", "a,b"),
        ("Object.values(Object('ab')).join(',')", "a,b"),
        // Another primitive contributes nothing, without throwing.
        ("Object.values(1).length", "0"),
        ("Object.values(true).length", "0"),
        // A fresh base Array is returned each time.
        ("Array.isArray(Object.values({}))", "true"),
        ("Object.values({})!==Object.values({})", "true"),
    ]);
    assert_throws(
        "return Object.values(null);",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
    assert_throws(
        "return Object.values();",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
}

/// `Object.entries` pairs each key with its value.
#[test]
fn object_entries_pairs_each_key_with_its_value() {
    assert_all(&[
        (
            "Object.entries({b:2,a:1}).map(function(e){return e.join(':');}).join(',')",
            "b:2,a:1",
        ),
        ("Object.entries({}).length", "0"),
        // Each pair is a fresh two-element base Array, and an index key is
        // reported as its decimal string.
        (
            "(function(){\
                const pairs=Object.entries({a:1});\
                return Array.isArray(pairs[0])+'|'+pairs[0].length+'|'+(Object.getPrototypeOf(pairs[0])===Array.prototype);\
            })()",
            "true|2|true",
        ),
        (
            "Object.entries([1,2]).map(function(e){return e.join(':');}).join(',')",
            "0:1,1:2",
        ),
        (
            "Object.entries('ab').map(function(e){return e.join(':');}).join(',')",
            "0:a,1:b",
        ),
        ("Object.entries(1).length", "0"),
        // The same enumerable-only, string-only projection `values` uses.
        (
            "(function(){\
                const s=Symbol('q');\
                const o={a:1};\
                o[s]=2;\
                Object.defineProperty(o,'h',{value:3});\
                return Object.entries(o).map(function(e){return e.join(':');}).join(',');\
            })()",
            "a:1",
        ),
    ]);
    assert_throws(
        "return Object.entries(null);",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
}

/// Both listings read through accessors, and re-test each key against the live
/// object so a getter's own mutations are observable.
#[test]
fn the_listings_read_through_accessors_in_key_order() {
    assert_all(&[
        // Every key is read, in the snapshot's order.
        (
            "(function(){\
                let log='';\
                const o={get a(){log+='a';return 1;},get b(){log+='b';return 2;}};\
                const values=Object.values(o);\
                return log+'|'+values.join(',');\
            })()",
            "ab|1,2",
        ),
        // The key set is snapshotted, so a key added during the walk is not
        // visited.
        (
            "(function(){\
                const o={get a(){o.z=9;return 1;},b:2};\
                return Object.values(o).join(',');\
            })()",
            "1,2",
        ),
        // A key deleted during the walk is skipped, because the enumerable
        // attribute is re-tested rather than trusted from the snapshot.
        (
            "(function(){\
                const o={get a(){delete o.b;return 1;},b:2};\
                return Object.values(o).join(',');\
            })()",
            "1",
        ),
        // Making a later key non-enumerable also removes it.
        (
            "(function(){\
                const o={get a(){Object.defineProperty(o,'b',{enumerable:false});return 1;},b:2};\
                return Object.values(o).join(',');\
            })()",
            "1",
        ),
        // A getter that runs several keys deep still leaves the earlier ones in
        // place.
        (
            "(function(){\
                let log='';\
                const o={get a(){log+='a';delete o.c;o.d=4;return 1;},b:2,c:3};\
                const values=Object.values(o);\
                return log+'|'+values.join(',');\
            })()",
            "a|1,2",
        ),
        // `entries` shares the same walk.
        (
            "(function(){\
                let log='';\
                const o={get a(){log+='a';return 1;}};\
                const pairs=Object.entries(o);\
                return log+'|'+pairs[0].join(':');\
            })()",
            "a|a:1",
        ),
    ]);
    // A throwing getter propagates rather than being swallowed.
    assert_all(&[(
        "(function(){\
            const marker={};\
            try {\
                Object.values({get a(){throw marker;}});\
            } catch (thrown) {\
                return String(thrown===marker);\
            }\
            return 'not thrown';\
        })()",
        "true",
    )]);
}

/// `Object.getOwnPropertyDescriptors` describes every own key without reading a
/// value.
#[test]
fn object_get_own_property_descriptors_describes_every_own_key() {
    assert_all(&[
        // Non-enumerable and symbol keys are included, in `[[OwnPropertyKeys]]`
        // order, which is what makes the result a valid `defineProperties`
        // argument.
        (
            "(function(){\
                const s=Symbol('q');\
                const o={b:1,0:2};\
                o[s]=3;\
                Object.defineProperty(o,'h',{value:4});\
                return Reflect.ownKeys(Object.getOwnPropertyDescriptors(o)).map(String).join(',');\
            })()",
            "0,b,h,Symbol(q)",
        ),
        (
            "(function(){\
                const d=Object.getOwnPropertyDescriptors({a:1});\
                return d.a.value+'|'+d.a.writable+'|'+d.a.enumerable+'|'+d.a.configurable;\
            })()",
            "1|true|true|true",
        ),
        (
            "Reflect.ownKeys(Object.getOwnPropertyDescriptors({})).length",
            "0",
        ),
        // An accessor contributes its functions; the getter is never called.
        (
            "(function(){\
                let log='';\
                const o={get a(){log+='a';return 1;}};\
                const d=Object.getOwnPropertyDescriptors(o);\
                return '['+log+']|'+(typeof d.a.get)+'|'+(typeof d.a.set)+'|'+String(d.a.value);\
            })()",
            "[]|function|undefined|undefined",
        ),
        // Each entry is itself fully mutable, and the result is an ordinary
        // object.
        (
            "(function(){\
                const d=Object.getOwnPropertyDescriptors({a:1});\
                const entry=Object.getOwnPropertyDescriptor(d,'a');\
                return entry.writable+'|'+entry.enumerable+'|'+entry.configurable;\
            })()",
            "true|true|true",
        ),
        (
            "Object.getPrototypeOf(Object.getOwnPropertyDescriptors({}))===Object.prototype",
            "true",
        ),
        // An array's `length` and a primitive `String`'s exotics are described
        // the way `getOwnPropertyDescriptor` describes them.
        (
            "(function(){\
                const d=Object.getOwnPropertyDescriptors([1]);\
                return Reflect.ownKeys(d).join(',')+'|'+d.length.value+'|'+d.length.enumerable;\
            })()",
            "0,length|1|false",
        ),
        (
            "(function(){\
                const d=Object.getOwnPropertyDescriptors('ab');\
                return Reflect.ownKeys(d).join(',')+'|'+d[0].value+'|'+d[0].writable+'|'+d.length.value;\
            })()",
            "0,1,length|a|false|2",
        ),
        (
            "Reflect.ownKeys(Object.getOwnPropertyDescriptors(1)).length",
            "0",
        ),
        // A null-prototype target is described like any other.
        (
            "(function(){\
                const o=Object.create(null);\
                Object.defineProperty(o,'a',{value:1});\
                const d=Object.getOwnPropertyDescriptors(o);\
                return Reflect.ownKeys(d).join(',')+'|'+d.a.value+'|'+d.a.writable;\
            })()",
            "a|1|false",
        ),
    ]);
    assert_throws(
        "return Object.getOwnPropertyDescriptors(null);",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
}

/// Each listing static carries the pinned `name` and `length`.
#[test]
fn each_listing_carries_the_pinned_identity() {
    assert_all(&[
        ("Object.values.length", "1"),
        ("Object.entries.length", "1"),
        ("Object.getOwnPropertyDescriptors.length", "1"),
        ("Object.values.name", "values"),
        ("Object.entries.name", "entries"),
        (
            "Object.getOwnPropertyDescriptors.name",
            "getOwnPropertyDescriptors",
        ),
        (
            "Object.getOwnPropertyDescriptor(Object,'values').enumerable",
            "false",
        ),
    ]);
}
