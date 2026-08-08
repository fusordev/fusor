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
