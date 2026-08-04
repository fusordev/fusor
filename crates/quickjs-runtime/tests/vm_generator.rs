use std::sync::Arc;

use quickjs_bytecode::VerificationLimits;
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    ExecutionError, ExecutionLimits, Function, JsNumber, JsValue, Realm, Runtime, RuntimeLimits,
    RuntimeResource, ValueKind,
};

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new_with_source_name(unit, Arc::from("generator.js"))
                .expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("root function");
            let tree = context
                .compile_tree(&root, VerificationLimits::default())
                .expect("verified generator tree");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn run(source: &str) -> String {
    let authority = compile(source, "run");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");
    context
        .call(&function, &[], ExecutionLimits::default())
        .expect("generator execution")
        .as_string()
        .expect("live value")
        .expect("string")
        .to_utf8_lossy()
        .expect("UTF-8")
}

struct GeneratorAllocationCase {
    runtime: Runtime,
    realm: Realm,
    setup: Function,
    maker: Function,
    resume: Function,
    read: Function,
    filler_factory: Function,
    state: JsValue,
    iterator: JsValue,
    filler: JsValue,
}

fn generator_allocation_case(limits: RuntimeLimits) -> GeneratorAllocationCase {
    let setup = compile("function setup(){return {hits:0};}", "setup");
    let maker = compile(
        "function make(state){\
            function* values(value=(state.hits=state.hits+1)){yield value;}\
            return values();\
        }",
        "make",
    );
    let resume = compile(
        "function resume(iterator){\
            let result=iterator.next();\
            return result.value+':'+result.done;\
        }",
        "resume",
    );
    let read = compile("function read(state){return state.hits;}", "read");
    let filler_factory = compile("function makeFiller(){return {};}", "makeFiller");
    let mut runtime = Runtime::try_new(limits).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let (setup, maker, resume, read, filler_factory, state, iterator, filler) = {
        let mut context = runtime.context(&realm).expect("context");
        let setup = context.instantiate(setup).expect("setup");
        let maker = context.instantiate(maker).expect("maker");
        let resume = context.instantiate(resume).expect("resume");
        let read = context.instantiate(read).expect("read");
        let filler_factory = context.instantiate(filler_factory).expect("filler factory");
        let state = context
            .call(&setup, &[], ExecutionLimits::default())
            .expect("state");
        let iterator = context
            .call(
                &maker,
                std::slice::from_ref(&state),
                ExecutionLimits::default(),
            )
            .expect("generator");
        let filler = context
            .call(&filler_factory, &[], ExecutionLimits::default())
            .expect("filler");
        (
            setup,
            maker,
            resume,
            read,
            filler_factory,
            state,
            iterator,
            filler,
        )
    };
    GeneratorAllocationCase {
        runtime,
        realm,
        setup,
        maker,
        resume,
        read,
        filler_factory,
        state,
        iterator,
        filler,
    }
}

#[test]
fn generator_initializes_parameters_before_resuming_next_return_and_finally_in_order() {
    assert_eq!(
        run("function run(){\
                let hits=0;\
                function side(){hits++;return 5;}\
                function* g(a=side()){\
                    try { let resumed=yield a; yield resumed+1; }\
                    finally { hits=hits+10; }\
                    return 99;\
                }\
                let iterator=g();\
                let before=hits;\
                let first=iterator.next(123);\
                let second=iterator.next(7);\
                let third=iterator.return(42);\
                return before+'|'+hits+'|'+first.value+':'+first.done+'|'+\
                    second.value+':'+second.done+'|'+third.value+':'+third.done;\
            }"),
        "1|11|5:false|8:false|42:true"
    );
}

#[test]
fn generator_throw_is_catchable_and_completed_generators_follow_resume_rules() {
    assert_eq!(
        run("function run(){\
                function* g(){try{yield 1;}catch(error){return error+1;}}\
                let iterator=g();\
                let first=iterator.next();\
                let second=iterator.throw(4);\
                let third=iterator.next();\
                let returned=iterator.return(9);\
                let thrown;\
                try{iterator.throw(7);}catch(error){thrown=error;}\
                return first.value+':'+first.done+'|'+second.value+':'+second.done+'|'+\
                    (''+third.value)+':'+third.done+'|'+returned.value+':'+returned.done+'|'+thrown;\
            }"),
        "1:false|5:true|undefined:true|9:true|7"
    );
}

