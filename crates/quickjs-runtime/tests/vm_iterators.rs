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
