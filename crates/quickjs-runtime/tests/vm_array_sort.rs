//! `Array.prototype.sort` and `toSorted`.
//!
//! Every expectation below was produced by the pinned oracle:
//!
//! ```console
//! $ /private/tmp/quickjs-2026-06-04/qjs -e 'const a=[3,,1]; a.sort();\
//!     console.log(a.join("|"), a.length, "2" in a);'
//! 1|3| 3 false
//! ```
//!
//! The comparison falls back to each element's original position, so the sort
//! is stable. The number and order of comparisons is implementation-defined by
//! ECMAScript and therefore not asserted; the final permutation is pinned.
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! [10,9,1].sort() => "1,10,9" (default compares UTF-16 strings)
//! [3,undefined,1,undefined].sort() => "1,3,," (undefined moves to the end)
//! [3,,1].sort() => "1,3", length 3, index 2 absent (holes are deleted)
//! [2,1,10].sort(undefined) => "1,10,2"
//! [10,9,1].sort((a,b)=>a-b) => "1,9,10"
//! [1,1,2].sort((a,b)=>NaN) => unchanged (NaN means 0)
//! [2,1].sort((a,b)=>({valueOf(){return -1}})) => "2,1" (result converts)
//! [2,1].sort((a,b)=>1n) !! TypeError: cannot convert bigint to number
//! [2,1].sort((a,b)=>Symbol()) !! TypeError: cannot convert symbol to number
//! [5,5,5,5].sort(throwing) => no call, no throw (bitwise-identical pairs skip)
//! comparator throws => array unmodified
//! {0:"b",2:nc"a",length:3}.sort() !! TypeError: could not delete property
//! [1].sort(5) !! TypeError: not a function
//! sort.call(null, 5) !! TypeError: not a function (checked before ToObject)
//! sort.call(null) !! TypeError: cannot convert to object
//! [3,1,2].toSorted() => "1,2,3", receiver unchanged, fresh real Array
//! [1,,3].toSorted() => "1,3,", every index present (dense)
//! write-back: unmoved elements are not Set; writes ascend; undefined tail Sets
//! lengths: sort 1, toSorted 1
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
                    Arc::from("<runtime Array sort>"),
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

/// The default comparison converts with `ToString` and compares UTF-16 code
/// units, so numbers sort lexicographically.
#[test]
fn the_default_sort_compares_strings() {
    assert_all(&[
        ("[10,9,1].sort().join()", "1,10,9"),
        ("[2,1,10].sort(undefined).join()", "1,10,2"),
        ("['b','a','c'].sort().join()", "a,b,c"),
        // The receiver itself is returned.
        ("(function(){const a=[2,1];return a.sort()===a;})()", "true"),
    ]);
}

/// `undefined` elements never reach the comparator and move to the end.
#[test]
fn undefined_elements_move_to_the_end() {
    assert_all(&[
        ("[3,undefined,1,undefined].sort().join('|')", "1|3||"),
        (
            "[3,undefined,1].sort(function(a,b){return a-b;}).join('|')",
            "1|3|",
        ),
    ]);
}

/// Holes are skipped during collection and deleted at the tail.
#[test]
fn holes_stay_holes() {
    assert_all(&[
        (
            "(function(){\
                const a=[3,,1];\
                a.sort();\
                return a.join()+'|'+a.length+'|'+Object.prototype.hasOwnProperty.call(a,2);\
            })()",
            "1,3,|3|false",
        ),
        // An array-like's holes are deleted the same way.
        (
            "(function(){\
                const o={length:3,0:'c',2:'a'};\
                Array.prototype.sort.call(o);\
                return o[0]+'|'+o[1]+'|'+Object.prototype.hasOwnProperty.call(o,2)+'|'+o.length;\
            })()",
            "a|c|false|3",
        ),
    ]);
}

