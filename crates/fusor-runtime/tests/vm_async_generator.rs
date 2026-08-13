use std::sync::Arc;

use fusor_bytecode::VerificationLimits;
use fusor_compiler::CompilationContext;
use fusor_frontend::{
    CompilationGoal, DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, FrontendOptions,
    GlobalScriptGoal, SourceFragment, with_dynamic_function_source, with_parsed_program,
};
use fusor_runtime::{
    ExecutionError, ExecutionLimits, Function, JsValue, Realm, Runtime, RuntimeLimits,
};

fn compile(source: &str, root_name: &str) -> Arc<fusor_bytecode::VerifiedBytecode> {
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

fn compile_dynamic(body: &str) -> Arc<fusor_bytecode::VerifiedBytecode> {
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
        .expect("dynamic async-generator function")
        .into_function()
        .expect("dynamic function result");
    let state = runtime
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
fn async_generator_class_methods_preserve_the_super_home_object_across_await() {
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
                    async *values(){let value=await 1;yield super['value']+=value;}\
                    static async *values(){let value=await 1;yield super['answer']+=value;}\
                }\
                let value=new Derived(3);\
                Derived._answer=4;\
                value.values().next().then(function(result){state.result=state.result+'instance:'+result.value+':'+result.done+'|';});\
                Derived.values().next().then(function(result){state.result=state.result+'static:'+result.value+':'+result.done;});\
                return state;\
            }"
        ),
        "instance:4:false|static:5:false"
    );
}

#[test]
fn static_private_async_generator_methods_preserve_the_home_object_across_await() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                class Base{static value(){return 2;}}\
                class Derived extends Base{\
                    static async *#values(){let increment=await 1;yield super.value()+increment;}\
                    static values(){return this.#values();}\
                    static privateName(){return this.#values.name;}\
                }\
                Derived.values().next().then(function(result){\
                    state.result=result.value+':'+result.done+'|'+Derived.privateName();\
                });\
                return state;\
            }"
        ),
        "3:false|#values"
    );
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
fn delegated_async_iterator_is_preferred_and_receives_resume_values() {
    assert_eq!(
        dynamic_start_and_read(
            "\
                let state={result:'',count:0};\
                let iterable={\
                [Symbol.iterator]:function(){\
                    state.result=state.result+'sync|';\
                    return {next:function(){return {value:'wrong',done:true};}};\
                },\
                [Symbol.asyncIterator]:function(){\
                    state.result=state.result+'async|';\
                    return {next:function(value){\
                        state.count=state.count+1;\
                        state.result=state.result+'next:'+arguments.length+':'+value+'|';\
                        if(state.count===1)return {then:function(resolve){resolve({value:3,done:false});}};\
                        return {then:function(resolve){resolve({value:9,done:true});}};\
                    }};\
                }};\
                async function* outer(){return yield* iterable;}\
                let iterator=outer();\
                iterator.next().then(function(first){\
                    state.result=state.result+'first:'+first.value+':'+first.done+'|';\
                    return iterator.next(7);\
                }).then(function(final){\
                    state.result=state.result+'final:'+final.value+':'+final.done;\
                }).catch(function(error){\
                    state.result='error:'+error.name+':'+error.message;\
                });\
                return state;"
        ),
        "async|next:1:undefined|first:3:false|next:1:7|final:9:true"
    );
}

#[test]
fn delegated_sync_iterator_uses_async_from_sync_value_unwrapping() {
    assert_eq!(
        dynamic_start_and_read(
            "\
                let state={result:'',count:0};\
                let iterable={[Symbol.iterator]:function(){\
                    return {next:function(value){\
                        state.count=state.count+1;\
                        state.result=state.result+'next:'+value+'|';\
                        if(state.count===1)return {value:{then:function(resolve){resolve(3);}},done:false};\
                        return {value:{then:function(resolve){resolve(8);}},done:true};\
                    }};\
                }};\
                async function* outer(){return yield* iterable;}\
                let iterator=outer();\
                iterator.next().then(function(first){\
                    state.result=state.result+'first:'+first.value+':'+first.done+'|';\
                    return iterator.next(5);\
                }).then(function(final){\
                    state.result=state.result+'final:'+final.value+':'+final.done;\
                }).catch(function(error){\
                    state.result='error:'+error.name+':'+error.message;\
                });\
                return state;"
        ),
        "next:undefined|first:3:false|next:5|final:8:true"
    );
}

