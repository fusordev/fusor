//! `Array.from` and `Array.of` generic factory semantics.

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExecutionError, ExecutionLimits, Function, JsString,
    JsValue, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource, Runtime,
    RuntimeLimits,
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
        with_dynamic_function_source(
            dynamic_source,
            FrontendLimits::default(),
            |unit, _prepared| {
                let context = CompilationContext::new_with_source_name(
                    unit,
                    Arc::from("<runtime Array statics>"),
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

fn assert_all(cases: &[(&str, &str)]) {
    for (expression, expected) in cases {
        assert_eq!(rendered(expression), *expected, "{expression}");
    }
}

#[test]
fn array_factory_identities_descriptors_and_default_results_are_exact() {
    assert_all(&[
        (
            "Object.getOwnPropertyNames(Array).join(',')",
            "length,name,isArray,from,of,prototype",
        ),
        ("Array.from.length+'|'+Array.from.name", "1|from"),
        ("Array.of.length+'|'+Array.of.name", "0|of"),
        (
            "(function(){let count=0;for(const f of [Array.from,Array.of]){try{Reflect.construct(f,[])}catch(e){if(e instanceof TypeError)count++}}return count})()",
            "2",
        ),
        (
            "(function(){const d=Object.getOwnPropertyDescriptor(Array,'from');return d.writable+'|'+d.enumerable+'|'+d.configurable})()",
            "true|false|true",
        ),
        (
            "JSON.stringify(Array.of(1,'x',undefined))",
            "[1,\"x\",null]",
        ),
        ("JSON.stringify(Array.from('A😀'))", "[\"A\",\"😀\"]"),
        (
            "(function(){const a=Array.from({0:'x',2:'z',length:3});return a.length+'|'+a[0]+'|'+Object.hasOwn(a,1)+'|'+a[1]+'|'+a[2]})()",
            "3|x|true|undefined|z",
        ),
    ]);
}

#[test]
fn array_of_is_generic_and_uses_create_data_property_before_strict_length_set() {
    assert_all(&[
        (
            "(function(){function C(n){this.requested=n;Object.defineProperty(this,'length',{set(v){this.final=v},configurable:true})}const r=Array.of.call(C,'a','b');return (r instanceof C)+'|'+r.requested+'|'+r[0]+r[1]+'|'+r.final})()",
            "true|2|ab|2",
        ),
        (
            "(function(){const r=Array.of.call({},1,2);return Array.isArray(r)+'|'+r.length+'|'+r.join(',')})()",
            "true|2|1,2",
        ),
        (
            "(function(){function C(){return Object.preventExtensions({})}try{Array.of.call(C,1);return 'miss'}catch(e){return e instanceof TypeError}})()",
            "true",
        ),
    ]);
}

#[test]
fn array_from_observes_mapper_constructor_and_iterable_order() {
    assert_all(&[
        (
            "(function(){let log=[];const items={get [Symbol.iterator](){log.push('iterator');return function(){log.push('call');return [1,2][Symbol.iterator]()}}};function C(n){log.push('ctor:'+n);Object.defineProperty(this,'length',{set(v){log.push('length:'+v)},configurable:true})}const receiver={};const r=Array.from.call(C,items,function(v,k){log.push('map:'+v+':'+k+':'+(this===receiver));return v*2},receiver);return log.join('|')+'#'+r[0]+','+r[1]})()",
            "iterator|ctor:undefined|call|map:1:0:true|map:2:1:true|length:2#2,4",
        ),
        (
            "(function(){let log=[];const items={get [Symbol.iterator](){log.push('iterator');return function(){log.push('call');return [1][Symbol.iterator]()}}};try{Array.from(items,1)}catch(e){return (e instanceof TypeError)+'|'+log.join(',')}})()",
            "true|",
        ),
    ]);
}

#[test]
fn array_from_array_like_path_converts_length_then_constructs_and_reads_in_order() {
    assert_all(&[(
        "(function(){let log=[];const items={get [Symbol.iterator](){log.push('probe');return undefined},get length(){log.push('length');return {valueOf(){log.push('convert');return 2}}},get 0(){log.push('get0');return 'x'},get 1(){log.push('get1');return 'y'}};function C(n){log.push('ctor:'+n);Object.defineProperty(this,'length',{set(v){log.push('set:'+v)},configurable:true})}const r=Array.from.call(C,items,function(v,k){log.push('map'+k);return v+k});return log.join('|')+'#'+r[0]+','+r[1]})()",
        "probe|length|convert|ctor:2|get0|map0|get1|map1|set:2#x0,y1",
    )]);
}

#[test]
fn array_from_closes_only_mapper_and_definition_abruptions() {
    assert_all(&[
        (
            "(function(){let closed=0;const items={[Symbol.iterator](){let sent=false;return {next(){if(sent)return {done:true};sent=true;return {done:false,value:1}},return(){closed++;throw 'close'}}}};let caught;try{Array.from(items,function(){throw 'mapper'})}catch(e){caught=e}return caught+'|'+closed})()",
            "mapper|1",
        ),
        (
            "(function(){let closed=0;const items={[Symbol.iterator](){let sent=false;return {next(){if(sent)return {done:true};sent=true;return {done:false,value:1}},return(){closed++;return {done:true}}}}};function C(){return Object.preventExtensions({})}let typed=false;try{Array.from.call(C,items)}catch(e){typed=e instanceof TypeError}return typed+'|'+closed})()",
            "true|1",
        ),
        (
            "(function(){let closed=0;const items={[Symbol.iterator](){return {next(){throw 'next'},return(){closed++}}}};let caught;try{Array.from(items)}catch(e){caught=e}return caught+'|'+closed})()",
            "next|0",
        ),
        (
            "(function(){let closed=0;const items={[Symbol.iterator](){return {next(){return {done:false,get value(){throw 'value'}}},return(){closed++}}}};let caught;try{Array.from(items)}catch(e){caught=e}return caught+'|'+closed})()",
            "value|0",
        ),
        (
            "(function(){let closed=0;const items={[Symbol.iterator](){return {next(){return {done:true}},return(){closed++}}}};function C(){return Object.defineProperty({},'length',{value:0,writable:false})}let typed=false;try{Array.from.call(C,items)}catch(e){typed=e instanceof TypeError}return typed+'|'+closed})()",
            "true|0",
        ),
    ]);
}

#[test]
fn array_from_array_like_scans_consume_shared_instruction_fuel() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, "return Array.from({length:200});");

    let result = context.call(
        &run,
        &[],
        ExecutionLimits::default().with_instruction_fuel(100),
    );

    assert!(matches!(
        result,
        Err(ExecutionError::InstructionLimitExceeded { limit: 100, .. })
    ));
}