/// A user comparator drives the ordering; the sort is stable.
#[test]
fn a_user_comparator_drives_a_stable_ordering() {
    assert_all(&[
        ("[10,9,1].sort(function(a,b){return a-b;}).join()", "1,9,10"),
        ("[2,1].sort(function(a,b){return b-a;}).join()", "2,1"),
        // Equal keys keep their original order.
        (
            "(function(){\
                const u=[{k:2,id:'a'},{k:1,id:'b'},{k:2,id:'c'},{k:1,id:'d'}];\
                u.sort(function(x,y){return x.k-y.k;});\
                return u.map(function(x){return x.id;}).join('');\
            })()",
            "bdac",
        ),
        // A `NaN` result means `0`, so nothing moves.
        ("[1,1,2].sort(function(){return NaN;}).join()", "1,1,2"),
        // A non-Number result converts with `ToNumber`.
        (
            "[2,1].sort(function(){return {valueOf(){return -1;}};}).join()",
            "2,1",
        ),
        ("[1,2,3].sort(function(){return Infinity;}).join()", "3,2,1"),
    ]);
}

/// A pair whose values share one bit pattern skips the comparator call,
/// which is observable: a throwing comparator on `[5,5,5,5]` is never invoked
/// (`quickjs.c:43151-43153`).
#[test]
fn bitwise_identical_pairs_skip_the_comparator() {
    assert_all(&[
        (
            "(function(){\
                let calls=0;\
                [5,5,5,5].sort(function(){calls++;return 0;});\
                return calls;\
            })()",
            "0",
        ),
        // A throwing comparator is never reached for identical pairs, but one
        // differing pair is compared.
        (
            "(function(){\
                let calls=0;\
                [5,5,4].sort(function(a,b){calls++;return a-b;});\
                return calls>0;\
            })()",
            "true",
        ),
    ]);
}

/// A comparator that throws aborts before the write-back, leaving the array
/// unmodified.
#[test]
fn a_throwing_comparator_leaves_the_array_unmodified() {
    assert_all(&[(
        "(function(){\
            const a=[2,1];\
            try { a.sort(function(){throw 1;}); } catch (e) {}\
            return a.join();\
        })()",
        "2,1",
    )]);
}

/// The comparator result's conversion keeps the numeric domains apart.
#[test]
fn the_comparator_result_respects_the_numeric_domains() {
    assert_throws(
        "return [2,1].sort(function(){return 1n;});",
        ExceptionKind::TypeError,
        "cannot convert bigint to number",
    );
    assert_throws(
        "return [2,1].sort(function(){return Symbol();});",
        ExceptionKind::TypeError,
        "cannot convert symbol to number",
    );
}

/// Every element read during collection can enter a getter, ascending.
#[test]
fn collection_reads_ascend_through_accessors() {
    assert_all(&[
        (
            "(function(){\
                let log='';\
                const o={length:2};\
                Object.defineProperty(o,0,{get(){log+='g0|';return 'b';},set(v){},configurable:true});\
                Object.defineProperty(o,1,{get(){log+='g1|';return 'a';},set(v){},configurable:true});\
                Array.prototype.sort.call(o);\
                return log+'|'+o[0]+o[1];\
            })()",
            "g0|g1||ba",
        ),
        // The length is read once, before any element.
        (
            "(function(){\
                let log='';\
                const o={get length(){log+='len|';return 1;}};\
                Object.defineProperty(o,0,{get(){log+='g0|';return 'a';},configurable:true});\
                Array.prototype.sort.call(o);\
                return log;\
            })()",
            "len|g0|",
        ),
    ]);
}

