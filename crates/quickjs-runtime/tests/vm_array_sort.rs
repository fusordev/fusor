//! `Array.prototype.sort`, `toSorted`, and `SortIndexedProperties` semantics.
//!
//! The pinned `QuickJS` 2026-06-04 oracle establishes the expected default
//! lexical ordering, stable comparator ties, hole placement, generic receiver
//! behavior, and built-in identities exercised below.

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
                    Arc::from("<runtime Array sorting>"),
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

fn assert_all(cases: &[(&str, &str)]) {
    for (expression, expected) in cases {
        assert_eq!(rendered(expression), *expected, "{expression}");
    }
}

#[test]
fn sort_is_stable_and_defaults_to_utf16_lexical_order() {
    assert_all(&[
        ("[10,2,1].sort().join()", "1,10,2"),
        ("[3,undefined,1,2].sort().join()", "1,2,3,"),
        (
            "[{k:1,i:'a'},{k:1,i:'b'},{k:0,i:'c'}].sort(function(a,b){return a.k-b.k;}).map(function(v){return v.i;}).join('')",
            "cab",
        ),
        ("[3,2,1].sort(function(){return NaN;}).join()", "3,2,1"),
        (
            "[3,1,2].sort(function(a,b){return String(a-b);}).join()",
            "1,2,3",
        ),
        (
            "(function(){let seen=false;[undefined,2,1].sort(function(a,b){seen=seen||a===undefined||b===undefined;return a-b;});return seen;})()",
            "false",
        ),
    ]);
}

#[test]
fn sort_skips_holes_and_deletes_the_trailing_indices() {
    assert_all(&[
        (
            "(function(){\
                const a=[,undefined,2,,1];a.sort();\
                return a.join()+'|'+[0,1,2,3,4].map(function(i){return Object.prototype.hasOwnProperty.call(a,i);}).join('');\
            })()",
            "1,2,,,|truetruetruefalsefalse",
        ),
        (
            "(function(){\
                const o={length:4,0:'b',2:'a'};\
                const r=Array.prototype.sort.call(o);\
                return (r===o)+'|'+o[0]+'|'+o[1]+'|'\
                    +Object.prototype.hasOwnProperty.call(o,2)+'|'\
                    +Object.prototype.hasOwnProperty.call(o,3);\
            })()",
            "true|a|b|false|false",
        ),
        (
            "(function(){\
                const p={1:'a'};const o=Object.create(p);o.length=3;o[0]='b';\
                Array.prototype.sort.call(o);\
                return o[0]+'|'+o[1]+'|'\
                    +Object.prototype.hasOwnProperty.call(o,1)+'|'\
                    +Object.prototype.hasOwnProperty.call(o,2);\
            })()",
            "a|b|true|false",
        ),
    ]);
}

/// `toSorted` calls `SortIndexedProperties` with read-through-holes, so it
/// collects a Proxy's entries with `Get` directly after the single length read.
#[test]
fn to_sorted_uses_proxy_internal_methods_while_collecting() {
    assert_all(&[(
        "(function(){\
            let log='';\
            const target={length:2,0:'b',1:'a'};\
            const proxy=new Proxy(target,{\
                get:function(t,k){log+='g'+k+';';return t[k];},\
                has:function(t,k){log+='h'+k+';';return k in t;}\
            });\
            const result=Array.prototype.toSorted.call(proxy);\
            return log+'|'+result.join();\
        })()",
        "glength;g0;g1;|a,b",
    )]);
}

#[test]
fn to_sorted_returns_a_fresh_dense_array() {
    assert_all(&[
        (
            "(function(){const a=[3,1,2];const r=a.toSorted();return a.join()+'|'+r.join()+'|'+(r===a)+'|'+Array.isArray(r);})()",
            "3,1,2|1,2,3|false|true",
        ),
        (
            "(function(){\
                const a=[,2,,1];const r=a.toSorted();\
                return r.join()+'|'+[0,1,2,3].map(function(i){return Object.prototype.hasOwnProperty.call(r,i);}).join('');\
            })()",
            "1,2,,|truetruetruetrue",
        ),
        (
            "Array.prototype.toSorted.call({length:3,0:'b',2:'a'}).join()",
            "a,b,",
        ),
    ]);
}

