//! `Set` constructor, mutation, iteration, and set-composition semantics.
//!
//! The surface is pinned to `QuickJS` 2026-06-04. Observable evaluation order
//! follows the current ECMA-262 keyed-collection algorithms, including the
//! branch-dependent iteration order and exact `IteratorClose` boundaries.

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExecutionError, ExecutionLimits, Function, JsString,
    OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource, Runtime, RuntimeLimits,
    RuntimeResource,
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
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime Set>"))
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
fn set_surface_matches_the_pinned_engine() {
    assert_eq!(
        rendered(
            "var methods=['isDisjointFrom','isSubsetOf','isSupersetOf','intersection','difference','symmetricDifference','union'];\
             return Object.getOwnPropertyNames(Set).join(',')+'|'+\
             Object.getOwnPropertyNames(Set.prototype).join(',')+'|'+\
             Set.length+'|'+(Set[Symbol.species]===Set)+'|'+\
             (Set.prototype.keys===Set.prototype.values)+'|'+\
             (Set.prototype[Symbol.iterator]===Set.prototype.values)+'|'+\
             Object.prototype.toString.call(new Set())+'|'+\
             methods.map(function(name){return Set.prototype[name].length}).join(',');"
        ),
        "length,name,groupBy,prototype|add,has,delete,clear,size,forEach,isDisjointFrom,isSubsetOf,isSupersetOf,intersection,difference,symmetricDifference,union,values,keys,entries,constructor|0|true|true|true|[object Set]|1,1,1,1,1,1,1"
    );
}

#[test]
fn set_uses_same_value_zero_and_preserves_insertion_order() {
    assert_eq!(
        rendered(
            "var s=new Set();var chained=s.add(-0).add(NaN).add(1).add(1)===s;\
             var removed=s.delete(NaN);s.add(2);s.delete(1);s.add(1);\
             return [s.size,s.has(+0),removed,chained,Array.from(s).join(':')].join('|');"
        ),
        "3|true|true|true|0:2:1"
    );
}

#[test]
fn set_constructor_observes_adder_before_iterator_and_uses_new_target() {
    assert_eq!(
        rendered(
            "var log=[];var saved=Set.prototype.add;\
             Object.defineProperty(Set.prototype,'add',{configurable:true,get:function(){log.push('adder');return saved}});\
             var iterable={[Symbol.iterator]:function(){log.push('iterator');return [1,2][Symbol.iterator]()}};\
             var set=new Set(iterable);\
             Object.defineProperty(Set.prototype,'add',{configurable:true,writable:true,value:saved});\
             function Derived(){}var prototype=Object.create(Set.prototype);Derived.prototype=prototype;\
             var derived=Reflect.construct(Set,[[3]],Derived);\
             return [log.join(','),Array.from(set).join(':'),Object.getPrototypeOf(derived)===prototype,derived instanceof Derived,derived.size].join('|');"
        ),
        "adder,iterator|1:2|true|true|1"
    );
}

#[test]
fn set_constructor_uses_the_exact_iterator_close_boundaries() {
    assert_eq!(
        rendered(
            "var original={};var secondary={};var log=[];var caught=[];\
             function iterable(next,close){return {[Symbol.iterator]:function(){return {next:next,return:close}}};}\
             var saved=Set.prototype.add;var touched=0;\
             Object.defineProperty(Set.prototype,'add',{configurable:true,writable:true,value:1});\
             try{new Set({get [Symbol.iterator](){touched++;return function(){}}})}catch(error){caught.push(error instanceof TypeError)}\
             Object.defineProperty(Set.prototype,'add',{configurable:true,writable:true,value:saved});\
             try{new Set(iterable(function(){throw original},function(){log.push('next-close');return {}}))}catch(error){caught.push(error===original)}\
             try{new Set(iterable(function(){return {get done(){throw original}}},function(){log.push('done-close');return {}}))}catch(error){caught.push(error===original)}\
             try{new Set(iterable(function(){return {done:false,get value(){throw original}}},function(){log.push('value-close');return {}}))}catch(error){caught.push(error===original)}\
             Object.defineProperty(Set.prototype,'add',{configurable:true,writable:true,value:function(){throw original}});\
             try{new Set(iterable(function(){return {done:false,value:1}},function(){log.push('adder-close');throw secondary}))}catch(error){caught.push(error===original)}\
             Object.defineProperty(Set.prototype,'add',{configurable:true,writable:true,value:saved});\
             return [touched,log.join(','),caught.join(',')].join('|');"
        ),
        "0|adder-close|true,true,true,true,true"
    );
}

