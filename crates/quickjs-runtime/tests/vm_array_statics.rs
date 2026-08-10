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

fn start_and_read(start_body: &str) -> String {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let start = dynamic_function(&mut context, start_body);
    let read = dynamic_function(&mut context, "return arguments[0].result;");
    let state = context
        .call(&start, &[], ExecutionLimits::default())
        .expect("Array.fromAsync setup");
    context
        .call(&read, &[state], ExecutionLimits::default())
        .expect("Array.fromAsync result read")
        .as_string()
        .expect("live result")
        .expect("String result")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
fn array_factory_identities_descriptors_and_default_results_are_exact() {
    assert_all(&[
        (
            "Object.getOwnPropertyNames(Array).join(',')",
            "length,name,isArray,from,fromAsync,of,prototype",
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
fn typed_array_factories_construct_typed_results_and_reject_immutable_destinations() {
    assert_all(&[
        (
            "(function(){const from=Uint8Array.from([1,2],function(v,k){return v+k});\
             const of=Uint8Array.of(3,4);return from.join(',')+'|'+of.join(',')+'|'+\
             Uint8Array.from.length+'|'+Uint8Array.of.length})()",
            "1,3|3,4|1|0",
        ),
        (
            "(function(){let log=[];function C(length){log.push('ctor:'+length);\
               return new Uint8Array((new ArrayBuffer(length)).transferToImmutable())}\
             const items={[Symbol.iterator](){let index=0;return {next(){log.push('next');\
               return index++<2?{value:index,done:false}:{done:true}}}}};\
             let kind='';try{Uint8Array.from.call(C,items,function(value){\
               log.push('map:'+value);return value})}catch(error){kind=error.name}\
             return kind+'|'+log.join(',')})()",
            "TypeError|next,next,next,ctor:2",
        ),
        (
            "(function(){let log=[];function C(length){log.push('ctor:'+length);\
               return new Uint8Array((new ArrayBuffer(length)).transferToImmutable())}\
             const value={valueOf(){log.push('value');return 1}};let kind='';\
             try{Uint8Array.of.call(C,value)}catch(error){kind=error.name}\
             return kind+'|'+log.join(',')})()",
            "TypeError|ctor:1",
        ),
    ]);
}

#[test]
fn array_from_async_identity_and_async_rejection_boundary_are_exact() {
    assert_all(&[
        (
            "Object.getOwnPropertyNames(Array).join(',')",
            "length,name,isArray,from,fromAsync,of,prototype",
        ),
        (
            "Array.fromAsync.length+'|'+Array.fromAsync.name",
            "1|fromAsync",
        ),
        (
            "(function(){try{Reflect.construct(Array.fromAsync,[]);return 'miss'}catch(e){return e instanceof TypeError}})()",
            "true",
        ),
    ]);
    assert_eq!(
        start_and_read(
            "let state={result:''};\
             const items={get [Symbol.asyncIterator](){state.result=state.result+'probe';return undefined}};\
             const promise=Array.fromAsync(items,1);\
             state.result='returned:'+(promise instanceof Promise)+'|'+state.result;\
             promise.catch(function(error){state.result=state.result+'|rejected:'+(error instanceof TypeError)});\
             return state;"
        ),
        "returned:true||rejected:true"
    );
}

#[test]
fn array_from_async_prefers_async_iteration_and_awaits_results_in_spec_order() {
    assert_eq!(
        start_and_read(
            "let state={result:''};let log=[];let step=0;\
             const items={\
               get [Symbol.asyncIterator](){log.push('async-get');return function(){log.push('async-call');return this.iterator}},\
               get [Symbol.iterator](){log.push('sync-get');return function(){throw 'wrong'}},\
               iterator:{get next(){log.push('next-get');return function(){log.push('next');if(step++)return Promise.resolve({get done(){log.push('done:true');return true},get value(){log.push('value:wrong');throw 'wrong'}});return Promise.resolve({get done(){log.push('done:false');return false},get value(){log.push('value');return 3}})}}}\
             };\
             function C(n){log.push('ctor:'+n);Object.defineProperty(this,'length',{set(value){log.push('length:'+value)},configurable:true})}\
             const receiver={};\
             Array.fromAsync.call(C,items,function(value,index){log.push('map:'+value+':'+index+':'+(this===receiver));return Promise.resolve(value*2)},receiver).then(function(result){state.result=log.join('|')+'#'+result[0]});\
             return state;"
        ),
        "async-get|async-call|next-get|ctor:undefined|next|done:false|value|map:3:0:true|next|done:true|length:1#6"
    );
}

#[test]
fn array_from_async_sync_fallback_and_array_like_path_await_values() {
    assert_eq!(
        start_and_read(
            "let state={result:''};let log=[];\
             const sync={get [Symbol.asyncIterator](){log.push('async');return undefined},get [Symbol.iterator](){log.push('sync');return function(){return [Promise.resolve(2),3][Symbol.iterator]()}}};\
             Array.fromAsync(sync,function(value,index){log.push('map:'+value+':'+index);return Promise.resolve(value+index)}).then(function(result){state.result=log.join('|')+'#'+result.join(',')});\
             return state;"
        ),
        "async|sync|map:2:0|map:3:1#2,4"
    );
    assert_eq!(
        start_and_read(
            "let state={result:''};let log=[];\
             const items={get [Symbol.asyncIterator](){log.push('async');return undefined},get [Symbol.iterator](){log.push('sync');return undefined},get length(){log.push('length');return 2},get 0(){log.push('get0');return Promise.resolve('a')},get 1(){log.push('get1');return 'b'}};\
             function C(length){log.push('ctor:'+length);Object.defineProperty(this,'length',{set(value){log.push('set:'+value)},configurable:true})}\
             Array.fromAsync.call(C,items,function(value,index){log.push('map:'+value+':'+index);return Promise.resolve(value+index)}).then(function(result){state.result=log.join('|')+'#'+result[0]+','+result[1]});\
             return state;"
        ),
        "async|sync|length|ctor:2|get0|map:a:0|get1|map:b:1|set:2#a0,b1"
    );
}

#[test]
fn array_from_async_closes_mapper_and_definition_abruptions_only() {
    assert_eq!(
        start_and_read(
            "let state={result:''};let closed=0;\
             const items={[Symbol.asyncIterator](){let sent=false;return {next(){if(sent)return Promise.resolve({done:true});sent=true;return Promise.resolve({done:false,value:1})},return(){closed++;return Promise.resolve({done:true})}}}};\
             Array.fromAsync(items,function(){return Promise.reject('mapper')}).catch(function(error){state.result=error+'|'+closed});\
             return state;"
        ),
        "mapper|1"
    );
    assert_eq!(
        start_and_read(
            "let state={result:''};let closed=0;\
             const items={[Symbol.asyncIterator](){return {next(){return Promise.reject('next')},return(){closed++;return Promise.resolve({done:true})}}}};\
             Array.fromAsync(items).catch(function(error){state.result=error+'|'+closed});\
             return state;"
        ),
        "next|0"
    );
    assert_eq!(
        start_and_read(
            "let state={result:''};let closed=0;\
             const items={[Symbol.asyncIterator](){let sent=false;return {next(){if(sent)return Promise.resolve({done:true});sent=true;return Promise.resolve({done:false,value:1})},return(){closed++;return Promise.resolve({done:true})}}}};\
             function C(){return Object.preventExtensions({})}\
             Array.fromAsync.call(C,items).catch(function(error){state.result=(error instanceof TypeError)+'|'+closed});\
             return state;"
        ),
        "true|1"
    );
}

#[test]
fn array_from_async_rejects_synchronous_get_and_conversion_abruptions() {
    assert_eq!(
        start_and_read(
            "let state={result:''};\
             const items={get [Symbol.asyncIterator](){throw 'get'}};\
             let promise;try{promise=Array.fromAsync(items);state.result='returned:'+(promise instanceof Promise)}catch(error){state.result='threw:'+error}\
             if(promise)promise.catch(function(error){state.result=state.result+'|rejected:'+error});\
             return state;"
        ),
        "returned:true|rejected:get"
    );
    assert_eq!(
        start_and_read(
            "let state={result:''};\
             const items={get [Symbol.asyncIterator](){return undefined},get [Symbol.iterator](){return undefined},length:{valueOf(){throw 'length'}}};\
             let promise;try{promise=Array.fromAsync(items);state.result='returned:'+(promise instanceof Promise)}catch(error){state.result='threw:'+error}\
             if(promise)promise.catch(function(error){state.result=state.result+'|rejected:'+error});\
             return state;"
        ),
        "returned:true|rejected:length"
    );
}

#[test]
fn array_from_async_await_and_close_abruptions_preserve_exact_precedence() {
    assert_eq!(
        start_and_read(
            "let state={result:''};let closed=0;\
             const awaited=Promise.resolve({done:false,value:1});Object.defineProperty(awaited,'constructor',{get(){throw 'next-constructor'}});\
             const items={[Symbol.asyncIterator](){return {next(){return awaited},return(){closed++;return Promise.resolve({done:true})}}}};\
             Array.fromAsync(items).catch(function(error){state.result=error+'|'+closed});\
             return state;"
        ),
        "next-constructor|0"
    );
    assert_eq!(
        start_and_read(
            "let state={result:''};let closed=0;\
             const items={[Symbol.asyncIterator](){let sent=false;return {next(){if(sent)return Promise.resolve({done:true});sent=true;return Promise.resolve({done:false,value:1})},return(){closed++;return Promise.reject('close')}}}};\
             Array.fromAsync(items,function(){return Promise.reject('mapper')}).catch(function(error){state.result=error+'|'+closed});\
             return state;"
        ),
        "mapper|1"
    );
    assert_eq!(
        start_and_read(
            "let state={result:''};let closed=0;\
             const items={[Symbol.asyncIterator](){return {next(){return Promise.resolve({done:true})},return(){closed++}}}};\
             function C(){Object.defineProperty(this,'length',{set(){throw 'length'},configurable:true})}\
             Array.fromAsync.call(C,items).catch(function(error){state.result=error+'|'+closed});\
             return state;"
        ),
        "length|0"
    );
}

#[test]
fn suspended_array_from_async_survives_collection_with_all_iterator_roots() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let (resume, read, state) = {
        let mut context = runtime.context(&realm).expect("context");
        let start = dynamic_function(
            &mut context,
            "let state={result:'waiting'};let step=0;\
             const items={[Symbol.asyncIterator](){return {next(){if(step++)return Promise.resolve({done:true});return {then:function(resolve){state.resume=resolve}}}}}};\
             state.promise=Array.fromAsync(items,function(value){return value*2});\
             state.promise.then(function(result){state.result='done:'+result[0]+':'+result.length});\
             return state;",
        );
        let resume = dynamic_function(
            &mut context,
            "arguments[0].resume({done:false,value:4});return arguments[0];",
        );
        let read = dynamic_function(&mut context, "return arguments[0].result;");
        let state = context
            .call(&start, &[], ExecutionLimits::default())
            .expect("suspended Array.fromAsync");
        (resume, read, state)
    };
    runtime
        .collect_cycles()
        .expect("collect while Array.fromAsync is suspended");
    let mut context = runtime.context(&realm).expect("context");
    let state = context
        .call(&resume, &[state], ExecutionLimits::default())
        .expect("resume Array.fromAsync");
    let result = context
        .call(&read, &[state], ExecutionLimits::default())
        .expect("read completed Array.fromAsync")
        .as_string()
        .expect("live result")
        .expect("string result")
        .to_utf8_lossy()
        .expect("UTF-8");

    assert_eq!(result, "done:8:1");
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
        (
            "(function(){const marker={};function C(){return new Proxy({}, {defineProperty(){throw marker}})}try{Array.of.call(C,1);return 'miss'}catch(e){return e===marker}})()",
            "true",
        ),
        (
            "(function(){function C(){return new Proxy({}, {defineProperty(){return false}})}try{Array.of.call(C,1);return 'miss'}catch(e){return e instanceof TypeError}})()",
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
            "(function(){const marker={};let closed=0,calls=0;const items={[Symbol.iterator](){let sent=false;return {next(){if(sent)return {done:true};sent=true;return {done:false,value:1}},return(){closed++;return {done:true}}}}};function C(){return new Proxy({}, {defineProperty(){calls++;throw marker}})}let same=false;try{Array.from.call(C,items)}catch(e){same=e===marker}return same+'|'+closed+'|'+calls})()",
            "true|1|1",
        ),
        (
            "(function(){let calls=0;const items={length:1,0:'x',[Symbol.iterator]:undefined};function C(){return new Proxy({}, {defineProperty(){calls++;return false}})}let typed=false;try{Array.from.call(C,items)}catch(e){typed=e instanceof TypeError}return typed+'|'+calls})()",
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