/// The write-back writes ascending, skips elements that did not move, and
/// writes the `undefined` tail through `Set` (`quickjs.c:43245-43267`).
#[test]
fn the_write_back_order_matches_the_oracle() {
    assert_all(&[
        // An element that did not move is not `Set`: the setter at index 1 is
        // never called.
        (
            "(function(){\
                let log='';\
                const o={length:3};\
                Object.defineProperty(o,0,{get(){return 1;},set(v){log+='s0:'+v+'|';},configurable:true});\
                Object.defineProperty(o,1,{get(){return 2;},set(v){log+='s1:'+v+'|';},configurable:true});\
                Object.defineProperty(o,2,{get(){return 3;},set(v){log+='s2:'+v+'|';},configurable:true});\
                Array.prototype.sort.call(o,function(a,b){return a-b;});\
                return log==='';\
            })()",
            "true",
        ),
        // Moved elements are written in ascending order.
        (
            "(function(){\
                let log='';\
                const o={length:2};\
                Object.defineProperty(o,0,{get(){return 'b';},set(v){log+='s0:'+v+'|';},configurable:true});\
                Object.defineProperty(o,1,{get(){return 'a';},set(v){log+='s1:'+v+'|';},configurable:true});\
                Array.prototype.sort.call(o);\
                return log+'|'+o[0]+o[1];\
            })()",
            "s0:a|s1:b||ba",
        ),
        // The `undefined` tail is written through `Set` after the sorted
        // values.
        (
            "(function(){\
                let log='';\
                const o={length:3};\
                Object.defineProperty(o,0,{get(){return 2;},set(v){log+='s0:'+v+'|';},configurable:true});\
                Object.defineProperty(o,1,{get(){return 1;},set(v){log+='s1:'+v+'|';},configurable:true});\
                Object.defineProperty(o,2,{get(){return undefined;},set(v){log+='s2:'+v+'|';},configurable:true});\
                Array.prototype.sort.call(o,function(a,b){\
                    return a===undefined?1:b===undefined?-1:a-b;\
                });\
                return log+'|'+o[0]+','+o[1]+','+o[2];\
            })()",
            "s0:1|s1:2|s2:undefined||2,1,undefined",
        ),
    ]);
}

/// A refused tail delete reports `could not delete property`.
#[test]
fn a_refused_tail_delete_is_rejected() {
    assert_throws(
        "(function(){\
            const o={0:'b',length:3};\
            Object.defineProperty(o,2,{value:'a',configurable:false,writable:true,enumerable:true});\
            Array.prototype.sort.call(o);\
        })()",
        ExceptionKind::TypeError,
        "could not delete property",
    );
}

/// A non-callable comparator is rejected before the receiver is coerced.
#[test]
fn a_non_callable_comparator_is_rejected_first() {
    assert_throws(
        "return [1].sort(5);",
        ExceptionKind::TypeError,
        "not a function",
    );
    assert_throws(
        "return Array.prototype.sort.call(null,5);",
        ExceptionKind::TypeError,
        "not a function",
    );
    assert_throws(
        "return [1].toSorted(5);",
        ExceptionKind::TypeError,
        "not a function",
    );
}

/// `toSorted` answers a fresh dense Array without mutating the receiver.
#[test]
fn to_sorted_answers_a_dense_sorted_copy() {
    assert_all(&[
        (
            "(function(){\
                const a=[3,1,2];\
                const b=a.toSorted();\
                return a.join()+'|'+b.join()+'|'+Array.isArray(b)+'|'+(a!==b);\
            })()",
            "3,1,2|1,2,3|true|true",
        ),
        (
            "[3,1,2].toSorted(function(a,b){return b-a;}).join()",
            "3,2,1",
        ),
        // Holes become present `undefined` elements that sort to the end.
        (
            "(function(){\
                const r=[1,,3].toSorted();\
                return r.join()+'|'+Object.prototype.hasOwnProperty.call(r,2)+'|'+r.length;\
            })()",
            "1,3,|true|3",
        ),
        // An array-like receiver answers a real Array.
        (
            "(function(){\
                const r=Array.prototype.toSorted.call({length:2,0:'b',1:'a'});\
                return r.join()+'|'+Array.isArray(r);\
            })()",
            "a,b|true",
        ),
        // A comparator on `toSorted` never writes the receiver.
        (
            "(function(){\
                const a=[2,1];\
                a.toSorted(function(x,y){return y-x;});\
                return a.join();\
            })()",
            "2,1",
        ),
    ]);
}

/// A nullish receiver is rejected after the comparator check.
#[test]
fn a_nullish_receiver_is_rejected() {
    for method in ["sort", "toSorted"] {
        for receiver in ["null", "undefined"] {
            assert_throws(
                &format!("return Array.prototype.{method}.call({receiver});"),
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
        ("Array.prototype.sort.length", "1"),
        ("Array.prototype.toSorted.length", "1"),
        ("Array.prototype.sort.name", "sort"),
        ("Array.prototype.toSorted.name", "toSorted"),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'sort').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'toSorted').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'sort').configurable",
            "true",
        ),
    ]);
}
