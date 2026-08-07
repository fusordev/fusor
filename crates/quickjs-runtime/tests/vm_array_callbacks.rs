//! The `Array.prototype` methods that take a callback.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'let n=0; [1,,3].forEach(() => n++);\
//!     let m=0; [1,,3].find(() => { m++; return false; }); console.log(n, m);'
//! 2 3
//! ```
//!
//! Three behaviors separate the nine methods, and the table below pins each.
//! Holes: `forEach`, `map`, `filter`, `every`, and `some` skip a missing index
//! while the `find` family visits it and sees `undefined`. Early exit: `every`
//! stops on a falsy result, `some` and the `find` family stop on a truthy one.
//! Result: `forEach` answers `undefined`, `map` and `filter` build a fresh Array,
//! `every` and `some` answer a Boolean, `find`/`findLast` the element, and
//! `findIndex`/`findLastIndex` the index.
//!
//! The length is snapshotted once, so a callback that grows the array is not
//! revisited and one that shrinks it still stops early.

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
                    Arc::from("<runtime Array callbacks>"),
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

/// Each method's result shape matches the oracle.
///
/// `forEach` answers `undefined`, `map` and `filter` build a fresh Array,
/// `every` and `some` answer a Boolean, `find`/`findLast` the element, and
/// `findIndex`/`findLastIndex` the index. The empty-array answers are the
/// identity elements of the two quantifiers: `every` is `true`, `some` is
/// `false`.
#[test]
fn the_results_match_the_oracle() {
    assert_all(&[
        (
            "(function(){let out='';[1,2,3].forEach(function(v){out+=v;});return out;})()",
            "123",
        ),
        ("String([1].forEach(function(){}))", "undefined"),
        ("[1,2,3].map(function(v){return v*2;}).join()", "2,4,6"),
        ("Array.isArray([1].map(function(v){return v;}))", "true"),
        (
            "[1,2,3,4].filter(function(v){return v%2===0;}).join()",
            "2,4",
        ),
        (
            "Array.isArray([1].filter(function(){return true;}))",
            "true",
        ),
        ("[1,2].every(function(v){return v>0;})", "true"),
        ("[1,-2].every(function(v){return v>0;})", "false"),
        ("[].every(function(){return false;})", "true"),
        ("[1,-2].some(function(v){return v<0;})", "true"),
        ("[1,2].some(function(v){return v<0;})", "false"),
        ("[].some(function(){return true;})", "false"),
        ("[1,2,3].find(function(v){return v>1;})", "2"),
        ("String([1].find(function(){return false;}))", "undefined"),
        ("[1,2,3].findIndex(function(v){return v>1;})", "1"),
        ("[1].findIndex(function(){return false;})", "-1"),
        ("[1,2,3].findLast(function(v){return v<3;})", "2"),
        ("[1,2,3].findLastIndex(function(v){return v<3;})", "1"),
        (
            "String([1].findLast(function(){return false;}))",
            "undefined",
        ),
        ("[1].findLastIndex(function(){return false;})", "-1"),
    ]);
}

/// The callback receives `(element, index, array)` and the `thisArg`.
#[test]
fn the_callback_receives_element_index_and_array() {
    assert_all(&[
        (
            "(function(){let out='';[7].forEach(function(v,i,a){out=v+'|'+i+'|'+a.length;});return out;})()",
            "7|0|1",
        ),
        (
            "(function(){let out='';[1].forEach(function(){out=this.x;},{x:5});return out;})()",
            "5",
        ),
    ]);
}

/// Most methods skip a hole; the `find` family visits it and sees `undefined`.
///
/// `map` still counts a skipped hole, so its result keeps the source's shape
/// rather than collapsing it.
#[test]
fn holes_are_skipped_except_by_the_find_family() {
    assert_all(&[
        (
            "(function(){let n=0;[1,,3].forEach(function(){n++;});return n;})()",
            "2",
        ),
        (
            "(function(){const r=[1,,3].map(function(v){return v;});return r.length+'|'+Object.prototype.hasOwnProperty.call(r,1);})()",
            "3|false",
        ),
        ("[1,,3].filter(function(){return true;}).length", "2"),
        (
            "(function(){let n=0;[1,,3].every(function(){n++;return true;});return n;})()",
            "2",
        ),
        (
            "(function(){let n=0;[1,,3].some(function(){n++;return false;});return n;})()",
            "2",
        ),
        (
            "(function(){let n=0;[1,,3].find(function(){n++;return false;});return n;})()",
            "3",
        ),
        (
            "(function(){let n=0;[1,,3].findIndex(function(){n++;return false;});return n;})()",
            "3",
        ),
    ]);
}

/// `every` stops on a falsy result while `some` and `find` stop on a truthy one.
#[test]
fn the_quantifiers_and_find_family_exit_early() {
    assert_all(&[
        (
            "(function(){let n=0;[1,2,3].some(function(){n++;return true;});return n;})()",
            "1",
        ),
        (
            "(function(){let n=0;[1,2,3].every(function(){n++;return false;});return n;})()",
            "1",
        ),
        (
            "(function(){let n=0;[1,2,3].find(function(){n++;return true;});return n;})()",
            "1",
        ),
    ]);
}

/// The length is read once, before the first callback.
///
/// A callback that grows the array is therefore not revisited, and one that
/// shrinks it still stops early because every index is re-tested.
#[test]
fn the_length_is_snapshotted_before_the_first_callback() {
    assert_all(&[
        (
            "(function(){let n=0;const o={get length(){n++;return 2;},0:1,1:2};Array.prototype.forEach.call(o,function(){});return n;})()",
            "1",
        ),
        (
            "(function(){let out='';const a=[1,2];a.forEach(function(v){out+=v;a.push(9);});return out+'|'+a.length;})()",
            "12|4",
        ),
        (
            "(function(){let out='';const a=[1,2,3];a.forEach(function(v){out+=v;a.length=1;});return out;})()",
            "1",
        ),
    ]);
}