#[test]
fn set_iterators_and_for_each_are_live_across_mutation() {
    assert_eq!(
        rendered(
            "var s=new Set([1,2]);var iterator=s.values();var first=iterator.next().value;\
             s.delete(2);s.add(3);var second=iterator.next().value;s.clear();s.add(4);var third=iterator.next().value;\
             var f=new Set([1,2]);var seen=[];\
             f.forEach(function(value,key,set){seen.push(value+':'+key+':'+(set===f));if(value===1){f.delete(2);f.add(3)}});\
             return [first,second,third,iterator.next().done,seen.join(',')].join('|');"
        ),
        "1|3|4|true|1:1:true,3:3:true"
    );
}

#[test]
fn set_composition_preserves_branch_dependent_order_and_duplicate_rules() {
    assert_eq!(
        rendered(
            "var base=new Set([1,2,3]);\
             function like(size,values){return {size:size,has:function(value){return values.indexOf(value)>=0},keys:function(){return values[Symbol.iterator]()}}}\
             var difference=base.difference(like(1,[2]));\
             var intersection=base.intersection(like(2,[3,1]));\
             var symmetric=base.symmetricDifference(like(4,[2,2,4,4]));\
             var union=base.union(like(3,[3,4,4]));\
             return [Array.from(difference),Array.from(intersection),Array.from(symmetric),Array.from(union)].map(function(values){return values.join(':')}).join('|');"
        ),
        "1:3|3:1|1:3:4|1:2:3:4"
    );
}

#[test]
fn set_composition_internal_branches_observe_appended_entries() {
    assert_eq!(
        rendered(
            "var set=new Set([1,2]);var seen=[];\
             var other={size:99,has:function(value){seen.push(value);if(value===1)set.add(3);return true},keys:function(){throw 1}};\
             var result=set.intersection(other);\
             var subsetSet=new Set([1]);var subsetSeen=[];\
             var subsetOther={size:99,has:function(value){subsetSeen.push(value);if(value===1)subsetSet.add(2);return true},keys:function(){throw 2}};\
             var subset=subsetSet.isSubsetOf(subsetOther);\
             return [seen.join(','),Array.from(result).join(','),subsetSeen.join(','),subset].join('|');"
        ),
        "1,2,3|1,2,3|1,2|true"
    );
}

#[test]
fn get_set_record_observes_conversion_and_validation_order() {
    assert_eq!(
        rendered(
            "var log=[];\
             var other={get size(){log.push('size');return {valueOf:function(){log.push('valueOf');return 0}}},\
                        get has(){log.push('has');return function(){}},\
                        get keys(){log.push('keys');return function(){log.push('keys-call');return [][Symbol.iterator]()}}};\
             new Set().union(other);\
             var nan={get size(){log.push('nan-size');return NaN},get has(){log.push('nan-has');return function(){}},get keys(){log.push('nan-keys');return function(){}}};\
             var negative={get size(){log.push('negative-size');return -1},get has(){log.push('negative-has');return function(){}},get keys(){log.push('negative-keys');return function(){}}};\
             var invalidThis={get size(){log.push('invalid-size');return 0},has:function(){},keys:function(){}};\
             var caught=[];try{new Set().union(nan)}catch(error){caught.push(error instanceof TypeError)}\
             try{new Set().union(negative)}catch(error){caught.push(error instanceof RangeError)}\
             try{Set.prototype.union.call({},invalidThis)}catch(error){caught.push(error instanceof TypeError)}\
             return [log.join(','),caught.join(',')].join('|');"
        ),
        "size,valueOf,has,keys,keys-call,nan-size,negative-size|true,true,true"
    );
}

