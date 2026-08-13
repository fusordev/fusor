//! `Map` constructor, mutation, iteration, and callback semantics.
//!
//! The surface is pinned to `QuickJS` 2026-06-04. Observable evaluation order
//! follows the current ECMA-262 algorithms, including `IteratorClose` at each
//! abrupt `AddEntriesFromIterable` boundary.

use std::{error::Error, fmt, sync::Arc};

use fusor_bytecode::{VerificationLimits, VerifiedBytecode};
use fusor_compiler::CompilationContext;
use fusor_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use fusor_runtime::{
    Context, DynamicFunctionCompileFailure, ExecutionLimits, Function, JsString,
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
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime Map>"))
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

fn rendered(body: &str) -> String {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, body);
    context
        .call(&run, &[], ExecutionLimits::default())
        .expect("completed")
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
fn map_surface_matches_the_pinned_engine() {
    assert_eq!(
        rendered(
            "return Object.getOwnPropertyNames(Map).join(',')+'|'+\
             Object.getOwnPropertyNames(Map.prototype).join(',')+'|'+\
             Map.length+'|'+(Map[Symbol.species]===Map)+'|'+\
             (Map.prototype[Symbol.iterator]===Map.prototype.entries)+'|'+\
             Object.prototype.toString.call(new Map());"
        ),
        "length,name,groupBy,prototype|set,get,getOrInsert,getOrInsertComputed,has,delete,clear,size,forEach,values,keys,entries,constructor|0|true|true|[object Map]"
    );
}

#[test]
fn map_uses_same_value_zero_and_preserves_insertion_order() {
    assert_eq!(
        rendered(
            "var m=new Map();m.set(-0,'zero').set(NaN,'nan').set(1,'one');\
             var same=m.set(1,'updated')===m;\
             return [m.size,m.get(+0),m.has(NaN),same,\
                     m.delete(NaN),m.size,Array.from(m.keys()).join(':')].join('|');"
        ),
        "3|zero|true|true|true|2|0:1"
    );
}

#[test]
fn map_constructor_observes_adder_then_iterator_and_closes_on_entry_failure() {
    assert_eq!(
        rendered(
            "var log=[];var original={};var closed=0;\
             var iterable={\
               [Symbol.iterator]:function(){log.push('iterator');return {\
                 next:function(){log.push('next');return {done:false,value:{\
                   get 0(){log.push('key');throw original},\
                   get 1(){log.push('value');return 1}\
                 }}},\
                 return:function(){closed++;log.push('return');return {}}\
               }}\
             };\
             var saved=Map.prototype.set;\
             Object.defineProperty(Map.prototype,'set',{configurable:true,get:function(){log.push('adder');return saved}});\
             var caught=false;try{new Map(iterable)}catch(error){caught=error===original}\
             Object.defineProperty(Map.prototype,'set',{configurable:true,writable:true,value:saved});\
             return [log.join(','),closed,caught].join('|');"
        ),
        "adder,iterator,next,key,return|1|true"
    );
}

#[test]
fn map_constructor_uses_the_exact_iterator_close_boundaries() {
    assert_eq!(
        rendered(
            "var original={};var secondary={};var log=[];var caught=[];\
             function iterable(next,close){return {[Symbol.iterator]:function(){return {next:next,return:close}}};}\
             var saved=Map.prototype.set;var touched=0;\
             Object.defineProperty(Map.prototype,'set',{configurable:true,writable:true,value:1});\
             try{new Map({get [Symbol.iterator](){touched++;return function(){}}})}catch(error){caught.push(error instanceof TypeError)}\
             Object.defineProperty(Map.prototype,'set',{configurable:true,writable:true,value:saved});\
             try{new Map(iterable(function(){throw original},function(){log.push('next-close');return {}}))}catch(error){caught.push(error===original)}\
             try{new Map(iterable(function(){return {get done(){throw original}}},function(){log.push('done-close');return {}}))}catch(error){caught.push(error===original)}\
             try{new Map(iterable(function(){return {done:false,get value(){throw original}}},function(){log.push('value-close');return {}}))}catch(error){caught.push(error===original)}\
             try{new Map(iterable(function(){return {done:false,value:{get 0(){throw original}}}},function(){log.push('entry-close');throw secondary}))}catch(error){caught.push(error===original)}\
             Object.defineProperty(Map.prototype,'set',{configurable:true,writable:true,value:function(){throw original}});\
             try{new Map(iterable(function(){return {done:false,value:[1,2]}},function(){log.push('adder-close');return {}}))}catch(error){caught.push(error===original)}\
             Object.defineProperty(Map.prototype,'set',{configurable:true,writable:true,value:saved});\
             return [touched,log.join(','),caught.join(',')].join('|');"
        ),
        "0|entry-close,adder-close|true,true,true,true,true,true"
    );
}

#[test]
fn map_constructor_uses_new_target_prototype() {
    assert_eq!(
        rendered(
            "function Derived(){}var prototype=Object.create(Map.prototype);prototype.marker=1;Derived.prototype=prototype;\
             var map=Reflect.construct(Map,[],Derived);\
             return [Object.getPrototypeOf(map)===prototype,map instanceof Derived,map.size].join('|');"
        ),
        "true|true|0"
    );
}

#[test]
fn map_iterators_are_live_across_delete_clear_and_append() {
    assert_eq!(
        rendered(
            "var m=new Map([[1,'a'],[2,'b']]);var iterator=m.entries();\
             var first=iterator.next().value.join(':');\
             m.delete(2);m.set(3,'c');\
             var second=iterator.next().value.join(':');\
             m.clear();m.set(4,'d');\
             var third=iterator.next().value.join(':');\
             return [first,second,third,iterator.next().done].join('|');"
        ),
        "1:a|3:c|4:d|true"
    );
}

#[test]
fn map_for_each_and_insert_helpers_preserve_reentrant_spec_order() {
    assert_eq!(
        rendered(
            "var m=new Map([[1,'a'],[2,'b']]);var seen=[];\
             m.forEach(function(value,key,map){seen.push(key+value+(map===m));if(key===1){m.delete(2);m.set(3,'c')}});\
             var first=m.getOrInsert('x',7);var existing=m.getOrInsert('x',8);\
             var computed=m.getOrInsertComputed('y',function(key){m.set(key,9);m.set('tail',11);return 10});\
             var canonical;new Map().getOrInsertComputed(-0,function(key){canonical=key});\
             return [seen.join(','),first,existing,computed,m.get('y'),Array.from(m.keys()).join(':'),\
               Object.is(canonical,0)].join('|');"
        ),
        "1atrue,3ctrue|7|7|10|10|1:3:x:y:tail|true"
    );
}

#[test]
fn map_group_by_returns_intrinsic_map_groups_in_first_key_order() {
    assert_eq!(
        rendered(
            "var grouped=Map.groupBy([1,2,3,4],function(value,index){return (value+index)%3});\
             var out=[];grouped.forEach(function(values,key){out.push(key+':'+values.join(','))});\
             return out.join('|');"
        ),
        "1:1,4|0:2|2:3"
    );
}