#[test]
fn delegated_sync_missing_throw_closes_before_rejecting_type_error() {
    assert_eq!(
        dynamic_start_and_read(
            "\
                let state={result:''};\
                let iterable={[Symbol.iterator]:function(){\
                    return {\
                        next:function(){return {value:1,done:false};},\
                        return:function(){\
                            state.result=state.result+'close:'+arguments.length+'|';\
                            return {value:undefined,done:true};\
                        }\
                    };\
                }};\
                async function* outer(){yield* iterable;}\
                let iterator=outer();\
                iterator.next().then(function(){\
                    return iterator.throw('boom').catch(function(error){\
                        state.result=state.result+'error:'+error.name;\
                    });\
                });\
                return state;"
        ),
        "close:0|error:TypeError"
    );
}

#[test]
fn delegated_sync_value_rejection_closes_and_preserves_the_reason() {
    assert_eq!(
        dynamic_start_and_read(
            "\
                let state={result:''};\
                let iterable={[Symbol.iterator]:function(){\
                    return {\
                        next:function(){\
                            return {\
                                value:{then:function(resolve,reject){reject('bad');}},\
                                done:false\
                            };\
                        },\
                        return:function(){\
                            state.result=state.result+'close|';\
                            throw 'ignored';\
                        }\
                    };\
                }};\
                async function* outer(){yield* iterable;}\
                outer().next().catch(function(reason){\
                    state.result=state.result+'reason:'+reason;\
                });\
                return state;"
        ),
        "close|reason:bad"
    );
}

#[test]
fn delegated_sync_promise_resolve_abrupt_closes_and_preserves_the_reason() {
    assert_eq!(
        dynamic_start_and_read(
            "\
                let state={result:''};\
                let value=Promise.resolve(1);\
                Object.defineProperty(value,'constructor',{get:function(){\
                    state.result=state.result+'getter|';\
                    throw 'bad';\
                }});\
                let iterable={[Symbol.iterator]:function(){\
                    return {\
                        next:function(){return {value:value,done:false};},\
                        return:function(){\
                            state.result=state.result+'close|';\
                            return {done:true};\
                        }\
                    };\
                }};\
                async function* outer(){yield* iterable;}\
                outer().next().catch(function(reason){\
                    state.result=state.result+'reason:'+reason;\
                });\
                return state;"
        ),
        "getter|close|reason:bad"
    );
}

#[test]
fn delegated_async_throw_is_forwarded_and_can_yield_again() {
    assert_eq!(
        dynamic_start_and_read(
            "\
                let state={result:''};\
                let iterable={[Symbol.asyncIterator]:function(){\
                    return {\
                        next:function(){return {then:function(resolve){resolve({value:1,done:false});}};},\
                        throw:function(reason){\
                            state.result=state.result+'throw:'+reason+'|';\
                            return {then:function(resolve){resolve({value:2,done:false});}};\
                        }\
                    };\
                }};\
                async function* outer(){yield* iterable;}\
                let iterator=outer();\
                iterator.next().then(function(){\
                    return iterator.throw('boom');\
                }).then(function(result){\
                    state.result=state.result+'yield:'+result.value+':'+result.done;\
                });\
                return state;"
        ),
        "throw:boom|yield:2:false"
    );
}

