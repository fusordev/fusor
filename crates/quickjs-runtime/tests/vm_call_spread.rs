use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExecutionLimits, Function, JsNumber, JsString,
    OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource, Runtime, RuntimeLimits,
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
        let body = source.body().to_utf8_lossy().map_err(engine_failure)?;
        let dynamic_source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &[],
            SourceFragment::new(&body),
        );
        with_dynamic_function_source(dynamic_source, FrontendLimits::default(), |unit, _| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("<runtime call spread>"))
                    .map_err(engine_failure)?;
            context
                .compile_dynamic_function_script(VerificationLimits::default())
                .map(|tree| Arc::new(tree.verified_bytecode().clone()))
                .map_err(engine_failure)
        })
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

fn assert_number(value: &quickjs_runtime::JsValue, expected: i32) {
    let actual = value.as_number().expect("live value").expect("number");
    assert!(actual.strict_equals(JsNumber::from_i32(expected)));
}

#[test]
fn call_spread_routes_an_array_of_arguments_in_order() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            function f(a,b,c){return a*100+b*10+c;}\
            return f(...[10,20,30]);",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("call spread result");
    assert_number(&result, 1230);
}

#[test]
fn call_spread_mixes_dense_and_spread_arguments_left_to_right() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            function f(a,b,c,d){return a*1000+b*100+c*10+d;}\
            return f(1,...[2,3],4);",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("mixed spread result");
    assert_number(&result, 1234);
}

#[test]
fn call_spread_with_an_empty_array_passes_no_arguments() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let count=0;\
            function f(){count++;return count;}\
            return f(...[]);",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("empty spread result");
    assert_number(&result, 1);
}

#[test]
fn call_spread_reads_a_custom_iterator_instead_of_indexes() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            function f(a,b){return a*10+b;}\
            let iterable={\
                [Symbol.iterator](){\
                    let index=0;\
                    return {next(){index++;return index===1?{done:false,value:7}:(index===2?{done:false,value:8}:{done:true});}};\
                }\
            };\
            return f(...iterable);",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("iterator spread result");
    assert_number(&result, 78);
}

#[test]
fn call_spread_preserves_the_member_receiver() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let holder={tag:'held',collect:function collect(a,b){return this.tag+a+b;}};\
            return holder.collect(...['x','y']);",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("member spread result");
    assert_eq!(
        result
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "heldxy"
    );
}

#[test]
fn new_with_spread_constructs_through_the_spread_arguments() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            function Point(x,y){this.x=x;this.y=y;}\
            let p=new Point(...[3,4]);\
            return p.x*10+p.y;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("new spread result");
    assert_number(&result, 34);
}

#[test]
fn call_spread_closes_an_abrupt_iterator_and_preserves_the_error() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let closed=false;\
            function f(){};\
            let iterable={\
                [Symbol.iterator](){\
                    return {next(){throw new Error('boom');},return(){closed=true;return {done:true};}};\
                }\
            };\
            try{f(...iterable);}catch(e){return e.message==='boom'&&closed;}",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("abrupt spread");
    assert_eq!(result.as_boolean().expect("live value"), Some(true));
}

#[test]
fn call_spread_targets_the_callee_realm_for_noncallable_errors() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let message='';\
            try{(42)(...[1]);}catch(e){message=e.message;}\
            return message;",
    );
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("noncallable spread");
    assert_eq!(
        result
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "not a function"
    );
}