#[test]
fn early_set_predicates_normally_close_the_other_iterator() {
    assert_eq!(
        rendered(
            "var log=[];function like(value,badReturn){return {size:0,has:function(){return false},keys:function(){var done=false;return {next:function(){if(done)return {done:true};done=true;return {done:false,value:value}},return:function(){log.push('return-'+value);return badReturn?1:{}}}}}}\
             var disjoint=new Set([1,2]).isDisjointFrom(like(2,false));\
             var superset=new Set([1,2]).isSupersetOf(like(3,false));\
             var type=false;try{new Set([1]).isDisjointFrom(like(1,true))}catch(error){type=error instanceof TypeError}\
             return [disjoint,superset,type,log.join(',')].join('|');"
        ),
        "false|false|true|return-2,return-3,return-1"
    );
}

#[test]
fn set_composition_constructs_an_intrinsic_set_without_species_observation() {
    assert_eq!(
        rendered(
            "var touched=0;var set=new Set([1]);\
             Object.defineProperty(set,'constructor',{get:function(){touched++;throw 1}});\
             var result=set.union(new Set([2]));\
             return [touched,Object.getPrototypeOf(result)===Set.prototype,result instanceof Set,Array.from(result).join(':')].join('|');"
        ),
        "0|true|true|1:2"
    );
}

#[test]
fn set_group_by_matches_the_pinned_quickjs_extension() {
    assert_eq!(
        rendered(
            "var grouped=Set.groupBy([1,2,3,4],function(value,index){return (value+index)%3});\
             var out=[];grouped.forEach(function(values,key){out.push(key+':'+values.join(','))});\
             return [grouped instanceof Map,out.join('|')].join('|');"
        ),
        "true|1:1,4|0:2|2:3"
    );
}

#[test]
fn set_native_scans_consume_shared_fuel_before_mutation() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let make = dynamic_function(
        &mut context,
        "var set=new Set();for(var index=0;index<200;index++)set.add(index);return set;",
    );
    let clear = dynamic_function(
        &mut context,
        "arguments[0].clear();return arguments[0].size;",
    );
    let size = dynamic_function(&mut context, "return arguments[0].size;");
    let set = context
        .call(&make, &[], ExecutionLimits::default())
        .expect("large Set");

    let result = context.call(
        &clear,
        std::slice::from_ref(&set),
        ExecutionLimits::default().with_instruction_fuel(100),
    );
    assert!(matches!(
        result,
        Err(ExecutionError::InstructionLimitExceeded { limit: 100, .. })
    ));
    let retained_size = context
        .call(
            &size,
            std::slice::from_ref(&set),
            ExecutionLimits::default(),
        )
        .expect("Set remains readable");
    assert_eq!(
        retained_size
            .as_number()
            .expect("live value")
            .expect("Number")
            .as_f64()
            .to_bits(),
        200.0_f64.to_bits()
    );
}

#[test]
fn set_composition_preflights_the_complete_result_entry_budget() {
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_collection_entries(3)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let make = dynamic_function(&mut context, "return new Set([1,2]);");
    let union = dynamic_function(&mut context, "return arguments[0].union(new Set());");
    let set = context
        .call(&make, &[], ExecutionLimits::default())
        .expect("source Set");
    assert_eq!(context.runtime_usage().collection_entries(), 2);

    let result = context.call(
        &union,
        std::slice::from_ref(&set),
        ExecutionLimits::default(),
    );
    assert!(matches!(
        result,
        Err(ExecutionError::LimitExceeded {
            resource: RuntimeResource::CollectionEntries,
            limit: 3,
            observed: 4,
        })
    ));
    assert_eq!(context.runtime_usage().collection_entries(), 2);
}
