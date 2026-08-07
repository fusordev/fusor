//! `WeakMap` and `WeakSet` constructor, key, and upsert semantics.
//!
//! The surface is pinned to `QuickJS` 2026-06-04. Observable ordering follows
//! the current ECMA-262 weak-keyed collection algorithms, including
//! `CanBeHeldWeakly`, callback validation, and `IteratorClose` boundaries.

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
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
                let context = CompilationContext::new_with_source_name(
                    unit,
                    Arc::from("<runtime weak collections>"),
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
fn weak_collection_surfaces_match_the_pinned_engine() {
    assert_eq!(
        rendered(
            "return Object.getOwnPropertyNames(WeakMap).join(',')+'|'+\
             Object.getOwnPropertyNames(WeakMap.prototype).join(',')+'|'+\
             WeakMap.length+'|'+Object.prototype.toString.call(new WeakMap())+'|'+\
             Object.getOwnPropertyNames(WeakSet).join(',')+'|'+\
             Object.getOwnPropertyNames(WeakSet.prototype).join(',')+'|'+\
             WeakSet.length+'|'+Object.prototype.toString.call(new WeakSet());"
        ),
        "length,name,prototype|set,get,getOrInsert,getOrInsertComputed,has,delete,constructor|0|[object WeakMap]|length,name,prototype|add,has,delete,constructor|0|[object WeakSet]"
    );
}

#[test]
fn weak_collections_accept_objects_and_non_registered_symbols_only() {
    assert_eq!(
        rendered(
            "var object={};var unique=Symbol('unique');var wellKnown=Symbol.iterator;var registered=Symbol.for('registered');\
             var map=new WeakMap();var set=new WeakSet();\
             var chainedMap=map.set(object,1).set(unique,2).set(wellKnown,3)===map;\
             var chainedSet=set.add(object).add(unique).add(wellKnown)===set;\
             var errors=[];try{map.set(registered,4)}catch(error){errors.push(error.message)}\
             try{set.add(registered)}catch(error){errors.push(error.message)}\
             return [chainedMap,chainedSet,map.get(object),map.get(unique),map.get(wellKnown),\
                     map.has(1),String(map.get(1)),map.delete(1),set.has(1),set.delete(1),\
                     errors.join(',')].join('|');"
        ),
        "true|true|1|2|3|false|undefined|false|false|false|invalid value used as WeakMap key,invalid value used as WeakSet key"
    );
}

#[test]
fn weak_map_upserts_follow_validation_and_reentrant_update_order() {
    assert_eq!(
        rendered(
            "var map=new WeakMap();var first={};var second={};var calls=[];\
             var inserted=map.getOrInsert(first,1);var existing=map.getOrInsert(first,2);\
             var computed=map.getOrInsertComputed(second,function(key){'use strict';calls.push(this===undefined,key===second);map.set(key,9);return 10});\
             var existingCallbackError=false;try{map.getOrInsertComputed(first,1)}catch(error){existingCallbackError=error instanceof TypeError}\
             var invalidKeyError=false;try{map.getOrInsert(1,3)}catch(error){invalidKeyError=error instanceof TypeError}\
             return [inserted,existing,computed,map.get(second),calls.join(','),existingCallbackError,invalidKeyError].join('|');"
        ),
        "1|1|10|10|true,true|true|true"
    );
}

#[test]
fn weak_collection_constructors_observe_adder_close_and_new_target_boundaries() {
    assert_eq!(
        rendered(
            "var log=[];var original={};var key={};var closed=0;\
             var iterable={[Symbol.iterator]:function(){log.push('iterator');var done=false;return {\
               next:function(){if(done)return {done:true};done=true;return {done:false,value:[1,2]}},\
               return:function(){closed++;log.push('return');return {}}}}};\
             var saved=WeakMap.prototype.set;\
             Object.defineProperty(WeakMap.prototype,'set',{configurable:true,get:function(){log.push('adder');return saved}});\
             var caught=false;try{new WeakMap(iterable)}catch(error){caught=error instanceof TypeError}\
             Object.defineProperty(WeakMap.prototype,'set',{configurable:true,writable:true,value:saved});\
             function Derived(){}var prototype=Object.create(WeakMap.prototype);Derived.prototype=prototype;\
             var derived=Reflect.construct(WeakMap,[[[key,7]]],Derived);\
             var setKey={};var weakSet=new WeakSet([setKey]);\
             return [log.join(','),closed,caught,Object.getPrototypeOf(derived)===prototype,derived instanceof Derived,derived.get(key),weakSet.has(setKey)].join('|');"
        ),
        "adder,iterator,return|1|true|true|true|7|true"
    );
}

#[test]
fn weak_collection_brand_checks_precede_key_validation() {
    assert_eq!(
        rendered(
            "var messages=[];\
             try{WeakMap.prototype.set.call({},1,2)}catch(error){messages.push(error.message)}\
             try{WeakSet.prototype.add.call({},1)}catch(error){messages.push(error.message)}\
             try{WeakMap.prototype.getOrInsertComputed.call(new WeakMap(),1,1)}catch(error){messages.push(error.message)}\
             return messages.join('|');"
        ),
        "not a WeakMap object|not a WeakSet object|invalid value used as WeakMap key"
    );
}