#[test]
fn delegated_async_missing_throw_awaits_close_then_rejects_type_error() {
    assert_eq!(
        dynamic_start_and_read(
            "\
                let state={result:''};\
                let iterable={[Symbol.asyncIterator]:function(){\
                    return {\
                        next:function(){return {then:function(resolve){resolve({value:1,done:false});}};},\
                        return:function(){\
                            state.result=state.result+'close:'+arguments.length+'|';\
                            return {then:function(resolve){\
                                state.result=state.result+'await-close|';\
                                resolve({value:4,done:true});\
                            }};\
                        }\
                    };\
                }};\
                async function* outer(){yield* iterable;}\
                let iterator=outer();\
                iterator.next().then(function(){\
                    return iterator.throw('boom').catch(function(error){\
                        state.result=state.result+'error:'+error.name;\
                    });\
                });\
                return state;"
        ),
        "close:0|await-close|error:TypeError"
    );
}

#[test]
fn delegated_async_return_awaits_input_and_forwards_non_done_results() {
    assert_eq!(
        dynamic_start_and_read(
            "\
                let state={result:'',count:0};\
                let iterable={[Symbol.asyncIterator]:function(){\
                    return {\
                        next:function(value){\
                            state.count=state.count+1;\
                            state.result=state.result+'next:'+value+'|';\
                            if(state.count===1)return {then:function(resolve){resolve({value:1,done:false});}};\
                            return {then:function(resolve){resolve({value:9,done:true});}};\
                        },\
                        return:function(value){\
                            state.result=state.result+'return:'+value+'|';\
                            return {then:function(resolve){resolve({value:4,done:false});}};\
                        }\
                    };\
                }};\
                async function* outer(){return yield* iterable;}\
                let iterator=outer();\
                iterator.next().then(function(){\
                    return iterator.return({then:function(resolve){\
                        state.result=state.result+'thenable|';resolve(6);\
                    }});\
                }).then(function(returned){\
                    state.result=state.result+'yield:'+returned.value+':'+returned.done+'|';\
                    return iterator.next(7);\
                }).then(function(final){\
                    state.result=state.result+'final:'+final.value+':'+final.done;\
                });\
                return state;"
        ),
        "next:undefined|thenable|return:6|yield:4:false|next:7|final:9:true"
    );
}

#[test]
fn delegated_async_return_done_preserves_the_raw_result_value() {
    assert_eq!(
        dynamic_start_and_read(
            "\
                let state={result:''};\
                let value={then:function(resolve){state.result=state.result+'assimilated|';resolve(4);}};\
                let iterable={[Symbol.asyncIterator]:function(){\
                    return {\
                        next:function(){return Promise.resolve({value:1,done:false});},\
                        return:function(){return Promise.resolve({value:value,done:true});}\
                    };\
                }};\
                async function* outer(){yield* iterable;}\
                let iterator=outer();\
                iterator.next().then(function(){\
                    return iterator.return(6);\
                }).then(function(result){\
                    state.result=state.result+'same:'+(result.value===value)+':'+result.done;\
                });\
                return state;"
        ),
        "same:true:true"
    );
}

#[test]
fn delegated_iterators_read_done_before_value() {
    assert_eq!(
        dynamic_start_and_read(
            "\
                let state={result:''};\
                let asyncResult={\
                    get done(){state.result=state.result+'async-done|';return false;},\
                    get value(){state.result=state.result+'async-value|';return 3;}\
                };\
                let syncResult={\
                    get done(){state.result=state.result+'sync-done|';return false;},\
                    get value(){state.result=state.result+'sync-value|';return 4;}\
                };\
                let asyncIterable={[Symbol.asyncIterator]:function(){\
                    return {next:function(){return {then:function(resolve){resolve(asyncResult);}};}};\
                }};\
                let syncIterable={[Symbol.iterator]:function(){\
                    return {next:function(){return syncResult;}};\
                }};\
                async function* delegate(value){yield* value;}\
                delegate(asyncIterable).next().then(function(first){\
                    state.result=state.result+'async-yield:'+first.value+'|';\
                    return delegate(syncIterable).next();\
                }).then(function(second){\
                    state.result=state.result+'sync-yield:'+second.value;\
                });\
                return state;"
        ),
        "async-done|async-value|async-yield:3|sync-done|sync-value|sync-yield:4"
    );
}