#[test]
fn generator_function_and_instance_prototypes_match_the_intrinsic_chain() {
    assert_eq!(
        run("function run(){\
                let generator=function* named(){};\
                let iterator=generator();\
                let functionPrototype=generator.__proto__;\
                let generatorPrototype=generator.prototype.__proto__;\
                let tag=({}).toString;\
                return typeof generator+'|'+typeof functionPrototype+'|'+\
                    generator.hasOwnProperty('length')+','+generator.hasOwnProperty('name')+','+\
                    generator.hasOwnProperty('prototype')+'|'+\
                    (iterator.__proto__===generator.prototype)+'|'+\
                    (generatorPrototype.constructor===functionPrototype)+'|'+\
                    tag.call(functionPrototype)+'|'+tag.call(generatorPrototype);\
            }"),
        "function|object|true,true,true|true|true|[object GeneratorFunction]|[object Generator]"
    );
}

#[test]
fn generator_prestart_completion_reentrancy_and_uncaught_abrupt_are_stable() {
    assert_eq!(
        run("function run(){\
                let hits=0;\
                function side(){hits++;return 1;}\
                function* deferred(value=side()){yield value;}\
                let returned=deferred().return(4);\
                let thrown;\
                try{deferred().throw(6);}catch(error){thrown=error;}\
                let iterator;\
                function* reentrant(){\
                    try{iterator.next();}catch(error){return error.name;}\
                }\
                iterator=reentrant();\
                let running=iterator.next();\
                function* abrupt(){yield 1;throw 8;}\
                let failed=abrupt();\
                failed.next();\
                let escaped;\
                try{failed.next();}catch(error){escaped=error;}\
                let completed=failed.next();\
                return hits+'|'+returned.value+':'+returned.done+'|'+thrown+'|'+\
                    running.value+':'+running.done+'|'+escaped+'|'+(''+completed.value)+':'+completed.done;\
            }"),
        "2|4:true|6|TypeError:true|8|undefined:true"
    );
}

#[test]
fn generator_method_called_through_function_prototype_call_returns_an_iterator() {
    assert_eq!(
        run("function run(){\
                let object={*values(){yield 2;yield 3;}};\
                let direct=object.values.call(object);\
                return ''+direct.next().value;\
            }"),
        "2"
    );
}

#[test]
fn generator_method_called_through_function_prototype_apply_returns_an_iterator() {
    assert_eq!(
        run("function run(){\
                let object={*values(){yield 2;yield 3;}};\
                let applied=object.values.apply(object,[]);\
                return ''+applied.next().value;\
            }"),
        "2"
    );
}

#[test]
fn generator_method_is_not_constructable() {
    assert_eq!(
        run("function run(){\
                let object={*values(){yield 2;yield 3;}};\
                try{new object.values();}catch(error){return error.name;}\
                return 'missing TypeError';\
            }"),
        "TypeError"
    );
}

#[test]
fn generator_method_iterator_is_consumed_by_for_of() {
    assert_eq!(
        run("function run(){\
                let object={*values(){yield 2;yield 3;}};\
                let sum=0;\
                for(const value of object.values())sum=sum+value;\
                return ''+sum;\
            }"),
        "5"
    );
}

#[test]
fn generator_return_completion_survives_a_yielding_finally() {
    assert_eq!(
        run("function run(){\
                function* values(){try{yield 1;}finally{yield 2;}}\
                let iterator=values();\
                let first=iterator.next();\
                let second=iterator.return(9);\
                let third=iterator.next();\
                return first.value+':'+first.done+'|'+second.value+':'+second.done+'|'+\
                    third.value+':'+third.done;\
            }"),
        "1:false|2:false|9:true"
    );
}

