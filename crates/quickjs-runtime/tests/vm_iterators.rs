use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExceptionKind, ExecutionError, ExecutionLimits,
    Function, JsNumber, JsString, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource,
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
        let body = source.body().to_utf8_lossy().map_err(engine_failure)?;
        let dynamic_source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &[],
            SourceFragment::new(&body),
        );
        with_dynamic_function_source(dynamic_source, FrontendLimits::default(), |unit, _| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("<runtime iterators>"))
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

fn string_value(value: &quickjs_runtime::JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("string")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
fn symbol_global_publishes_distinct_well_known_symbols_and_method_identities() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let first=Symbol('item');\
            let second=Symbol('item');\
            let statics=[\
                Symbol.toPrimitive,Symbol.iterator,Symbol.match,Symbol.matchAll,\
                Symbol.replace,Symbol.search,Symbol.split,Symbol.toStringTag,\
                Symbol.isConcatSpreadable,Symbol.hasInstance,Symbol.species,\
                Symbol.unscopables,Symbol.asyncIterator];\
            for(let i=0;i<statics.length;i++){\
                if(typeof statics[i]!=='symbol')return false;\
                for(let j=i+1;j<statics.length;j++){\
                    if(statics[i]===statics[j])return false;\
                }\
            }\
            return (first!==second)&&\
                (String(first)==='Symbol(item)')&&\
                (first.description==='item')&&\
                (Symbol.name==='Symbol')&&(Symbol.length===0)&&\
                (Symbol.for.name==='for')&&(Symbol.for.length===1)&&\
                (Symbol.keyFor.name==='keyFor')&&(Symbol.keyFor.length===1)&&\
                (Symbol.prototype.toString.name==='toString')&&\
                (Symbol.prototype.toString.length===0)&&\
                (Symbol.prototype.valueOf.name==='valueOf')&&\
                (Symbol.prototype.valueOf.length===0)&&\
                (Symbol.prototype[Symbol.toPrimitive].name==='[Symbol.toPrimitive]')&&\
                (Symbol.prototype[Symbol.toPrimitive].length===1)&&\
                (Symbol.keyFor(Symbol.for('shared'))==='shared');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Symbol surface");
    assert_eq!(result.as_boolean().expect("live value"), Some(true));
}

#[test]
fn array_iterators_use_live_length_and_inherited_holes() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            Array.prototype[0]=7;\
            let values=[,2];\
            let iterator=values.values();\
            let first=iterator.next();\
            values[2]=3;\
            let second=iterator.next();\
            let third=iterator.next();\
            let done=iterator.next();\
            return first.value*1000+second.value*100+third.value*10+Number(done.done);",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("array iteration");
    assert_number(&result, 7231);
}

#[test]
fn array_keys_and_entries_return_iterator_result_objects() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let key=[9].keys().next();\
            let entry=[4].entries().next();\
            return Number(key.done)*1000+key.value*100+entry.value[0]*10+entry.value[1];",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("array iterator kinds");
    assert_number(&result, 4);
}

#[test]
fn string_iterator_yields_unicode_code_points() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let iterator='A𐐷B'[Symbol.iterator]();\
            let first=iterator.next();\
            let second=iterator.next();\
            let third=iterator.next();\
            let done=iterator.next();\
            return first.value+'|'+second.value+'|'+third.value+'|'+done.done;",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("string iteration");
    assert_eq!(
        result
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "A|𐐷|B|true"
    );
}

#[test]
fn iterator_prototype_symbol_iterator_returns_the_receiver() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let iterator=[1].values();\
            return iterator[Symbol.iterator]()===iterator;",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("iterator identity");
    assert_eq!(result.as_boolean().expect("live value"), Some(true));
}

#[test]
fn iterator_constructor_surface_and_subclassing_follow_ecma_262() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let prototype=Object.getOwnPropertyDescriptor(Iterator,'prototype');\
            let constructor=Object.getOwnPropertyDescriptor(Iterator.prototype,'constructor');\
            let tag=Object.getOwnPropertyDescriptor(Iterator.prototype,Symbol.toStringTag);\
            class Derived extends Iterator{}\
            let derived=new Derived();\
            let directCall=false,directConstruct=false;\
            try{Iterator();}catch(error){directCall=error instanceof TypeError;}\
            try{new Iterator();}catch(error){directConstruct=error instanceof TypeError;}\
            return [typeof Iterator,Iterator.name,Iterator.length,\
                Object.getPrototypeOf(Iterator)===Function.prototype,\
                prototype.writable,prototype.enumerable,prototype.configurable,\
                typeof constructor.get,typeof constructor.set,\
                constructor.enumerable,constructor.configurable,\
                Iterator.prototype[Symbol.toStringTag],typeof tag.get,typeof tag.set,\
                Object.getPrototypeOf(derived)===Derived.prototype,\
                derived instanceof Derived,derived instanceof Iterator,\
                directCall,directConstruct].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator constructor surface");
    assert_eq!(
        string_value(&result),
        "function|Iterator|0|true|false|false|false|function|function|false|true|\
         Iterator|function|function|true|true|true|true|true"
    );
}

#[test]
fn iterator_from_gets_the_protocol_once_and_wraps_only_plain_iterators() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let log='';let count=0;\
            let iterator=new Proxy({},{get(target,key,receiver){\
                if(key==='next'){log+='n';return function(){count++;return {done:count>1,value:7};};}\
                return Reflect.get(target,key,receiver);}});\
            let iterable=new Proxy({},{get(target,key,receiver){\
                if(key===Symbol.iterator){log+='i';return function(){log+='m';return iterator;};}\
                return Reflect.get(target,key,receiver);}});\
            let wrapper=Iterator.from(iterable);let first=wrapper.next();let done=wrapper.next();\
            function* values(){yield 1;}let generator=values();\
            return [log,first.value,first.done,done.done,\
                Object.getPrototypeOf(Object.getPrototypeOf(wrapper))===Iterator.prototype,\
                Iterator.from(generator)===generator,\
                Array.from(Iterator.from('A𐐷')).join(':'),\
                Iterator.from.call(null,iterator)!==iterator].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.from protocol");
    assert_eq!(string_value(&result), "imn|7|false|true|true|true|A:𐐷|true");
}

