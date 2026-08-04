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
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("async-generator.js"))
                    .expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("root function");
            let tree = context
                .compile_tree(&root, VerificationLimits::default())
                .unwrap_or_else(|error| {
                    panic!("verified async-generator tree for {source:?}: {error:?}")
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
        .expect("async-generator setup");
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
fn async_generator_call_is_deferred_and_next_returns_a_promise() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function* values(){\
                    state.result=state.result+'start|';\
                    yield 1;\
                    state.result=state.result+'resume|';\
                    return 2;\
                }\
                let iterator=values();\
                state.result=state.result+'called|';\
                let first=iterator.next();\
                state.result=state.result+'next|';\
                first.then(function(result){\
                    state.result=state.result+'first:'+result.value+':'+result.done+'|';\
                    iterator.next().then(function(final){\
                        state.result=state.result+'final:'+final.value+':'+final.done;\
                    });\
                });\
                return state;\
            }"
        ),
        "called|start|next|first:1:false|resume|final:2:true"
    );
}

#[test]
fn queued_next_requests_resume_in_fifo_order() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function* values(){\
                    let resumed=yield 1;\
                    yield resumed+1;\
                    return 9;\
                }\
                let iterator=values();\
                let first=iterator.next();\
                let second=iterator.next(7);\
                first.then(function(result){\
                    state.result=state.result+'first:'+result.value+':'+result.done+'|';\
                });\
                second.then(function(result){\
                    state.result=state.result+'second:'+result.value+':'+result.done+'|';\
                    iterator.next().then(function(final){\
                        state.result=state.result+'final:'+final.value+':'+final.done;\
                    });\
                });\
                return state;\
            }"
        ),
        "first:1:false|second:8:false|final:9:true"
    );
}

#[test]
fn invalid_receiver_rejects_the_returned_promise() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:'returned|'};\
                async function* values(){}\
                let next=values().next;\
                next.call({}).catch(function(error){\
                    state.result=state.result+error.name;\
                });\
                return state;\
            }"
        ),
        "returned|TypeError"
    );
}

#[test]
fn uncaught_body_throw_rejects_the_active_request() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:'returned|'};\
                async function* values(){throw 'boom';}\
                values().next().catch(function(error){\
                    state.result=state.result+'rejected:'+error;\
                });\
                return state;\
            }"
        ),
        "returned|rejected:boom"
    );
}

#[test]
fn rejected_await_is_thrown_at_the_await_site() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function* values(){\
                    try{await {then:function(resolve,reject){reject('reason');}};}\
                    catch(error){yield 'caught:'+error;}\
                }\
                values().next().then(function(result){\
                    state.result=result.value+':'+result.done;\
                });\
                return state;\
            }"
        ),
        "caught:reason:false"
    );
}

#[test]
fn return_awaits_thenables_before_running_finally() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function* values(){\
                    try{yield 1;}\
                    finally{state.result=state.result+'finally|';}\
                }\
                let iterator=values();\
                iterator.next().then(function(){\
                    let completion=iterator.return({then:function(resolve){\
                        state.result=state.result+'thenable|';resolve(8);\
                    }});\
                    state.result=state.result+'after-return|';\
                    completion.then(function(result){\
                        state.result=state.result+'return:'+result.value+':'+result.done;\
                    });\
                });\
                return state;\
            }"
        ),
        "after-return|thenable|finally|return:8:true"
    );
}

#[test]
fn completed_return_still_awaits_its_value() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function* values(){}\
                let iterator=values();\
                iterator.next().then(function(){\
                    let completion=iterator.return({then:function(resolve){\
                        state.result=state.result+'thenable|';resolve(4);\
                    }});\
                    state.result=state.result+'after-return|';\
                    completion.then(function(result){\
                        state.result=state.result+'return:'+result.value+':'+result.done;\
                    });\
                });\
                return state;\
            }"
        ),
        "after-return|thenable|return:4:true"
    );
}

#[test]
fn queued_return_awaits_before_resuming_the_generator() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function* values(){\
                    try{yield 1;}\
                    finally{state.result=state.result+'finally|';}\
                }\
                let iterator=values();\
                let first=iterator.next();\
                let returned=iterator.return({then:function(resolve){\
                    state.result=state.result+'thenable|';resolve(6);\
                }});\
                first.then(function(result){\
                    state.result=state.result+'first:'+result.value+':'+result.done+'|';\
                });\
                returned.then(function(result){\
                    state.result=state.result+'return:'+result.value+':'+result.done;\
                });\
                return state;\
            }"
        ),
        "first:1:false|thenable|finally|return:6:true"
    );
}

