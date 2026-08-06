use std::sync::Arc;

use quickjs_bytecode::VerificationLimits;
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
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
