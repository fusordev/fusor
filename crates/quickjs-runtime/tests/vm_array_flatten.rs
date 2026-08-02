//! `Array.prototype.flat` and `flatMap`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'console.log([1,,[3]].flat().length,\
//!     [1,,[3]].flat().join("|"));'
//! 2 1|3
//! ```
//!
//! Both methods run `JS_FlattenIntoArray` (`quickjs.c:43014-43074`): each
//! present source element is appended to a fresh Array or, when the remaining
//! depth is positive and the element is a real Array, entered and read element
//! by element. Holes are skipped rather than appended.
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! [[1,2],[3]].flat() => "1,2,3"             [1,[2,[3,[4]]]].flat() => [1,2,[3,[4]]]
//! [1,[2,[3,[4]]]].flat(2) => [1,2,3,[4]]    [1,[2,[3,[4]]]].flat(Infinity) => "1,2,3,4"
//! [1,[2]].flat(0) => no flattening          [1,[2]].flat(null) => no flattening
//! [1,[2]].flat(NaN) => no flattening        [1,[2]].flat(1.9) => one level
//! [1,[2]].flat(undefined) => one level
//! [1,,[3]].flat() => length 2, "1|3" (holes are skipped)
//! [1,2].flatMap(x=>[x,[x]]) => [1,[1],2,[2]] (mapped results flatten one level)
//! [[1,2],[3]].flatMap(x=>x) => "1,2,3"      [1,,3].flatMap(x=>x*2) => "2|6", 2 calls
//! [10,20].flatMap(f,{b:100}) => "110,121"   flatMap on null !! cannot convert to object
//! [1].flatMap(5) !! TypeError: not a function (checked after the length read)
//! lengths: flat 0, flatMap 1
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
                    Arc::from("<runtime Array flatten>"),
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

/// `flat` flattens nested real Arrays up to its depth.
#[test]
fn flat_flattens_nested_arrays_up_to_its_depth() {
    assert_all(&[
        ("[[1,2],[3]].flat().join()", "1,2,3"),
        // The default depth is one level.
        (
            "[1,[2,[3,[4]]]].flat().map(function(x){return Array.isArray(x)?'A':x;}).join('|')",
            "1|2|A",
        ),
        (
            "[1,[2,[3,[4]]]].flat(2).map(function(x){return Array.isArray(x)?'A':x;}).join('|')",
            "1|2|3|A",
        ),
        // A saturating depth flattens the whole nesting.
        ("[1,[2,[3,[4]]]].flat(Infinity).join()", "1,2,3,4"),
        // A non-positive depth flattens nothing.
        (
            "[1,[2]].flat(0).map(function(x){return Array.isArray(x)?'A':x;}).join('|')",
            "1|A",
        ),
        (
            "[1,[2]].flat(-1).map(function(x){return Array.isArray(x)?'A':x;}).join('|')",
            "1|A",
        ),
        // `null` and `NaN` convert to depth zero; `undefined` keeps the
        // default of one.
        (
            "[1,[2]].flat(null).map(function(x){return Array.isArray(x)?'A':x;}).join('|')",
            "1|A",
        ),
        (
            "[1,[2]].flat(NaN).map(function(x){return Array.isArray(x)?'A':x;}).join('|')",
            "1|A",
        ),
        ("[1,[2]].flat(undefined).join()", "1,2"),
        // A fractional depth truncates toward zero.
        (
            "[1,[2,[3]]].flat(1.9).map(function(x){return Array.isArray(x)?'A':x;}).join('|')",
            "1|2|A",
        ),
        // The result is a fresh Array; the receiver is not mutated.
        (
            "(function(){const a=[[1],2];const b=a.flat();return a.length+'|'+Array.isArray(b);})()",
            "2|true",
        ),
    ]);
}

/// Holes are skipped rather than appended as `undefined`.
#[test]
fn flat_skips_holes() {
    assert_all(&[
        (
            "(function(){\
                const r=[1,,[3]].flat();\
                return r.join()+'|'+r.length+'|'+Object.prototype.hasOwnProperty.call(r,1);\
            })()",
            "1,3|2|true",
        ),
        (
            "(function(){\
                const r=[[1],,[2]].flat();\
                return r.join()+'|'+r.length;\
            })()",
            "1,2|2",
        ),
    ]);
}