#[test]
fn comparator_validation_and_conversions_follow_specification_order() {
    assert_all(&[
        // Comparator validation precedes both `ToObject(this)` and `length`.
        (
            "(function(){let log='';const o={get length(){log+='length';return 2;}};try{Array.prototype.sort.call(o,1);}catch(error){}return log;})()",
            "",
        ),
        (
            "(function(){try{Array.prototype.sort.call(null,1);}catch(error){return error.message;}})()",
            "not a function",
        ),
        (
            "(function(){\
                let log='';\
                const o={\
                    get length(){log+='length|';return {valueOf(){log+='lengthValue|';return 2;}};},\
                    get 0(){log+='get0|';return 2;},get 1(){log+='get1|';return 1;},\
                    set 0(v){log+='set0:'+v+'|';},set 1(v){log+='set1:'+v;}\
                };\
                Array.prototype.sort.call(o,function(a,b){\
                    'use strict';log+='compare:'+a+','+b+','+(this===undefined)+'|';\
                    return {valueOf(){log+='resultValue|';return a-b;}};\
                });\
                return log;\
            })()",
            "length|lengthValue|get0|get1|compare:2,1,true|resultValue|set0:1|set1:2",
        ),
        // Default comparison performs and caches left ToString before right.
        (
            "(function(){\
                let log='';\
                const b={toString(){log+='b|';return 'b';}};\
                const a={toString(){log+='a';return 'a';}};\
                [b,a].sort();return log;\
            })()",
            "b|a",
        ),
    ]);
}

#[test]
fn abrupt_sort_publication_preserves_completed_writes() {
    assert_throws(
        "const o={length:2};\
         Object.defineProperty(o,1,{value:'locked',configurable:false,writable:true});\
         return Array.prototype.sort.call(o);",
        ExceptionKind::TypeError,
        "could not delete property",
    );
    assert_all(&[
        (
            "(function(){\
            let log='';\
            const o={length:2,0:2,1:1};\
            Object.defineProperty(o,0,{get(){return 2;},set(v){log+='set0:'+v;},configurable:true});\
            Object.defineProperty(o,1,{get(){return 1;},set(v){throw new Error('stop');},configurable:true});\
            try{Array.prototype.sort.call(o,function(a,b){return a-b;});}catch(error){}\
            return log;\
        })()",
            "set0:1",
        ),
        (
            "(function(){\
            let writes=0;const o={length:2};\
            Object.defineProperty(o,0,{get(){return 2;},set(v){writes=writes+1;},configurable:true});\
            Object.defineProperty(o,1,{get(){return 1;},set(v){writes=writes+1;},configurable:true});\
            try{Array.prototype.sort.call(o,function(){throw new Error('compare');});}catch(error){}\
            return writes;\
        })()",
            "0",
        ),
    ]);
    assert_throws(
        "return [Symbol('x'),1].sort();",
        ExceptionKind::TypeError,
        "cannot convert symbol to string",
    );
    assert_throws(
        "return [2,1].sort(function(){return Symbol('x');});",
        ExceptionKind::TypeError,
        "cannot convert symbol to number",
    );
}

#[test]
fn to_sorted_rejects_an_unrepresentable_result_before_element_reads() {
    assert_all(&[(
        "(function(){\
            let log='';const o={length:4294967296,get 0(){log+='get';return 1;}};\
            try{Array.prototype.toSorted.call(o);}catch(error){return error.name+'|'+error.message+'|'+log;}\
        })()",
        "RangeError|invalid array length|",
    )]);
}

#[test]
fn sorting_methods_box_primitives_reject_nullish_receivers_and_have_exact_shape() {
    assert_all(&[
        (
            "Object.prototype.toString.call(Array.prototype.sort.call(3))",
            "[object Number]",
        ),
        ("Array.prototype.sort.length", "1"),
        ("Array.prototype.toSorted.length", "1"),
        ("Array.prototype.sort.name", "sort"),
        ("Array.prototype.toSorted.name", "toSorted"),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'sort').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'sort').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'sort').configurable",
            "true",
        ),
        (
            "Object.prototype.hasOwnProperty.call(Array.prototype.sort,'prototype')",
            "false",
        ),
        (
            "(function(){try{new Array.prototype.toSorted();}catch(error){return error instanceof TypeError;}})()",
            "true",
        ),
    ]);
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

#[test]
fn sort_collection_and_merge_work_consume_shared_instruction_fuel() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let scan = dynamic_function(
        &mut context,
        "return Array.prototype.sort.call({length:1000});",
    );
    let result = context.call(
        &scan,
        &[],
        ExecutionLimits::default().with_instruction_fuel(100),
    );
    assert!(matches!(
        result,
        Err(ExecutionError::InstructionLimitExceeded { limit: 100, .. })
    ));
}