#[test]
fn delegated_async_iterator_does_not_assimilate_the_result_value() {
    assert_eq!(
        dynamic_start_and_read(
            "\
                let state={result:''};\
                let value={then:function(resolve){state.result=state.result+'assimilated|';resolve(3);}};\
                let iterable={[Symbol.asyncIterator]:function(){\
                    return {next:function(){return {then:function(resolve){\
                        resolve({value:value,done:false});\
                    }};}};\
                }};\
                async function* outer(){yield* iterable;}\
                outer().next().then(function(result){\
                    state.result=state.result+'same:'+(result.value===value);\
                });\
                return state;"
        ),
        "same:true"
    );
}

#[test]
fn delegated_iterator_methods_must_produce_objects() {
    assert_eq!(
        dynamic_start_and_read(
            "\
                let state={result:''};\
                let asyncIterable={[Symbol.asyncIterator]:function(){\
                    return {next:function(){return {then:function(resolve){resolve(1);}};}};\
                }};\
                let syncIterable={[Symbol.iterator]:function(){\
                    return {next:function(){return 1;}};\
                }};\
                async function* delegate(value){yield* value;}\
                delegate(asyncIterable).next().catch(function(error){\
                    state.result=state.result+'async:'+error.name+'|';\
                    return delegate(syncIterable).next().catch(function(syncError){\
                        state.result=state.result+'sync:'+syncError.name;\
                    });\
                });\
                return state;"
        ),
        "async:TypeError|sync:TypeError"
    );
}