#[test]
fn generator_return_closes_an_active_for_of_iterator() {
    assert_eq!(
        run("function run(){\
                let box={log:''};\
                function* inner(){try{while(true)yield 3;}finally{box.log=box.log+'close|';}}\
                function* values(){for(const value of inner())yield value;}\
                let iterator=values();\
                let first=iterator.next();\
                let second=iterator.return(7);\
                return first.value+':'+first.done+'|'+box.log+second.value+':'+second.done;\
            }"),
        "3:false|close|7:true"
    );
}

#[test]
fn generator_return_from_a_destructuring_default_runs_finally() {
    assert_eq!(
        run("function run(){\
                let log='';\
                function* values(){\
                    try{let [value=yield 1]=[void 0];yield value;}\
                    finally{log=log+'finally';}\
                }\
                let iterator=values();\
                let first=iterator.next();\
                let second=iterator.return(8);\
                return first.value+':'+first.done+'|'+log+'|'+second.value+':'+second.done;\
            }"),
        "1:false|finally|8:true"
    );
}

#[test]
fn suspended_generator_frames_trace_functions_cells_and_heap_values_until_completion() {
    let maker = compile(
        "function make(){\
            let box={marker:7};\
            function* values(){yield 1;return box.marker;}\
            let iterator=values();\
            iterator.next();\
            return iterator;\
        }",
        "make",
    );
    let resume = compile(
        "function resume(iterator){return iterator.next().value;}",
        "resume",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let (maker, resume, iterator) = {
        let mut context = runtime.context(&realm).expect("context");
        let maker = context.instantiate(maker).expect("maker");
        let resume = context.instantiate(resume).expect("resume");
        let iterator = context
            .call(&maker, &[], ExecutionLimits::default())
            .expect("suspended generator");
        (maker, resume, iterator)
    };
    assert_eq!(iterator.kind().expect("live iterator"), ValueKind::Object);

    runtime
        .collect_cycles()
        .expect("collection with suspended generator root");
    let result = runtime
        .context(&realm)
        .expect("context")
        .call(
            &resume,
            std::slice::from_ref(&iterator),
            ExecutionLimits::default(),
        )
        .expect("resume after collection");
    assert_eq!(
        result
            .as_number()
            .expect("live result")
            .map(JsNumber::as_f64),
        Some(7.0)
    );

    let completed = runtime
        .collect_cycles()
        .expect("completed generator collection");
    assert!(completed.functions() >= 1);
    assert!(completed.objects() >= 2);
    drop(iterator);
    drop(maker);
    drop(resume);
    let released = runtime
        .collect_cycles()
        .expect("released generator collection");
    assert!(released.objects() >= 1);
}

#[test]
fn rejected_generator_result_allocation_does_not_advance_the_generator() {
    let probe = generator_allocation_case(RuntimeLimits::default());
    let heap_limit = probe.runtime.usage().heap_objects();
    drop(probe);

    let GeneratorAllocationCase {
        mut runtime,
        realm,
        setup: _setup,
        maker: _maker,
        resume,
        read,
        filler_factory: _filler_factory,
        state,
        iterator,
        filler,
    } = generator_allocation_case(RuntimeLimits::default().with_max_heap_objects(heap_limit));
    {
        let mut context = runtime.context(&realm).expect("context");
        let error = context
            .call(
                &resume,
                std::slice::from_ref(&iterator),
                ExecutionLimits::default(),
            )
            .expect_err("iterator result exceeds the exact heap limit");
        assert!(matches!(
            error,
            ExecutionError::LimitExceeded {
                resource: RuntimeResource::HeapObjects,
                limit,
                observed,
            } if limit == heap_limit && observed == heap_limit + 1
        ));
        let hits = context
            .call(
                &read,
                std::slice::from_ref(&state),
                ExecutionLimits::default(),
            )
            .expect("read after rejected resume");
        assert!(
            hits.as_number()
                .expect("live hit count")
                .expect("number")
                .strict_equals(JsNumber::from_i32(1))
        );
    }

    drop(filler);
    runtime.collect_cycles().expect("release filler");
    let resumed = runtime
        .context(&realm)
        .expect("context")
        .call(
            &resume,
            std::slice::from_ref(&iterator),
            ExecutionLimits::default(),
        )
        .expect("retry the same first yield");
    assert_eq!(
        resumed
            .as_string()
            .expect("live resume result")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "1:false"
    );
}
