use std::sync::Arc;

use quickjs_bytecode::VerificationLimits;
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    CompilationGoal, DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, FrontendOptions,
    GlobalScriptGoal, SourceFragment, with_dynamic_function_source, with_parsed_program,
};
use quickjs_runtime::{ExecutionLimits, Function, JsValue, Realm, Runtime, RuntimeLimits};

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new_with_source_name(unit, Arc::from("async.js"))
                .expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("root function");
            let tree = context
                .compile_tree(&root, VerificationLimits::default())
                .unwrap_or_else(|error| {
                    panic!("verified async-function tree for {source:?}: {error:?}")
                });
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn compile_dynamic(body: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    let parameters = [];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &parameters,
        SourceFragment::new(body),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |unit, _| {
        let context = CompilationContext::new(unit).expect("dynamic storage plan");
        context
            .compile_dynamic_function_script(VerificationLimits::default())
            .map(|tree| Arc::new(tree.verified_bytecode().clone()))
    })
    .expect("dynamic frontend")
    .expect("dynamic compiler")
}

fn instantiate(runtime: &mut Runtime, realm: &Realm, source: &str, name: &str) -> Function {
    runtime
        .context(realm)
        .expect("context")
        .instantiate(compile(source, name))
        .expect("function")
}

fn start_and_read(start_source: &str) -> String {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let start = instantiate(&mut runtime, &realm, start_source, "start");
    let read = instantiate(
        &mut runtime,
        &realm,
        "function read(state){return state.result;}",
        "read",
    );
    let state: JsValue = runtime
        .context(&realm)
        .expect("context")
        .call(&start, &[], ExecutionLimits::default())
        .expect("async setup");
    runtime
        .context(&realm)
        .expect("context")
        .call(&read, &[state], ExecutionLimits::default())
        .expect("state read")
        .as_string()
        .expect("live result")
        .expect("string result")
        .to_utf8_lossy()
        .expect("UTF-8")
}

fn dynamic_start_and_read(body: &str) -> String {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let read = instantiate(
        &mut runtime,
        &realm,
        "function read(state){return state.result;}",
        "read",
    );
    let start = runtime
        .context(&realm)
        .expect("context")
        .execute_dynamic_function_script(compile_dynamic(body), ExecutionLimits::default())
        .expect("dynamic async function")
        .into_function()
        .expect("dynamic function result");
    let state = runtime
        .context(&realm)
        .expect("context")
        .call(&start, &[], ExecutionLimits::default())
        .expect("async setup");
    runtime
        .context(&realm)
        .expect("context")
        .call(&read, &[state], ExecutionLimits::default())
        .expect("state read")
        .as_string()
        .expect("live result")
        .expect("string result")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
fn async_class_methods_preserve_the_super_home_object_across_await() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                class Base{\
                    get value(){return this._value;}\
                    set value(next){this._value=next;}\
                    static get answer(){return this._answer;}\
                    static set answer(next){this._answer=next;}\
                }\
                class Derived extends Base{\
                    constructor(value){super();this._value=value;}\
                    async read(){let value=await 1;return super['value']+=value;}\
                    static async read(){let value=await 1;return super['answer']+=value;}\
                }\
                let value=new Derived(3);\
                Derived._answer=4;\
                value.read().then(function(result){state.result=state.result+'instance:'+result+'|';});\
                Derived.read().then(function(result){state.result=state.result+'static:'+result;});\
                return state;\
            }"
        ),
        "instance:4|static:5"
    );
}

#[test]
fn static_private_async_methods_preserve_the_home_object_across_await() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                class Base{static value(){return 2;}}\
                class Derived extends Base{\
                    static async #value(){let increment=await 1;return super.value()+increment;}\
                    static read(){return this.#value();}\
                    static privateName(){return this.#value.name;}\
                }\
                Derived.read().then(function(result){\
                    state.result=result+':'+Derived.privateName();\
                });\
                return state;\
            }"
        ),
        "3:#value"
    );
}

#[test]
fn async_arrows_await_with_lexical_receiver_arguments_and_inferred_name() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                function make(value){\
                    let task=async ({increment})=>{\
                        let resumed=await increment;\
                        return this.base+value+resumed+arguments[0];\
                    };\
                    task({increment:2}).then(function(result){\
                        state.result=result+':'+task.name+':'+('prototype' in task);\
                    });\
                    return state;\
                }\
                return make.call({base:3},4);\
            }"
        ),
        "13:task:false"
    );
}