#[test]
fn delegated_sync_return_rejection_does_not_close_twice() {
    assert_eq!(
        dynamic_start_and_read(
            "\
                let state={result:''};\
                let iterable={[Symbol.iterator]:function(){\
                    return {\
                        next:function(){return {value:1,done:false};},\
                        return:function(value){\
                            state.result=state.result+'return:'+value+'|';\
                            return {\
                                value:{then:function(resolve,reject){reject('bad');}},\
                                done:false\
                            };\
                        }\
                    };\
                }};\
                async function* outer(){yield* iterable;}\
                let iterator=outer();\
                iterator.next().then(function(){\
                    return iterator.return(6).catch(function(reason){\
                        state.result=state.result+'reason:'+reason;\
                    });\
                });\
                return state;"
        ),
        "return:6|reason:bad"
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
fn async_generator_host_fault_preserves_the_original_error() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let make = instantiate(
        &mut runtime,
        &realm,
        "function make(){return {result:''};}",
        "make",
    );
    let start = instantiate(
        &mut runtime,
        &realm,
        "function start(holder){\
            async function* values(){while(true){}}\
            holder.iterator=values();\
            holder.promise=holder.iterator.next();\
        }",
        "start",
    );
    let resume = instantiate(
        &mut runtime,
        &realm,
        "function resume(holder){\
            holder.iterator.next().then(function(result){\
                holder.result=result.value+':'+result.done;\
            });\
            return holder;\
        }",
        "resume",
    );
    let read = instantiate(
        &mut runtime,
        &realm,
        "function read(holder){return holder.result;}",
        "read",
    );
    let holder = runtime
        .context(&realm)
        .expect("context")
        .call(&make, &[], ExecutionLimits::default())
        .expect("holder");

    let error = runtime
        .context(&realm)
        .expect("context")
        .call(
            &start,
            std::slice::from_ref(&holder),
            ExecutionLimits::default().with_instruction_fuel(64),
        )
        .expect_err("async-generator execution must exhaust the host budget");
    assert!(matches!(
        error,
        ExecutionError::InstructionLimitExceeded { limit: 64, .. }
    ));

    let holder = runtime
        .context(&realm)
        .expect("context")
        .call(&resume, &[holder], ExecutionLimits::default())
        .expect("completed generator remains reusable");
    let result = runtime
        .context(&realm)
        .expect("context")
        .call(&read, &[holder], ExecutionLimits::default())
        .expect("read completed generator result")
        .as_string()
        .expect("live result")
        .expect("string result")
        .to_utf8_lossy()
        .expect("UTF-8");
    assert_eq!(result, "undefined:true");
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
#[allow(
    clippy::too_many_lines,
    reason = "the GC regression keeps setup, forced collection, resumption, and observable assertions together"
)]
fn suspended_async_from_sync_delegation_survives_collection() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let start = runtime
        .context(&realm)
        .expect("context")
        .execute_dynamic_function_script(
            compile_dynamic(
                "\
                    let state={result:'waiting|'};\
                    let value={then:function(resolve){state.result=state.result+'then|';state.resume=resolve;}};\
                    let iterable={[Symbol.iterator]:function(){\
                        state.result=state.result+'iterator|';\
                        return {next:function(){state.result=state.result+'next|';return {value:value,done:false};}};\
                    }};\
                    async function* outer(){yield* iterable;}\
                    state.iterator=outer();\
                    state.promise=state.iterator.next();\
                    state.promise.then(function(result){\
                        state.result='yielded:'+result.value+':'+result.done;\
                    });\
                    return state;",
            ),
            ExecutionLimits::default(),
        )
        .expect("dynamic delegation setup")
        .into_function()
        .expect("dynamic start function");
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
    let read_resume_type = instantiate(
        &mut runtime,
        &realm,
        "function readResumeType(state){return typeof state.resume;}",
        "readResumeType",
    );

    let state = runtime
        .context(&realm)
        .expect("context")
        .call(&start, &[], ExecutionLimits::default())
        .expect("suspended async-from-sync delegation");
    let progress = runtime
        .context(&realm)
        .expect("context")
        .call(
            &read,
            std::slice::from_ref(&state),
            ExecutionLimits::default(),
        )
        .expect("drain the queued thenable job")
        .as_string()
        .expect("live progress")
        .expect("progress string")
        .to_utf8_lossy()
        .expect("UTF-8");
    assert_eq!(progress, "waiting|iterator|next|then|");
    let resume_type = runtime
        .context(&realm)
        .expect("context")
        .call(
            &read_resume_type,
            std::slice::from_ref(&state),
            ExecutionLimits::default(),
        )
        .expect("read resolver type before collection")
        .as_string()
        .expect("live resolver type")
        .expect("resolver type string")
        .to_utf8_lossy()
        .expect("UTF-8");
    assert_eq!(resume_type, "function");
    runtime
        .collect_cycles()
        .expect("collect while async-from-sync value is pending");
    let resume_type = runtime
        .context(&realm)
        .expect("context")
        .call(
            &read_resume_type,
            std::slice::from_ref(&state),
            ExecutionLimits::default(),
        )
        .expect("read resolver type after collection")
        .as_string()
        .expect("live resolver type")
        .expect("resolver type string")
        .to_utf8_lossy()
        .expect("UTF-8");
    assert_eq!(resume_type, "function");
    let state = runtime
        .context(&realm)
        .expect("context")
        .call(&resume, &[state], ExecutionLimits::default())
        .expect("resolve delegated value");
    let result = runtime
        .context(&realm)
        .expect("context")
        .call(&read, &[state], ExecutionLimits::default())
        .expect("read resumed delegation")
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

#[test]
fn async_generator_class_method_uses_the_async_generator_intrinsics() {
    assert_eq!(
        start_and_read(
            "function start(){\
                let state={result:''};\
                class Holder{async *values(){yield 3;}}\
                let holder=new Holder;\
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