#[test]
fn iterator_from_wrapper_return_and_intrinsic_accessors_are_spec_ordered() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "\
            let log='';let result={done:true,value:9};\
            let iterator=new Proxy({},{get(target,key,receiver){\
                log+='get:'+String(key)+'|';\
                if(key==='next')return function(){return result;};\
                if(key==='return')return function(){log+='call:'+(this===iterator)+'|';return result;};\
                return Reflect.get(target,key,receiver);}});\
            let wrapper=Iterator.from(iterator);let returned=wrapper.return();\
            let missing=Iterator.from({}).return();\
            let constructor=Object.getOwnPropertyDescriptor(Iterator.prototype,'constructor');\
            let tag=Object.getOwnPropertyDescriptor(Iterator.prototype,Symbol.toStringTag);\
            let child=Object.create(Iterator.prototype);constructor.set.call(child,4);\
            let tagChild=Object.create(Iterator.prototype);tag.set.call(tagChild,'custom');\
            let homeConstructor=false,homeTag=false,primitive=false,invalidWrapper=false;\
            try{constructor.set.call(Iterator.prototype,0);}catch(error){homeConstructor=error instanceof TypeError;}\
            try{tag.set.call(Iterator.prototype,0);}catch(error){homeTag=error instanceof TypeError;}\
            try{constructor.set.call(null,0);}catch(error){primitive=error instanceof TypeError;}\
            try{Object.getPrototypeOf(wrapper).return.call({});}catch(error){invalidWrapper=error instanceof TypeError;}\
            return [log,returned===result,missing.done,missing.value===undefined,\
                child.constructor,tagChild[Symbol.toStringTag],homeConstructor,homeTag,primitive,invalidWrapper].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator wrapper return and accessors");
    assert_eq!(
        string_value(&result),
        "get:Symbol(Symbol.iterator)|get:next|get:return|call:true||true|true|true|4|custom|\
         true|true|true|true"
    );
}

#[test]
fn rooted_iterator_from_wrapper_keeps_its_hidden_record_live_through_collection() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let wrapper = {
        let mut context = runtime.context(&realm).expect("context");
        let function = dynamic_function(
            &mut context,
            "let state={value:41};\
             let iterator={next(){return {done:false,value:state.value};}};\
             return Iterator.from(iterator);",
        );
        context
            .call(&function, &[], ExecutionLimits::default())
            .expect("Iterator.from wrapper")
    };

    runtime
        .collect_cycles()
        .expect("rooted Iterator wrapper survives collection");

    let mut context = runtime.context(&realm).expect("context");
    let next = dynamic_function(&mut context, "return arguments[0].next().value;");
    let result = context
        .call(&next, &[wrapper], ExecutionLimits::default())
        .expect("hidden Iterator Record remains live");
    assert!(
        result
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(41))
    );
}

#[test]
fn rooted_iterator_map_helper_keeps_its_hidden_state_live_through_collection() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let helper = {
        let mut context = runtime.context(&realm).expect("context");
        let function = dynamic_function(
            &mut context,
            "let state={value:41};\
             let iterator={next(){return {done:false,value:state.value};}};\
             return Iterator.prototype.map.call(iterator,value=>value+1);",
        );
        context
            .call(&function, &[], ExecutionLimits::default())
            .expect("Iterator map helper")
    };

    runtime
        .collect_cycles()
        .expect("rooted Iterator map helper survives collection");

    let mut context = runtime.context(&realm).expect("context");
    let next = dynamic_function(&mut context, "return arguments[0].next().value;");
    let result = context
        .call(&next, &[helper], ExecutionLimits::default())
        .expect("hidden Iterator helper state remains live");
    assert!(
        result
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(42))
    );
}

#[test]
fn rooted_iterator_concat_helper_keeps_captured_records_live_through_collection() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let helper = {
        let mut context = runtime.context(&realm).expect("context");
        let function = dynamic_function(
            &mut context,
            "return Iterator.concat({get [Symbol.iterator](){\
               let state={value:42};return function(){return {\
                 next(){return {done:false,value:state.value};}};};}});",
        );
        context
            .call(&function, &[], ExecutionLimits::default())
            .expect("Iterator concat helper")
    };

    runtime
        .collect_cycles()
        .expect("captured Iterator.concat records survive collection");

    let mut context = runtime.context(&realm).expect("context");
    let next = dynamic_function(&mut context, "return arguments[0].next().value;");
    let result = context
        .call(&next, &[helper], ExecutionLimits::default())
        .expect("hidden Iterator.concat record remains live");
    assert_number(&result, 42);
}

#[test]
fn rooted_iterator_zip_helper_keeps_records_padding_and_keys_live_through_collection() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let helper = {
        let mut context = runtime.context(&realm).expect("context");
        let function = dynamic_function(
            &mut context,
            "let state={value:42};return Iterator.zipKeyed({item:{[Symbol.iterator](){\
               let used=false;return {next(){return used?{done:true}:\
                 (used=true,{done:false,value:state.value});}};}}},\
               {mode:'longest',padding:{item:99}});",
        );
        context
            .call(&function, &[], ExecutionLimits::default())
            .expect("Iterator.zipKeyed helper")
    };

    runtime
        .collect_cycles()
        .expect("captured Iterator.zipKeyed state survives collection");

    let mut context = runtime.context(&realm).expect("context");
    let next = dynamic_function(&mut context, "return arguments[0].next().value.item;");
    let result = context
        .call(&next, &[helper], ExecutionLimits::default())
        .expect("hidden Iterator.zipKeyed state remains live");
    assert_number(&result, 42);
}

#[test]
fn rooted_iterator_flat_map_helper_keeps_its_active_inner_iterator_live() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let helper = {
        let mut context = runtime.context(&realm).expect("context");
        let function = dynamic_function(
            &mut context,
            "let outerDone=false,state={value:40};\
             let helper=Iterator.prototype.flatMap.call({next(){\
               if(outerDone)return {done:true};outerDone=true;return {done:false,value:state};}},\
               function(shared){let index=0;return {next(){index++;return index<3\
                 ?{done:false,value:shared.value+index}:{done:true};}};});\
             helper.next();return helper;",
        );
        context
            .call(&function, &[], ExecutionLimits::default())
            .expect("Iterator flatMap helper")
    };

    runtime
        .collect_cycles()
        .expect("active flatMap inner iterator survives collection");

    let mut context = runtime.context(&realm).expect("context");
    let next = dynamic_function(&mut context, "return arguments[0].next().value;");
    let result = context
        .call(&next, &[helper], ExecutionLimits::default())
        .expect("hidden flatMap inner iterator remains live");
    assert_number(&result, 42);
}