#[test]
fn queued_return_is_awaited_when_the_active_request_completes() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function* values(){await 1;return 2;}\
                let iterator=values();\
                let first=iterator.next();\
                let returned=iterator.return({then:function(resolve){\
                    state.result=state.result+'thenable|';resolve(5);\
                }});\
                first.then(function(result){\
                    state.result=state.result+'first:'+result.value+':'+result.done+'|';\
                });\
                returned.then(function(result){\
                    state.result=state.result+'return:'+result.value+':'+result.done;\
                });\
                return state;\
            }"
        ),
        "first:2:true|thenable|return:5:true"
    );
}

#[test]
fn return_await_rejection_runs_finally_and_rejects_the_request() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function* values(){\
                    try{yield 1;}\
                    finally{state.result=state.result+'finally|';}\
                }\
                let iterator=values();\
                iterator.next().then(function(){\
                    let returned=iterator.return({then:function(resolve,reject){\
                        state.result=state.result+'thenable|';reject('nope');\
                    }});\
                    state.result=state.result+'after-return|';\
                    returned.catch(function(reason){\
                        state.result=state.result+'rejected:'+reason;\
                    });\
                });\
                return state;\
            }"
        ),
        "after-return|thenable|finally|rejected:nope"
    );
}

#[test]
fn return_promise_resolve_abrupt_completion_rejects_without_throwing() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function* values(){\
                    try{yield 1;}\
                    finally{state.result=state.result+'finally|';}\
                }\
                let iterator=values();\
                let value=iterator.next();\
                value.then(function(){\
                    value.__defineGetter__('constructor',function(){\
                        state.result=state.result+'getter|';\
                        throw 'bad';\
                    });\
                    let returned;\
                    try{\
                        returned=iterator.return(value);\
                        state.result=state.result+'returned|';\
                    }catch(error){\
                        state.result=state.result+'sync:'+error+'|';\
                    }\
                    returned.catch(function(reason){\
                        state.result=state.result+'rejected:'+reason;\
                    });\
                });\
                return state;\
            }"
        ),
        "getter|finally|returned|rejected:bad"
    );
}

#[test]
fn completed_return_promise_resolve_abrupt_completion_rejects_request() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function* values(){}\
                let iterator=values();\
                let value=iterator.next();\
                value.then(function(){\
                    value.__defineGetter__('constructor',function(){\
                        state.result=state.result+'getter|';\
                        throw 'bad';\
                    });\
                    let returned;\
                    try{\
                        returned=iterator.return(value);\
                        state.result=state.result+'returned|';\
                    }catch(error){\
                        state.result=state.result+'sync:'+error+'|';\
                    }\
                    returned.catch(function(reason){\
                        state.result=state.result+'rejected:'+reason;\
                    });\
                });\
                return state;\
            }"
        ),
        "getter|returned|rejected:bad"
    );
}

#[test]
fn throw_is_catchable_and_completed_requests_follow_async_generator_rules() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                async function* values(){\
                    try{yield 1;}catch(error){yield 'caught:'+error;}\
                }\
                let iterator=values();\
                iterator.next().then(function(){\
                    iterator.throw('boom').then(function(caught){\
                        state.result=state.result+caught.value+':'+caught.done+'|';\
                        iterator.next().then(function(done){\
                            state.result=state.result+'next:'+done.value+':'+done.done+'|';\
                            iterator.throw('late').catch(function(reason){\
                                state.result=state.result+'throw:'+reason;\
                            });\
                        });\
                    });\
                });\
                return state;\
            }"
        ),
        "caught:boom:false|next:undefined:true|throw:late"
    );
}

#[test]
fn suspended_async_generator_survives_collection_through_its_request_promise() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let start = instantiate(
        &mut runtime,
        &realm,
        "function start(){\
            let state={result:'waiting'};\
            async function* values(){\
                yield {then:function(resolve){state.resume=resolve;}};\
            }\
            state.iterator=values();\
            state.promise=state.iterator.next();\
            state.promise.then(function(result){\
                state.result='yielded:'+result.value+':'+result.done;\
            });\
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
        .expect("suspended async generator");
    runtime
        .collect_cycles()
        .expect("collect while async generator is suspended");
    let state = runtime
        .context(&realm)
        .expect("context")
        .call(&resume, &[state], ExecutionLimits::default())
        .expect("resume async generator");
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

    assert_eq!(result, "yielded:8:false");
}

#[test]
fn async_generator_object_method_uses_the_async_generator_intrinsics() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                let holder={async *values(){yield 3;}};\
                holder.values().next().then(function(result){\
                    state.result=result.value+':'+result.done+'|'+\
                        holder.values.constructor.name;\
                });\
                return state;\
            }"
        ),
        "3:false|AsyncGeneratorFunction"
    );
}