#[test]
fn await_always_resumes_as_a_fifo_promise_job() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function work(){\
                    state.result=state.result+'start|';\
                    let value=await 1;\
                    state.result=state.result+'resume:'+value+'|';\
                    return value+1;\
                }\
                function completed(value){state.result=state.result+'then:'+value;}\
                let promise=work();\
                state.result=state.result+'after-call|';\
                promise.then(completed);\
                return state;\
            }"
        ),
        "start|after-call|resume:1|then:2"
    );
}

#[test]
fn rejected_await_resumes_as_a_throw_completion_at_the_await_site() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function work(){\
                    let rejected={then:function(resolve,reject){reject('reason');}};\
                    try{await rejected;}\
                    catch(error){state.result='caught:'+error;}\
                    return 7;\
                }\
                function completed(value){state.result=state.result+'|then:'+value;}\
                work().then(completed);\
                return state;\
            }"
        ),
        "caught:reason|then:7"
    );
}

#[test]
fn uncaught_throw_rejects_instead_of_escaping_the_async_call() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:'returned|'};\
                async function work(){throw 'boom';}\
                function rejected(reason){state.result=state.result+'rejected:'+reason;}\
                let promise=work();\
                promise.catch(rejected);\
                return state;\
            }"
        ),
        "returned|rejected:boom"
    );
}

#[test]
fn async_return_uses_promise_resolution_and_assimilates_thenables() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function work(){\
                    return {then:function(resolve){state.result=state.result+'thenable|';resolve(9);}};\
                }\
                function completed(value){state.result=state.result+'value:'+value;}\
                work().then(completed);\
                return state;\
            }"
        ),
        "thenable|value:9"
    );
}

#[test]
fn await_and_return_preserve_finally_before_promise_settlement() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function work(){\
                    try{return await 2;}\
                    finally{state.result=state.result+'finally|';}\
                }\
                work().then(function(value){state.result=state.result+'then:'+value;});\
                return state;\
            }"
        ),
        "finally|then:2"
    );
}

#[test]
fn for_await_consumes_sync_values_and_skips_value_after_done() {
    assert_eq!(
        dynamic_start_and_read(
            "let state={result:''};\
                let iterable={\
                    [Symbol.asyncIterator]:function(){return {\
                        next:function(){return {\
                            get done(){state.result=state.result+'done|';return true;},\
                            get value(){state.result=state.result+'value|';throw 'unreachable';}\
                        };}\
                    };}\
                };\
                async function work(){\
                    for await(const value of [1,{then:function(resolve){resolve(2);}}]){\
                        state.result=state.result+'item:'+value+'|';\
                    }\
                    for await(const value of iterable){state.result=state.result+'body|';}\
                    return 3;\
                }\
                work().then(function(value){state.result=state.result+'then:'+value;});\
                return state;"
        ),
        "item:1|item:2|done|then:3"
    );
}

#[test]
fn for_await_break_awaits_the_async_iterator_return_value() {
    assert_eq!(
        dynamic_start_and_read(
            "let state={result:''};\
                let iterable={\
                    [Symbol.asyncIterator]:function(){return {\
                        next:function(){state.result=state.result+'next|';return {value:1,done:false};},\
                        return:function(){\
                            state.result=state.result+'return|';\
                            return {then:function(resolve){\
                                state.result=state.result+'return-await|';resolve({});\
                            }};\
                        }\
                    };}\
                };\
                async function work(){\
                    for await(const value of iterable){\
                        state.result=state.result+'body:'+value+'|';break;\
                    }\
                    state.result=state.result+'after|';\
                }\
                work().then(function(){state.result=state.result+'done';});\
                return state;"
        ),
        "next|body:1|return|return-await|after|done"
    );
}

#[test]
fn for_await_throw_preserves_the_original_completion_during_async_close() {
    assert_eq!(
        dynamic_start_and_read(
            "let state={result:''};\
                let iterable={\
                    [Symbol.asyncIterator]:function(){return {\
                        next:function(){return {value:1,done:false};},\
                        return:function(){return {then:function(resolve,reject){reject('close');}};}\
                    };}\
                };\
                async function work(){\
                    try{for await(const value of iterable){throw 'body';}}\
                    catch(error){state.result='caught:'+error;}\
                }\
                work().then(function(){state.result=state.result+'|done';});\
                return state;"
        ),
        "caught:body|done"
    );
}