#[test]
fn iterator_to_array_retains_next_and_observes_step_value_order() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let log='',nextGets=0,nextCalls=0,returnCalls=0;\
         let iterator={\
           get next(){nextGets++;log+='n';return function(){\
             nextCalls++;log+='c';\
             if(nextCalls>2)return {get done(){log+='D';return true;},\
               get value(){throw new Error('must not read value');}};\
             return {get done(){log+='d';return false;},\
               get value(){log+='v';return nextCalls;}};};},\
           return(){returnCalls++;return {done:true};}\
         };\
         let result=Iterator.prototype.toArray.call(iterator);\
         return [log,result.join(','),result.length,\
           Object.getPrototypeOf(result)===Array.prototype,\
           nextGets,nextCalls,returnCalls].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.toArray ordering");
    assert_eq!(string_value(&result), "ncdvcdvcD|1,2|2|true|1|3|0");
}

#[test]
fn iterator_to_array_propagates_step_abrupts_without_closing() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let doneError={},valueError={},doneReturns=0,valueReturns=0;\
         let doneIterator={next(){return {get done(){throw doneError;}};},\
           return(){doneReturns++;throw new Error('must not close');}};\
         let valueIterator={next(){return {done:false,get value(){throw valueError;}};},\
           return(){valueReturns++;throw new Error('must not close');}};\
         let donePreserved=false,valuePreserved=false,nonObject=false,primitive=false;\
         try{Iterator.prototype.toArray.call(doneIterator);}catch(error){donePreserved=error===doneError;}\
         try{Iterator.prototype.toArray.call(valueIterator);}catch(error){valuePreserved=error===valueError;}\
         try{Iterator.prototype.toArray.call({next(){return null;}});}catch(error){nonObject=error instanceof TypeError;}\
         try{Iterator.prototype.toArray.call(0);}catch(error){primitive=error instanceof TypeError;}\
         return [donePreserved,valuePreserved,doneReturns,valueReturns,nonObject,primitive].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.toArray abrupt order");
    assert_eq!(string_value(&result), "true|true|0|0|true|true");
}

#[test]
fn iterator_to_array_infinite_input_is_stopped_by_uncatchable_fuel() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "return Iterator.prototype.toArray.call({\
           next(){return {done:false,value:1};}\
         });",
    );
    let error = context
        .call(
            &function,
            &[],
            ExecutionLimits::default().with_instruction_fuel(128),
        )
        .expect_err("infinite toArray must exhaust fuel");
    assert!(matches!(
        error,
        ExecutionError::InstructionLimitExceeded {
            limit: 128,
            executed: 128,
        }
    ));
}

#[test]
fn iterator_map_is_lazy_and_exposes_the_iterator_helper_protocol() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let log='',nextGets=0,nextCalls=0,mapperCalls=0;\
         let iterator={\
           get next(){nextGets++;log+='g';return function(){\
             nextCalls++;log+='n';return nextCalls<3\
               ?{get done(){log+='d';return false;},get value(){log+='v';return nextCalls;}}\
               :{get done(){log+='D';return true;},get value(){throw new Error('unread');}};};}};\
         let helper=Iterator.prototype.map.call(iterator,function(value,index){\
           mapperCalls++;log+='m'+index;return value*10+index;});\
         let helperPrototype=Object.getPrototypeOf(helper);\
         let before=[log,nextGets,nextCalls,mapperCalls].join(',');\
         let first=helper.next();let second=helper.next();let done=helper.next();\
         return [before,log,first.value,first.done,second.value,second.done,\
           done.value===undefined,done.done,nextGets,nextCalls,mapperCalls,\
           Object.getPrototypeOf(helperPrototype)===Iterator.prototype,\
           helperPrototype[Symbol.toStringTag],Iterator.prototype.map.name,\
           Iterator.prototype.map.length,typeof helperPrototype.next,\
           typeof helperPrototype.return,helper[Symbol.iterator]()===helper].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.map lazy helper");
    assert_eq!(
        string_value(&result),
        "g,1,0,0|gndvm0ndvm1nD|10|false|21|false|true|true|1|3|2|true|\
         Iterator Helper|map|1|function|function|true"
    );
}

#[test]
fn iterator_map_return_and_mapper_abrupts_close_exactly_once() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let startCloses=0,yieldCloses=0,abruptCloses=0,reentrantCloses=0;\
         let start=Iterator.prototype.map.call({next(){return {done:false,value:1};},\
           return(){startCloses++;return {};}},x=>x);\
         let startResult=start.return();start.return();\
         let yielded=Iterator.prototype.map.call({next(){return {done:false,value:2};},\
           return(){yieldCloses++;return {};}},x=>x);\
         yielded.next();let yieldResult=yielded.return();yielded.return();\
         let original={};let preserved=false;\
         let abrupt=Iterator.prototype.map.call({next(){return {done:false,value:3};},\
           return(){abruptCloses++;throw {};}},function(){throw original;});\
         try{abrupt.next();}catch(error){preserved=error===original;}\
         let helper;let reentrant=false;\
         helper=Iterator.prototype.map.call({next(){return {done:false,value:4};},\
           return(){reentrantCloses++;return {};}},function(){\
             try{helper.next();}catch(error){reentrant=error instanceof TypeError;}\
             throw original;});\
         try{helper.next();}catch(error){}\
         return [startResult.value===undefined,startResult.done,startCloses,\
           yieldResult.value===undefined,yieldResult.done,yieldCloses,\
           preserved,abruptCloses,reentrant,reentrantCloses].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.map closing");
    assert_eq!(
        string_value(&result),
        "true|true|1|true|true|1|true|1|true|1"
    );
}

