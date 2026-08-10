//! `Array.prototype.join` and `Array.prototype.toString`, pinned to the
//! `QuickJS` 2026-06-04 oracle.
//!
//! Oracle transcript for the behaviors asserted here:
//!
//! ```text
//! []+"" => []
//! String([1,2]) => [1,2]
//! [1,[2,3]].toString => [1,2,3]
//! join default => [1,2,3]
//! join sep => [1-2-3]
//! join holes => [1--3]
//! join null undefined => [--1]
//! join sep undefined => [1,2]
//! join length 0 => []
//! join toString order => [oo]
//! toString length => [0]
//! join length => [1]
//! toString on nonarray => [[object Object]]
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
    Function, JsNumber, JsString, JsValue, OrdinaryDynamicFunctionCompiler,
    OrdinaryDynamicFunctionSource, Runtime, RuntimeLimits,
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
                let context =
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime join>"))
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

fn text(body: &str) -> String {
    evaluate(body, |result| {
        result
            .expect("completed")
            .as_string()
            .expect("live value")
            .expect("String")
            .to_utf8_lossy()
            .expect("UTF-8")
    })
}

fn assert_number(body: &str, expected: i32) {
    evaluate(body, |result| {
        let actual = result
            .expect("completed")
            .as_number()
            .expect("live value")
            .expect("Number");
        assert!(
            actual.strict_equals(JsNumber::from_i32(expected)),
            "{body} produced {actual:?}, expected {expected}"
        );
    });
}

fn type_error_message(body: &str) -> String {
    evaluate(body, |result| {
        let Err(ExecutionError::Exception(exception)) = result else {
            panic!("expected a JavaScript throw from {body}");
        };
        assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
        exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("UTF-8")
    })
}

/// Oracle: `join default => [1,2,3]` and `join sep => [1-2-3]`.
#[test]
fn join_uses_a_comma_by_default_and_the_supplied_separator_otherwise() {
    assert_eq!(text("return [1,2,3].join();"), "1,2,3");
    assert_eq!(text("return [1,2,3].join(\"-\");"), "1-2-3");
    assert_eq!(text("return [1,2,3].join(\"\");"), "123");
    // A multi-character separator is used verbatim.
    assert_eq!(text("return [1,2].join(\"<>\");"), "1<>2");
}

#[test]
fn joining_many_primitive_elements_does_not_grow_the_host_stack() {
    assert_eq!(
        text("return new Array(4096).fill(1).join('').length.toString();"),
        "4096"
    );
}

/// Oracle: `join sep undefined => [1,2]`. An explicit `undefined` separator is
/// the default, not the string `"undefined"`.
#[test]
fn an_undefined_separator_uses_the_default_comma() {
    assert_eq!(text("return [1,2].join(undefined);"), "1,2");
}

/// Oracle: `join holes => [1--3]` and `join null undefined => [--1]`.
/// `null` and `undefined` elements contribute nothing.
#[test]
fn nullish_elements_and_holes_contribute_nothing() {
    assert_eq!(text("return [1,,3].join(\"-\");"), "1--3");
    assert_eq!(text("return [null,undefined,1].join(\"-\");"), "--1");
    assert_eq!(text("return [null,null].join(\"-\");"), "-");
}

/// Oracle: `join length 0 => []`.
#[test]
fn joining_an_empty_array_produces_the_empty_string() {
    assert_eq!(text("return [].join(\"-\");"), "");
    assert_eq!(text("return [].join();"), "");
}

/// Oracle: `String([1,2]) => [1,2]`, `[]+"" => []`, and
/// `[1,[2,3]].toString => [1,2,3]`.
///
/// This is the coercion path that was previously wrong: without
/// `Array.prototype.toString`, `ToPrimitive` fell through to
/// `Object.prototype.toString` and produced `"[object Array]"`.
#[test]
fn array_to_string_drives_ordinary_string_coercion() {
    assert_eq!(text("return String([1,2]);"), "1,2");
    assert_eq!(text("return []+\"\";"), "");
    assert_eq!(text("return [1,2]+\"\";"), "1,2");
    assert_eq!(text("return [1,[2,3]].toString();"), "1,2,3");
    // A template literal would exercise the same coercion, but template
    // lowering is a separate milestone, so concatenation covers it here.
}

#[test]
fn array_to_string_calls_join_or_the_intrinsic_object_fallback() {
    assert_eq!(
        text(
            "var receiver={flag:'ok',join:function(){\
                 return this.flag+':'+arguments.length;\
             }};\
             return Array.prototype.toString.call(receiver);"
        ),
        "ok:0"
    );
    assert_eq!(
        text(
            "delete Object.prototype.toString;\
             return Array.prototype.toString.call({join:null});"
        ),
        "[object Object]"
    );
    assert_eq!(
        text(
            "return Array.prototype.toString.call(true)+'|'\
                 +Array.prototype.toString.call(false);"
        ),
        "[object Boolean]|[object Boolean]"
    );
    assert_eq!(
        text(
            "return Array.prototype.toString.call({\
                 join:0,[Symbol.toStringTag]:'Tagged'\
             });"
        ),
        "[object Tagged]"
    );
    assert_eq!(
        type_error_message(
            "return [{\
                 toString(){return {};},\
                 valueOf(){return {};}\
             }].toString();"
        ),
        "toPrimitive"
    );
}