/// The callback methods accept any array-like receiver.
#[test]
fn the_callback_methods_accept_an_array_like_receiver() {
    assert_all(&[
        (
            "(function(){let out='';Array.prototype.forEach.call({length:2,0:'a',1:'b'},function(v){out+=v;});return out;})()",
            "ab",
        ),
        (
            "(function(){const r=Array.prototype.map.call({length:2,0:'a',1:'b'},function(v){return v;});return Array.isArray(r)+'|'+r.join();})()",
            "true|a,b",
        ),
    ]);
}

/// Generic callback methods retain `ToObject(this)` as the callback's third
/// argument rather than passing the original primitive receiver.
#[test]
fn callback_methods_box_primitive_receivers_before_callback_invocation() {
    assert_all(&[(
        "(function(){\
            let accessed=false;\
            Boolean.prototype[0]=1;\
            Boolean.prototype.length=1;\
            const result=Array.prototype.every.call(false,function(value,index,object){\
                accessed=value===1&&index===0&&object instanceof Boolean;\
                return accessed;\
            });\
            return result+'|'+accessed;\
        })()",
        "true|true",
    )]);
}

/// `LengthOfArrayLike` applies `ToLength`, including a resumable
/// `ToPrimitive(number)` for an object-valued `length`.
#[test]
fn callback_methods_convert_object_valued_lengths_before_iteration() {
    assert_all(&[
        (
            "(function(){\
                let log='';\
                const source={0:1,length:{valueOf(){log+='valueOf|';return 1;}}};\
                const result=Array.prototype.every.call(source,function(value){return value===1;});\
                return result+'|'+log;\
            })()",
            "true|valueOf|",
        ),
        (
            "(function(){\
                let log='';\
                const source={0:1,length:{\
                    valueOf(){log+='valueOf|';return {};},\
                    toString(){log+='toString|';return '1';}\
                }};\
                const result=Array.prototype.every.call(source,function(value){return value===1;});\
                return result+'|'+log;\
            })()",
            "true|valueOf|toString|",
        ),
    ]);
}

/// Generic callback methods run `LengthOfArrayLike` before testing whether the
/// callback is callable, preserving the observable getter and coercion order.
#[test]
fn callback_methods_read_and_convert_length_before_rejecting_a_bad_callback() {
    assert_all(&[
        (
            "(function(){\
                let log='';\
                const source={get length(){log+='length|';return 1;}};\
                try{Array.prototype.every.call(source,null);}catch(error){\
                    return (error instanceof TypeError)+'|'+log;\
                }\
            })()",
            "true|length|",
        ),
        (
            "(function(){\
                let log='';\
                const source={get length(){return {toString(){log+='toString|';return '1';}};}};\
                try{Array.prototype.every.call(source,null);}catch(error){\
                    return (error instanceof TypeError)+'|'+log;\
                }\
            })()",
            "true|toString|",
        ),
    ]);
}

/// Generic callback methods observe a Proxy's `get` and `has` traps in the
/// order required by `LengthOfArrayLike` and the indexed iteration.
#[test]
fn callback_methods_use_proxy_internal_methods() {
    assert_all(&[(
        "(function(){\
            let log='';\
            const target={length:2,0:3};\
            const proxy=new Proxy(target,{\
                get:function(t,k){log+='g'+k+';';return t[k];},\
                has:function(t,k){log+='h'+k+';';return k in t;}\
            });\
            const result=Array.prototype.map.call(proxy,function(v){return v*2;});\
            return log+'|'+result.join()+'|'+Object.prototype.hasOwnProperty.call(result,1);\
        })()",
        "glength;h0;g0;h1;|6,|false",
    )]);
}

/// Every installed method reports arity 1 with the pinned descriptors.
#[test]
fn the_callback_methods_have_the_pinned_shape() {
    assert_all(&[
        ("Array.prototype.forEach.length", "1"),
        ("Array.prototype.map.length", "1"),
        ("Array.prototype.filter.length", "1"),
        ("Array.prototype.every.length", "1"),
        ("Array.prototype.some.length", "1"),
        ("Array.prototype.find.length", "1"),
        ("Array.prototype.findIndex.length", "1"),
        ("Array.prototype.findLast.length", "1"),
        ("Array.prototype.findLastIndex.length", "1"),
        ("Array.prototype.forEach.name", "forEach"),
        ("Array.prototype.findLast.name", "findLast"),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'map').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'map').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'map').configurable",
            "true",
        ),
    ]);
}

/// A non-callable callback and a nullish receiver are both rejected.
///
/// The receiver is checked first, so `Array.prototype.forEach.call(null, 1)`
/// reports the conversion failure rather than the callable failure.
#[test]
fn a_bad_callback_or_receiver_is_rejected() {
    assert_throws(
        "return [1].forEach(1);",
        ExceptionKind::TypeError,
        "not a function",
    );
    assert_throws(
        "return [1].forEach();",
        ExceptionKind::TypeError,
        "not a function",
    );
    assert_throws(
        "return [1].map();",
        ExceptionKind::TypeError,
        "not a function",
    );
    assert_throws(
        "return Array.prototype.forEach.call(null,function(){});",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
    assert_throws(
        "return Array.prototype.map.call(undefined,function(){});",
        ExceptionKind::TypeError,
        "cannot convert to object",
    );
}