#[test]
fn iterator_map_validation_and_step_abrupts_follow_spec_order() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let validationLog='',stepCloses=0,validationType=false,stepPreserved=false;\
         class ValidationIterator extends Iterator {\
           get next(){validationLog+='next';throw {}}\
           return(){validationLog+='return';return {};}}\
         let validation=new ValidationIterator();\
         try{validation.map();}catch(error){validationType=error instanceof TypeError;}\
         try{validation.map({});}catch(error){validationType=validationType&&(error instanceof TypeError);}\
         let original={};\
         let helper=Iterator.prototype.map.call({next(){return {\
           get done(){throw original;}};},return(){stepCloses++;return {};}},x=>x);\
         try{helper.next();}catch(error){stepPreserved=error===original;}\
         return [validationLog,validationType,stepPreserved,stepCloses].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.map validation and step abrupts");
    assert_eq!(string_value(&result), "returnreturn|true|true|0");
}

#[test]
fn iterator_flat_map_is_lazy_and_reuses_each_inner_next_method() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let outerCalls=0,mapperLog='',iteratorCalls=0,nextGets=0;\
         let outer={next(){outerCalls++;return outerCalls<3\
           ?{done:false,value:outerCalls}:{done:true};}};\
         let helper=Iterator.prototype.flatMap.call(outer,function(value,index){\
           mapperLog+=value+':'+index+',';let innerCalls=0;return {\
             [Symbol.iterator](){iteratorCalls++;return {get next(){nextGets++;return function(){\
               innerCalls++;return innerCalls<3?{done:false,value:value*10+innerCalls}:{done:true};};}};}};});\
         let before=[outerCalls,mapperLog,iteratorCalls,nextGets].join(',');\
         let a=helper.next(),b=helper.next(),c=helper.next(),d=helper.next(),done=helper.next();\
         return [before,a.value,b.value,c.value,d.value,done.value===undefined,done.done,\
           outerCalls,mapperLog,iteratorCalls,nextGets,Iterator.prototype.flatMap.name,\
           Iterator.prototype.flatMap.length].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.flatMap lazy helper");
    assert_eq!(
        string_value(&result),
        "0,,0,0|11|12|21|22|true|true|3|1:0,2:1,|2|2|flatMap|1"
    );
}

#[test]
fn iterator_flat_map_closes_inner_then_outer_and_preserves_abrupts() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let log='',innerError={},outerError={},preserved=false;\
         let outer={next(){return {done:false,value:1};},return(){log+='O';throw outerError;}};\
         let helper=Iterator.prototype.flatMap.call(outer,function(){let first=true;return {\
           next(){if(first){first=false;return {done:false,value:1};}return {done:true};},\
           return(){log+='I';throw innerError;}};});\
         helper.next();try{helper.return();}catch(error){preserved=error===innerError;}\
         let mapperError={},mapperPreserved=false,mapperCloses=0;\
         let mapperHelper=Iterator.prototype.flatMap.call({\
           next(){return {done:false,value:1};},return(){mapperCloses++;throw {};}} ,\
           function(){throw mapperError;});\
         try{mapperHelper.next();}catch(error){mapperPreserved=error===mapperError;}\
         let stepError={},stepPreserved=false,innerCloses=0,outerCloses=0;\
         let stepHelper=Iterator.prototype.flatMap.call({\
           next(){return {done:false,value:1};},return(){outerCloses++;return {};}},\
           function(){return {next(){throw stepError;},return(){innerCloses++;return {};}};});\
         try{stepHelper.next();}catch(error){stepPreserved=error===stepError;}\
         return [log,preserved,mapperPreserved,mapperCloses,stepPreserved,innerCloses,outerCloses].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.flatMap close ordering");
    assert_eq!(string_value(&result), "IO|true|true|1|true|0|1");
}

#[test]
fn iterator_consumers_share_indexed_callback_and_exhaustion_semantics() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "function source(){let index=0;return {next(){index++;return index<4\
           ?{done:false,value:index}:{done:true};}};}\
         let everyLog='',someLog='',findLog='',forEachLog='',thisValues=[];\
         let every=Iterator.prototype.every.call(source(),function(value,index){'use strict';\
           everyLog+=value+':'+index+',';thisValues.push(this);return value<4;});\
         let some=Iterator.prototype.some.call(source(),function(value,index){'use strict';\
           someLog+=value+':'+index+',';thisValues.push(this);return value===2;});\
         let found=Iterator.prototype.find.call(source(),function(value,index){'use strict';\
           findLog+=value+':'+index+',';thisValues.push(this);return value===2;});\
         let each=Iterator.prototype.forEach.call(source(),function(value,index){'use strict';\
           forEachLog+=value+':'+index+',';thisValues.push(this);return 99;});\
         let empty={next(){return {done:true};}};\
         return [every,everyLog,some,someLog,found,findLog,each===undefined,forEachLog,\
           Iterator.prototype.every.call(empty,()=>false),\
           Iterator.prototype.some.call(empty,()=>true),\
           Iterator.prototype.find.call(empty,()=>true)===undefined,\
           thisValues.every(value=>value===undefined)].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator consuming helpers");
    assert_eq!(
        string_value(&result),
        "true|1:0,2:1,3:2,|true|1:0,2:1,|2|1:0,2:1,|true|1:0,2:1,3:2,|true|false|true|true"
    );
}

#[test]
fn iterator_consumers_close_only_for_validation_callbacks_and_early_exit() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let validationLog='',validationType=false;\
         let validation={get next(){validationLog+='n';throw {};},\
           return(){validationLog+='r';throw {};}};\
         try{Iterator.prototype.every.call(validation,0);}catch(error){\
           validationType=error instanceof TypeError;}\
         let callbackError={},callbackCloses=0,callbackPreserved=false;\
         try{Iterator.prototype.forEach.call({next(){return {done:false,value:1};},\
           return(){callbackCloses++;throw {};}} ,function(){throw callbackError;});}\
         catch(error){callbackPreserved=error===callbackError;}\
         let closeError={},closeOverrides=false;\
         try{Iterator.prototype.some.call({next(){return {done:false,value:1};},\
           return(){throw closeError;}},()=>true);}catch(error){closeOverrides=error===closeError;}\
         let stepError={},stepCloses=0,stepPreserved=false;\
         try{Iterator.prototype.find.call({next(){return {get done(){throw stepError;}};},\
           return(){stepCloses++;return {};}},()=>true);}\
         catch(error){stepPreserved=error===stepError;}\
         return [validationLog,validationType,callbackPreserved,callbackCloses,\
           closeOverrides,stepPreserved,stepCloses].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator consumer close ordering");
    assert_eq!(string_value(&result), "r|true|true|1|true|true|0");
}