/// Oracle: `join toString order => [oo]`. Each element's `toString` runs, in
/// index order, and its result is interpolated.
#[test]
fn join_runs_each_element_to_string_in_index_order() {
    assert_eq!(
        text(
            "var log=\"\";\
             function make(tag){return {toString(){log+=tag;return tag;}};}\
             var joined=[make(\"a\"),make(\"b\")].join(\"-\");\
             return log+\"|\"+joined;"
        ),
        "ab|a-b"
    );
}

/// A getter element runs during the join, and its returned value is used.
///
/// The accessor lives on an array-like object literal rather than on an array
/// index, because `Object.defineProperty` is a separate milestone; the join
/// path reads both through the same resumable element read.
#[test]
fn join_reads_accessor_elements_through_their_getters() {
    assert_eq!(
        text(
            "var log=\"\";\
             var source={length:2,get 0(){log+=\"g\";return \"v\";},get 1(){log+=\"h\";return \"w\";}};\
             var joined=Array.prototype.join.call(source,\"-\");\
             return log+\"|\"+joined;"
        ),
        "gh|v-w"
    );
}

/// Oracle: `toString length => [0]` and `join length => [1]`.
#[test]
fn join_and_to_string_report_the_pinned_arities() {
    assert_number("return Array.prototype.join.length;", 1);
    assert_number("return Array.prototype.toString.length;", 0);
}

/// `Array.prototype.join` is generic over array-like receivers and reads
/// `length` with `LengthOfArrayLike`.
#[test]
fn join_is_generic_over_array_like_receivers() {
    assert_eq!(
        text("return Array.prototype.join.call({length:2,0:\"a\",1:\"b\"},\"-\");"),
        "a-b"
    );
    // A receiver with no `length` has zero elements.
    assert_eq!(text("return Array.prototype.join.call({},\"-\");"), "");
    // The length is read with `ToLength`, so a fractional value truncates.
    assert_eq!(
        text("return Array.prototype.join.call({length:2.9,0:\"a\",1:\"b\",2:\"c\"},\"-\");"),
        "a-b"
    );
}

/// `join` uses the Proxy `[[Get]]` path for the snapshotted length and every
/// element rather than bypassing the handler through ordinary storage reads.
#[test]
fn join_uses_proxy_get() {
    assert_eq!(
        text(
            "var log='';\
             var target={length:2,0:'a',1:'b'};\
             var proxy=new Proxy(target,{get:function(t,k){log+='g'+k+';';return t[k];}});\
             var result=Array.prototype.join.call(proxy,'-');\
             return log+'|'+result;"
        ),
        "glength;g0;g1;|a-b"
    );
}

/// `join` reads its `length` once, before any element, so mutating `length`
/// from an element getter cannot change the iteration count.
#[test]
fn join_reads_its_length_once_before_the_element_loop() {
    assert_eq!(
        text(
            "var source={length:2,get 0(){source.length=5;return \"x\";}};\
             return Array.prototype.join.call(source,\"-\");"
        ),
        "x-"
    );
}

/// `LengthOfArrayLike` snapshots the iteration count before separator
/// conversion, even when that conversion mutates the receiver's length.
#[test]
fn join_snapshots_length_before_converting_the_separator() {
    assert_eq!(
        text(
            "var log='';\
             var source={get length(){log+='l';return 2;},0:'a',1:'b'};\
             var separator={toString(){log+='s';return '-';}};\
             var joined=Array.prototype.join.call(source,separator);\
             return log+'|'+joined;"
        ),
        "ls|a-b"
    );
    assert_eq!(
        text(
            "var source={length:2,0:'a',1:'b',2:'c'};\
             var separator={toString(){source.length=3;return '-';}};\
             return Array.prototype.join.call(source,separator);"
        ),
        "a-b"
    );
    assert_eq!(
        text(
            "var source={length:2,0:'a',1:'b'};\
             var separator={toString(){source.length=1;return '-';}};\
             return Array.prototype.join.call(source,separator);"
        ),
        "a-b"
    );
}

/// A nullish receiver fails the initial `ToObject`.
#[test]
fn join_rejects_a_nullish_receiver() {
    assert_eq!(
        type_error_message("return Array.prototype.join.call(null,\"-\");"),
        "cannot convert to object"
    );
    assert_eq!(
        type_error_message("return Array.prototype.join.call(undefined,\"-\");"),
        "cannot convert to object"
    );
}

/// The separator is converted with `ToString`, so a non-string separator runs
/// its own `toString` exactly once.
#[test]
fn the_separator_is_converted_once_with_to_string() {
    assert_eq!(
        text(
            "var log=\"\";\
             var sep={toString(){log+=\"s\";return \"|\";}};\
             var joined=[1,2,3].join(sep);\
             return log+\"/\"+joined;"
        ),
        "s/1|2|3"
    );
    // A numeric separator stringifies.
    assert_eq!(text("return [1,2].join(0);"), "102");
}