/// `flat` accepts an array-like receiver and answers a real Array.
#[test]
fn flat_accepts_an_array_like_receiver() {
    assert_all(&[
        (
            "(function(){\
                const r=Array.prototype.flat.call({length:2,0:[1,2],1:3});\
                return r.join()+'|'+r.length+'|'+Array.isArray(r);\
            })()",
            "1,2,3|3|true",
        ),
        // An absent index of an array-like is skipped like a hole.
        (
            "(function(){\
                const r=Array.prototype.flat.call({length:3,0:'a',2:'c'});\
                return r.join()+'|'+r.length;\
            })()",
            "a,c|2",
        ),
    ]);
}

/// `flatMap` maps each element and flattens the results one level.
#[test]
fn flat_map_maps_and_flattens_one_level() {
    assert_all(&[
        (
            "[1,2].flatMap(function(x){return [x,x*2];}).join()",
            "1,2,2,4",
        ),
        (
            "[[1,2],[3]].flatMap(function(x){return x;}).join()",
            "1,2,3",
        ),
        // A mapped result that is an Array flattens exactly one level.
        (
            "[1,2].flatMap(function(x){return [x,[x]];}).map(function(x){return Array.isArray(x)?'A':x;}).join('|')",
            "1|A|2|A",
        ),
        // Holes are skipped: the mapper never runs for them.
        (
            "(function(){\
                let calls=0;\
                const r=[1,,3].flatMap(function(x){calls++;return x*2;});\
                return r.join()+'|'+r.length+'|'+calls;\
            })()",
            "2,6|2|2",
        ),
        // The mapper receives `(element, index, source)` and the `thisArg`.
        (
            "[10,20].flatMap(function(x,i){return [x+i+this.b];},{b:100}).join()",
            "110,121",
        ),
        (
            "(function(){\
                let seen='';\
                [5].flatMap(function(x,i,a){seen=[x,i,a.length,this.t].join(',');return x;},{t:9});\
                return seen;\
            })()",
            "5,0,1,9",
        ),
        // An array-like receiver is mapped and flattened the same way.
        (
            "(function(){\
                const r=Array.prototype.flatMap.call({length:2,0:'a',1:'b'},function(x){return x+'!';});\
                return r.join()+'|'+Array.isArray(r);\
            })()",
            "a!,b!|true",
        ),
    ]);
}

/// A non-callable mapper is rejected after the length read.
#[test]
fn flat_map_rejects_a_non_callable_mapper() {
    assert_throws(
        "return [1].flatMap(5);",
        ExceptionKind::TypeError,
        "not a function",
    );
    assert_throws(
        "return [1].flatMap();",
        ExceptionKind::TypeError,
        "not a function",
    );
    // The length is read first, so its getter's exception beats the mapper
    // check.
    assert_throws(
        "return Array.prototype.flatMap.call({get length(){return null.x;}},5);",
        ExceptionKind::TypeError,
        "cannot read property 'x' of null",
    );
}

/// Every element read can enter an accessor, in ascending index order.
#[test]
fn element_reads_enter_accessors_in_order() {
    assert_all(&[
        (
            "(function(){\
                let log='';\
                const o={length:2,0:'a'};\
                Object.defineProperty(o,1,{get(){log+='g1|';return ['b'];},configurable:true});\
                Array.prototype.flat.call(o);\
                return log;\
            })()",
            "g1|",
        ),
        // The length is read once, before any element.
        (
            "(function(){\
                let log='';\
                const o={get length(){log+='len|';return 2;},0:'a'};\
                Object.defineProperty(o,1,{get(){log+='g1|';return 'b';},configurable:true});\
                Array.prototype.flat.call(o);\
                return log;\
            })()",
            "len|g1|",
        ),
    ]);
}

/// A nullish receiver is rejected before the length is read.
#[test]
fn a_nullish_receiver_is_rejected() {
    for method in ["flat", "flatMap"] {
        for receiver in ["null", "undefined"] {
            let mapper = if method == "flatMap" {
                ",function(x){return x;}"
            } else {
                ""
            };
            assert_throws(
                &format!("return Array.prototype.{method}.call({receiver}{mapper});"),
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
        ("Array.prototype.flat.length", "0"),
        ("Array.prototype.flatMap.length", "1"),
        ("Array.prototype.flat.name", "flat"),
        ("Array.prototype.flatMap.name", "flatMap"),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'flat').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'flatMap').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'flat').configurable",
            "true",
        ),
    ]);
}