#[test]
fn iterator_includes_uses_same_value_zero_skipping_and_normal_close() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let method=Iterator.prototype.includes;
         let descriptor=Object.getOwnPropertyDescriptor(Iterator.prototype,'includes');
         let nextGets=0,nextCalls=0,returnCalls=0;
         let iterator={get next(){nextGets++;return function(){nextCalls++;
           return nextCalls<=3?{done:false,value:nextCalls}:{done:true};};},
           return(){returnCalls++;return {};}};
         let matched=method.call(iterator,2,1);
         let token={},identity=[{},token].values().includes(token);
         return [matched,nextGets,nextCalls,returnCalls,[NaN].values().includes(NaN),
           [-0].values().includes(+0),identity,[4].values().includes(4,1),
           method.name,method.length,descriptor.writable,!descriptor.enumerable,
           descriptor.configurable].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.includes comparison and close");
    assert_eq!(
        string_value(&result),
        "true|1|2|1|true|true|true|false|includes|1|true|true|true"
    );
}

#[test]
fn iterator_includes_validates_before_next_and_does_not_close_step_failures() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let method=Iterator.prototype.includes,validationLog='',valueOfCalls=0;
         function validation(value){let iterator={get next(){validationLog+='n';throw {};},
           return(){validationLog+='r';return {};}};let type='';
           try{method.call(iterator,0,value);}catch(error){type=error.name;}return type;}
         let objectType=validation({valueOf(){valueOfCalls++;return 0;}});
         let negativeType=validation(-1),largeType=validation(Number.MAX_SAFE_INTEGER+1);
         let stepError={},stepCloses=0,stepPreserved=false;
         try{method.call({next(){return {get done(){throw stepError;}};},
           return(){stepCloses++;return {};}} ,0);}catch(error){stepPreserved=error===stepError;}
         let naturalReturns=0,natural=method.call({next(){return {done:true};},
           return(){naturalReturns++;return {};}} ,0,Infinity);
         return [objectType,negativeType,largeType,validationLog,valueOfCalls,
           stepPreserved,stepCloses,natural,naturalReturns].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.includes validation and abrupt order");
    assert_eq!(
        string_value(&result),
        "TypeError|RangeError|RangeError|rrr|0|true|0|false|0"
    );
}

#[test]
fn iterator_join_formats_values_and_publishes_the_intrinsic_contract() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let method=Iterator.prototype.join;
         let descriptor=Object.getOwnPropertyDescriptor(Iterator.prototype,'join');
         let separatorCalls=0,separator={toString(){separatorCalls++;return '--';}};
         let result=method.call([1,null,undefined,'x'].values(),separator);
         let defaulted=[1,null,3].values().join();
         let empty=[].values().join('-');
         return [result,defaulted,empty,separatorCalls,method.name,method.length,
           descriptor.writable,!descriptor.enumerable,descriptor.configurable].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.join results and descriptor");
    assert_eq!(
        string_value(&result),
        "1------x|1,,3||1|join|1|true|true|true"
    );
}

#[test]
fn iterator_join_closes_only_string_conversion_failures() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let method=Iterator.prototype.join;
         let bad={toString(){return {};},valueOf(){return {};}};
         let separatorClose=0,separatorNext=false,separatorType=false;
         try{method.call({get next(){separatorNext=true;},
           return(){separatorClose++;}},bad);}catch(error){separatorType=error instanceof TypeError;}
         let elementClose=0,elementType=false,elementStep=0;
         try{method.call({next(){elementStep++;return elementStep===1?
           {done:false,value:bad}:{done:true};},return(){elementClose++;}});}
           catch(error){elementType=error instanceof TypeError;}
         let nextError={},nextClose=0,nextPreserved=false;
         try{method.call({get next(){throw nextError;},return(){nextClose++;}});}
           catch(error){nextPreserved=error===nextError;}
         let exhaustionClose=0,exhausted=method.call({next(){return {done:true};},
           return(){exhaustionClose++;}});
         let protocolClose=0,protocolType=false;
         try{method.call({next(){return 1;},return(){protocolClose++;}});}
           catch(error){protocolType=error instanceof TypeError;}
         return [separatorType,separatorClose,separatorNext,elementType,elementClose,
           nextPreserved,nextClose,exhausted,exhaustionClose,protocolType,protocolClose].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.join close ordering");
    assert_eq!(
        string_value(&result),
        "true|1|false|true|1|true|0||0|true|0"
    );
}

#[test]
fn iterator_chunks_and_windows_use_retained_helper_buffers() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "function source(){return [0,1,2,3,4].values();}\
         let chunks=Array.from(source().chunks(2));\
         let windows=Array.from(source().windows(3));\
         let partial=Array.from([0,1].values().windows(3,'allow-partial'));\
         return [chunks.length,chunks[0].join(','),chunks[1].join(','),\
           chunks[2].join(','),chunks[0]!==chunks[1],windows.length,\
           windows[0].join(','),windows[1].join(','),windows[2].join(','),\
           windows[0]!==windows[1],partial.length,partial[0].join(','),\
           Iterator.prototype.chunks.name,Iterator.prototype.chunks.length,\
           Iterator.prototype.windows.name,Iterator.prototype.windows.length].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator chunking helper buffers");
    assert_eq!(
        string_value(&result),
        "3|0,1|2,3|4|true|3|0,1,2|1,2,3|2,3,4|true|1|0,1|chunks|1|windows|1"
    );
}

#[test]
fn iterator_chunking_validation_and_exhaustion_close_in_spec_order() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let log='',chunksType=false,windowsType=false;\
         function invalid(){return {get next(){log+='n';throw {};},\
           return(){log+='r';return {};}};}\
         try{Iterator.prototype.chunks.call(invalid(),'2');}\
         catch(error){chunksType=error instanceof TypeError;}\
         try{Iterator.prototype.windows.call(invalid(),1,'bad');}\
         catch(error){windowsType=error instanceof TypeError;}\
         let step=0,exhaustedReturns=0;\
         let exhausted=Iterator.prototype.chunks.call({next(){step++;return step===1\
           ?{done:false,value:1}:{done:true};},return(){exhaustedReturns++;throw {}; }},2);\
         let partial=exhausted.next();let exhaustedReturn=true;\
         try{exhausted.return();}catch(error){exhaustedReturn=false;}\
         let closes=0,open=Iterator.prototype.windows.call({\
           next(){return {done:false,value:1};},return(){closes++;return {};}},2);\
         open.return();open.return();\
         return [log,chunksType,windowsType,partial.value.join(','),exhaustedReturn,\
           exhaustedReturns,closes].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator chunking validation and close order");
    assert_eq!(string_value(&result), "rr|true|true|1|true|0|1");
}