#[test]
fn for_await_step_errors_do_not_close_the_iterator() {
    assert_eq!(
        dynamic_start_and_read(
            "let state={result:'',closes:0};\
                function iterable(next){return {\
                    [Symbol.asyncIterator]:function(){return {\
                        next:next,\
                        return:function(){state.closes=state.closes+1;return {};}\
                    };}\
                };}\
                async function consume(value){\
                    try{for await(const item of value){item;}}\
                    catch(error){state.result=state.result+error+'|';}\
                }\
                async function work(){\
                    await consume(iterable(function(){return {then:function(resolve,reject){reject('next');}};}));\
                    await consume(iterable(function(){return {get done(){throw 'done';}};}));\
                    await consume(iterable(function(){return {done:false,get value(){throw 'value';}};}));\
                    state.result=state.result+'closes:'+state.closes;\
                }\
                work().then(function(){state.result=state.result+'|done';});\
                return state;"
        ),
        "next|done|value|closes:0|done"
    );
}

#[test]
fn for_await_normal_close_uses_the_awaited_close_completion() {
    assert_eq!(
        dynamic_start_and_read(
            "let state={result:''};\
                function iterable(close){return {\
                    [Symbol.asyncIterator]:function(){return {\
                        next:function(){return {value:1,done:false};},return:close\
                    };}\
                };}\
                async function work(){\
                    try{for await(const value of iterable(function(){\
                        return {then:function(resolve){state.result=state.result+'await-primitive|';resolve(1);}};\
                    })){value;break;}}\
                    catch(error){state.result=state.result+'primitive:'+error.name+'|';}\
                    try{for await(const value of iterable(function(){\
                        return {then:function(resolve,reject){state.result=state.result+'await-reject|';reject('close');}};\
                    })){value;break;}}\
                    catch(error){state.result=state.result+'reject:'+error;}\
                }\
                work().then(function(){state.result=state.result+'|done';});\
                return state;"
        ),
        "await-primitive|primitive:TypeError|await-reject|reject:close|done"
    );
}

#[test]
fn for_await_in_an_async_generator_awaits_close_after_resume() {
    assert_eq!(
        dynamic_start_and_read(
            "let state={result:''};\
                let iterable={\
                    [Symbol.asyncIterator]:function(){return {\
                        next:function(){return {value:2,done:false};},\
                        return:function(){return {then:function(resolve){\
                            state.result=state.result+'close|';resolve({});\
                        }};}\
                    };}\
                };\
                async function* work(){\
                    for await(const value of iterable){yield value+1;break;}\
                }\
                let iterator=work();\
                iterator.next().then(function(first){\
                    state.result=state.result+'yield:'+first.value+':'+first.done+'|';\
                    return iterator.next();\
                }).then(function(last){state.result=state.result+'done:'+last.done;});\
                return state;"
        ),
        "yield:3:false|close|done:true"
    );
}

#[test]
fn suspended_async_frame_survives_collection_through_its_output_promise() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let start = instantiate(
        &mut runtime,
        &realm,
        "function start(){\
            let state={result:'waiting'};\
            async function work(){\
                let value=await {then:function(resolve){state.resume=resolve;}};\
                state.result='resumed:'+value;\
                return value+1;\
            }\
            state.promise=work();\
            state.promise.then(function(value){state.result=state.result+'|then:'+value;});\
            return state;\
        }",
        "start",
    );
    let resume = instantiate(
        &mut runtime,
        &realm,
        "function resume(state){state.resume(8);return state;}",
        "resume",
    );
    let read = instantiate(
        &mut runtime,
        &realm,
        "function read(state){return state.result;}",
        "read",
    );

    let state = runtime
        .context(&realm)
        .expect("context")
        .call(&start, &[], ExecutionLimits::default())
        .expect("suspended async function");
    runtime
        .collect_cycles()
        .expect("collect while async frame is suspended");
    let state = runtime
        .context(&realm)
        .expect("context")
        .call(&resume, &[state], ExecutionLimits::default())
        .expect("resume async function");
    let result = runtime
        .context(&realm)
        .expect("context")
        .call(&read, &[state], ExecutionLimits::default())
        .expect("read resumed result")
        .as_string()
        .expect("live result")
        .expect("string result")
        .to_utf8_lossy()
        .expect("UTF-8");

    assert_eq!(result, "resumed:8|then:9");
}