#[test]
fn iterator_reduce_distinguishes_missing_and_explicit_initial_values() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "function source(){let value=0;return {next(){value++;return value<4\
           ?{done:false,value}:{done:true};}};}\
         let initialLog='',missingLog='',thisValues=[];\
         let withInitial=Iterator.prototype.reduce.call(source(),function(memo,value,index){\
           'use strict';initialLog+=memo+':'+value+':'+index+',';thisValues.push(this);\
           return memo+value;},10);\
         let withoutInitial=Iterator.prototype.reduce.call(source(),function(memo,value,index){\
           'use strict';missingLog+=memo+':'+value+':'+index+',';thisValues.push(this);\
           return memo+value;});\
         let explicit=Iterator.prototype.reduce.call({\
           next(){return this.done?{done:true}:(this.done=true,{done:false,value:7});}},\
           function(memo,value,index){return [memo===undefined,value,index].join(':');},undefined);\
         let token={},empty={next(){return {done:true};}};\
         let retained=Iterator.prototype.reduce.call(empty,()=>0,token)===token;\
         let emptyType=false;try{Iterator.prototype.reduce.call(empty,()=>0);}\
         catch(error){emptyType=error instanceof TypeError;}\
         return [withInitial,initialLog,withoutInitial,missingLog,explicit,retained,emptyType,\
           thisValues.every(value=>value===undefined)].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.reduce accumulator semantics");
    assert_eq!(
        string_value(&result),
        "16|10:1:0,11:2:1,13:3:2,|6|1:2:1,3:3:2,|true:7:0|true|true|true"
    );
}

#[test]
fn iterator_reduce_closes_only_validation_and_reducer_failures() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let validationLog='',validationType=false;\
         let validation={get next(){validationLog+='n';throw {};},\
           return(){validationLog+='r';throw {};}};\
         try{Iterator.prototype.reduce.call(validation,0);}catch(error){\
           validationType=error instanceof TypeError;}\
         let reducerError={},reducerCloses=0,reducerPreserved=false;\
         try{Iterator.prototype.reduce.call({next(){return {done:false,value:1};},\
           return(){reducerCloses++;throw {};}} ,function(){throw reducerError;},0);}\
         catch(error){reducerPreserved=error===reducerError;}\
         let stepError={},stepCloses=0,stepPreserved=false;\
         try{Iterator.prototype.reduce.call({next(){return {get done(){throw stepError;}};},\
           return(){stepCloses++;return {};}},()=>0,0);}\
         catch(error){stepPreserved=error===stepError;}\
         let emptyCloses=0,emptyType=false;\
         try{Iterator.prototype.reduce.call({next(){return {done:true};},\
           return(){emptyCloses++;return {};}},()=>0);}\
         catch(error){emptyType=error instanceof TypeError;}\
         return [validationLog,validationType,reducerPreserved,reducerCloses,\
           stepPreserved,stepCloses,emptyType,emptyCloses].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.reduce close ordering");
    assert_eq!(string_value(&result), "r|true|true|1|true|0|true|0");
}

#[test]
fn iterator_symbol_dispose_invokes_return_and_ignores_its_result() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let method=Iterator.prototype[Symbol.dispose],calls=0,receiver=false,args=-1;\
         let iterator={return(){calls++;receiver=this===iterator;args=arguments.length;return 99;}};\
         let result=method.call(iterator);\
         let absent=method.call({})===undefined;\
         let type=false;try{method.call({return:0});}catch(error){type=error instanceof TypeError;}\
         let descriptor=Object.getOwnPropertyDescriptor(Iterator.prototype,Symbol.dispose);\
         let symbolDescriptor=Object.getOwnPropertyDescriptor(Symbol,'dispose');\
         return [typeof Symbol.dispose,typeof method,method.name,method.length,calls,receiver,args,\
           result===undefined,absent,type,descriptor.writable,!descriptor.enumerable,\
           descriptor.configurable,!symbolDescriptor.writable,!symbolDescriptor.enumerable,\
           !symbolDescriptor.configurable].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype[Symbol.dispose]");
    assert_eq!(
        string_value(&result),
        "symbol|function|[Symbol.dispose]|0|1|true|0|true|true|true|true|true|true|true|true|true"
    );
}

#[test]
fn iterator_concat_captures_methods_eagerly_and_opens_iterators_lazily() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let log='';\
         function item(tag,values){return {get [Symbol.iterator](){log+='g'+tag;return function(){\
           log+='o'+tag;let index=0;return {get next(){log+='n'+tag;return function(){\
             return index<values.length?{done:false,value:values[index++]}:{done:true};};}};};}};}\
         let helper=Iterator.concat(item('a',[1,2]),item('b',[3]));\
         let before=log;let first=helper.next();let middle=log;\
         let second=helper.next();let third=helper.next();let done=helper.next();\
         return [before,first.value,first.done,middle,second.value,third.value,done.value===undefined,\
           done.done,log,helper[Symbol.iterator]()===helper].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.concat lazy sequencing");
    assert_eq!(
        string_value(&result),
        "gagb|1|false|gagboana|2|3|true|true|gagboanaobnb|true"
    );
}

#[test]
fn iterator_concat_return_targets_only_the_active_iterator() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let unopened=0,opened=0,closed=0;\
         let fresh=Iterator.concat({[Symbol.iterator](){unopened++;return {next(){return {done:false};},\
           return(){closed++;return {};}};}});fresh.return();\
         let helper=Iterator.concat({[Symbol.iterator](){opened++;return {\
           next(){return {done:false,value:1};},return(){closed++;return {};}};}},\
           {[Symbol.iterator](){unopened++;return {next(){return {done:true};}};}});\
         helper.next();let returned=helper.return();let after=helper.next();\
         let stepError={},stepClosed=0,preserved=false;\
         let failing=Iterator.concat({[Symbol.iterator](){return {next(){throw stepError;},\
           return(){stepClosed++;return {};}};}});\
         try{failing.next();}catch(error){preserved=error===stepError;}\
         return [opened,unopened,closed,returned.value===undefined,returned.done,\
           after.value===undefined,after.done,preserved,stepClosed].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.concat return forwarding");
    assert_eq!(string_value(&result), "1|0|1|true|true|true|true|true|0");
}

#[test]
fn iterator_zip_modes_and_keyed_results_share_reverse_close_semantics() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let close='';\
         function input(tag,values){let index=0;return {next(){return index<values.length\
           ?{done:false,value:values[index++]}:{done:true};},return(){close+=tag;return {};}};}\
         let longest=Iterator.zipKeyed({x:input('x',[1]),y:input('y',[2,3])},\
           {mode:'longest',padding:{x:9,y:8}});\
         let first=longest.next().value,second=longest.next().value,done=longest.next();\
         let strict=Iterator.zip([input('a',[4,5]),input('b',[6])],{mode:'strict'});\
         strict.next();let mismatch=false;try{strict.next();}catch(error){mismatch=error instanceof TypeError;}\
         let left=input('l',[1]),right=input('r',[2]);Iterator.zip([left,right]).return();\
         return [Object.getPrototypeOf(first)===null,first.x,first.y,second.x,second.y,\
           done.done,mismatch,close].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.zip modes");
    assert_eq!(string_value(&result), "true|1|2|9|3|true|true|arl");
}

#[test]
fn iterator_filter_is_lazy_and_indexes_every_examined_value() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let nextCalls=0,predicateLog='';\
         let iterator={next(){nextCalls++;return nextCalls<4\
           ?{done:false,value:nextCalls}:{done:true};}};\
         let helper=Iterator.prototype.filter.call(iterator,function(value,index){\
           predicateLog+=value+':'+index+',';return value%2;});\
         let before=[nextCalls,predicateLog].join('|');\
         let first=helper.next();let second=helper.next();let done=helper.next();\
         return [before,first.value,first.done,second.value,second.done,\
           done.value===undefined,done.done,nextCalls,predicateLog].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.filter lazy helper");
    assert_eq!(
        string_value(&result),
        "0||1|false|3|false|true|true|4|1:0,2:1,3:2,"
    );
}

#[test]
fn iterator_filter_predicate_abrupt_closes_and_preserves_the_original_error() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let original={},closes=0,preserved=false;\
         let helper=Iterator.prototype.filter.call({\
           next(){return {done:false,value:1};},\
           return(){closes++;throw {};}} ,function(){throw original;});\
         try{helper.next();}catch(error){preserved=error===original;}\
         let done=helper.next();\
         return [preserved,closes,done.value===undefined,done.done].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.filter abrupt close");
    assert_eq!(string_value(&result), "true|1|true|true");
}

#[test]
fn iterator_take_coerces_before_getting_next_and_closes_at_the_limit() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let log='',nextCalls=0,returnCalls=0;\
         let iterator={get next(){log+='n';return function(){nextCalls++;\
           return {done:false,value:nextCalls};};},return(){returnCalls++;log+='r';return {};}};\
         let limit={[Symbol.toPrimitive](){log+='c';return 2.9;}};\
         let helper=Iterator.prototype.take.call(iterator,limit);\
         let before=[log,nextCalls,returnCalls].join(',');\
         let first=helper.next();let second=helper.next();let done=helper.next();\
         return [before,first.value,first.done,second.value,second.done,\
           done.value===undefined,done.done,log,nextCalls,returnCalls].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.take ordering");
    assert_eq!(
        string_value(&result),
        "cn,0,0|1|false|2|false|true|true|cnr|2|1"
    );
}

#[test]
fn iterator_take_invalid_limits_close_before_reading_next() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let log='',range=false,negative=false,getterPreserved=false,\
           preserved=false,original={};\
         let iterator={get next(){log+='n';return function(){};},\
           return(){log+='r';return {};}};\
         try{Iterator.prototype.take.call(iterator,NaN);}catch(error){range=error instanceof RangeError;}\
         try{Iterator.prototype.take.call(iterator,-1);}catch(error){negative=error instanceof RangeError;}\
         try{Iterator.prototype.take.call(iterator,{get valueOf(){throw original;}});}\
         catch(error){getterPreserved=error===original;}\
         try{Iterator.prototype.take.call(iterator,{[Symbol.toPrimitive](){throw original;}});}\
         catch(error){preserved=error===original;}\
         return [log,range,negative,getterPreserved,preserved].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.take invalid limits");
    assert_eq!(string_value(&result), "rrrr|true|true|true|true");
}

#[test]
fn iterator_take_accepts_finite_limits_above_max_safe_integer() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let nextGets=0;let iterator={get next(){nextGets++;return function(){\
           return {done:true};};}};\
         let helper=Iterator.prototype.take.call(iterator,Number.MAX_SAFE_INTEGER+1);\
         let done=helper.next();return [nextGets,done.done,done.value===undefined].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("current Iterator.prototype.take large limit");
    assert_eq!(string_value(&result), "1|true|true");
}

#[test]
fn iterator_drop_is_lazy_and_does_not_read_skipped_values() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let log='',nextCalls=0,valueGets=0;\
         let iterator={get next(){log+='n';return function(){let current=++nextCalls;\
           return {done:false,get value(){valueGets++;return current;}};};},\
           return(){log+='r';return {};}};\
         let limit={[Symbol.toPrimitive](){log+='c';return 2.9;}};\
         let helper=Iterator.prototype.drop.call(iterator,limit);\
         let before=[log,nextCalls,valueGets].join(',');\
         let first=helper.next();let second=helper.next();helper.return();\
         return [before,first.value,first.done,second.value,second.done,\
           log,nextCalls,valueGets].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.drop lazy skipping");
    assert_eq!(string_value(&result), "cn,0,0|3|false|4|false|cnr|4|2");
}

#[test]
fn iterator_drop_invalid_limits_close_before_reading_next() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let log='',range=false,negative=false,getterPreserved=false,\
           preserved=false,original={};\
         let iterator={get next(){log+='n';return function(){};},\
           return(){log+='r';return {};}};\
         try{Iterator.prototype.drop.call(iterator,NaN);}catch(error){range=error instanceof RangeError;}\
         try{Iterator.prototype.drop.call(iterator,-1);}catch(error){negative=error instanceof RangeError;}\
         try{Iterator.prototype.drop.call(iterator,{get valueOf(){throw original;}});}\
         catch(error){getterPreserved=error===original;}\
         try{Iterator.prototype.drop.call(iterator,{[Symbol.toPrimitive](){throw original;}});}\
         catch(error){preserved=error===original;}\
         return [log,range,negative,getterPreserved,preserved].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("Iterator.prototype.drop invalid limits");
    assert_eq!(string_value(&result), "rrrr|true|true|true|true");
}

#[test]
fn iterator_drop_accepts_finite_limits_above_max_safe_integer() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = dynamic_function(
        &mut context,
        "let nextGets=0;let iterator={get next(){nextGets++;return function(){\
           return {done:true};};}};\
         let helper=Iterator.prototype.drop.call(iterator,Number.MAX_SAFE_INTEGER+1);\
         let done=helper.next();return [nextGets,done.done,done.value===undefined].join('|');",
    );

    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("current Iterator.prototype.drop large limit");
    assert_eq!(string_value(&result), "1|true|true");
}

#[test]
fn array_spread_reads_iterator_twice_and_retains_next_once() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let iteratorReads=0;let nextReads=0;let count=0;\
         let iterable={\
           get [Symbol.iterator](){iteratorReads++;return function(){return {\
             get next(){nextReads++;return function(){count++;return {done:count>1,value:6};};}\
           };};}\
         };\
         let result=[...iterable];\
         return result[0]+'|'+iteratorReads+'|'+nextReads;",
    );

    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("custom spread");
    assert_eq!(string_value(&value), "6|2|1");
}

#[test]
fn initial_next_getter_failure_does_not_close_the_iterator() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let closed=0;\
         let iterable={[Symbol.iterator](){return {\
           get next(){throw 'next';},\
           return(){closed++;return {};}};}};\
         try{[...iterable];}catch(error){return error+'|'+closed;}",
    );

    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("caught next getter");
    assert_eq!(string_value(&value), "next|0");
}

#[test]
fn iterator_close_preserves_the_original_abrupt_completion() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let closed=0;\
         let iterable={[Symbol.iterator](){return {\
           next(){return {get done(){throw 'original';}};},\
           return(){closed++;throw 'close';}};}};\
         try{[...iterable];}catch(error){return error+'|'+closed;}",
    );

    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("caught original completion");
    assert_eq!(string_value(&value), "original|1");
}

#[test]
fn nested_abrupt_spreads_close_inner_then_outer() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let log='';\
         let inner={[Symbol.iterator](){return {\
           next(){throw 'original';},\
           return(){log+='i';return {};}};}};\
         let outer={[Symbol.iterator](){return {\
           next(){[...inner];return {done:true};},\
           return(){log+='o';return {};}};}};\
         try{[...outer];}catch(error){return error+'|'+log;}",
    );

    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("caught nested spread");
    assert_eq!(string_value(&value), "original|io");
}

#[test]
fn array_iterator_length_uses_quickjs_uint32_conversion() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let values=Array.prototype.values;\
         let negative=values.call({0:7,length:-1}).next();\
         let wrappedZero=values.call({0:8,length:4294967296}).next();\
         let wrappedOne=values.call({0:9,length:4294967297}).next();\
         return negative.done+'|'+negative.value+'|'+wrappedZero.done+'|'\
           +wrappedOne.done+'|'+wrappedOne.value;",
    );

    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("uint32 lengths");
    assert_eq!(string_value(&value), "false|7|true|false|9");
}

#[test]
fn array_iterator_rereads_live_index_after_reentrant_length_getter() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let iterator;let inside=false;let log='';\
         let value={\
           get length(){\
             log+='L';\
             if(!inside){inside=true;let inner=iterator.next();\
               log+='i'+inner.value+':'+inner.done;inside=false;}\
             return 2;\
           },\
           0:'a',1:'b'\
         };\
         iterator=Array.prototype.values.call(value);\
         let outer=iterator.next();\
         return log+'|'+outer.value+'|'+outer.done;",
    );

    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("reentrant Array iterator");
    assert_eq!(string_value(&value), "LLia:false|b|false");
}

#[test]
fn symbol_undefined_is_descriptionless() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let missing;let symbol=Symbol(missing);\
         return symbol.description===missing&&String(symbol)==='Symbol()';",
    );

    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("descriptionless Symbol");
    assert_eq!(value.as_boolean().expect("live value"), Some(true));
}

#[test]
fn iterator_method_nonobject_result_uses_the_pinned_type_error() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "return [...{[Symbol.iterator](){return 1;}}];",
    );

    let error = context
        .call(&run, &[], ExecutionLimits::default())
        .expect_err("nonobject iterator result");
    let ExecutionError::Exception(exception) = error else {
        panic!("iterator result must throw a JavaScript exception");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
    assert_eq!(
        exception
            .message()
            .expect("message")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "not an object"
    );
}

#[test]
fn iterator_protocol_routes_proxy_gets_through_start_step_and_close() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let log='';let result=new Proxy({},{get(target,key,receiver){\
           if(key==='done'){log+='d';return false;}\
           if(key==='value'){log+='v';return 7;}\
           return Reflect.get(target,key,receiver);}});\
         let iterator=new Proxy({},{get(target,key,receiver){\
           if(key==='next'){log+='n';return function(){log+='c';return result;};}\
           if(key==='return'){log+='r';return function(){log+='x';return {};};}\
           return Reflect.get(target,key,receiver);}});\
         let iterable=new Proxy({},{get(target,key,receiver){\
           if(key===Symbol.iterator){log+='i';return function(){log+='m';return iterator;};}\
           return Reflect.get(target,key,receiver);}});\
         let value;for(value of iterable){log+='b';break;}return value+'|'+log;",
    );

    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("Proxy iterator protocol");
    assert_eq!(string_value(&value), "7|imncdvbrx");
}

#[test]
fn infinite_custom_iterator_is_stopped_by_uncatchable_fuel() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let iterable={[Symbol.iterator](){return {next(){return {done:false,value:1};}};}};\
         try{return [...iterable];}catch(error){return 'caught';}",
    );

    let error = context
        .call(
            &run,
            &[],
            ExecutionLimits::default().with_instruction_fuel(100),
        )
        .expect_err("infinite iterator must exhaust fuel");
    assert!(matches!(
        error,
        ExecutionError::InstructionLimitExceeded { limit: 100, .. }
    ));
}
