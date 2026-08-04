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
use std::sync::Arc;
use std::{error::Error, fmt};

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
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime Promise>"))
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

fn rendered(expression: &str) -> String {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, &format!("return String({expression});"));
    context
        .call(&run, &[], ExecutionLimits::default())
        .expect("completed")
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

fn turn_result(setup: &str, projection: &str) -> Result<String, ExecutionError> {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let setup = dynamic_function(&mut context, setup);
    let state = context.call(&setup, &[], ExecutionLimits::default())?;
    let project = dynamic_function(&mut context, projection);
    context
        .call(&project, &[state], ExecutionLimits::default())?
        .as_string()
        .expect("live projection")
        .expect("String projection")
        .to_utf8_lossy()
        .map_err(ExecutionError::from)
}

#[test]
fn promise_core_surface_and_constructor_validation_match_the_specification() {
    assert_eq!(
        rendered("Object.getOwnPropertyNames(Promise).join('|')"),
        "length|name|resolve|reject|prototype"
    );
    assert_eq!(
        rendered("Object.getOwnPropertyNames(Promise.prototype).join('|')"),
        "then|catch|finally|constructor"
    );
    assert_eq!(rendered("Promise.length+':'+Promise.name"), "1:Promise");
    assert_eq!(
        rendered("Promise.resolve.length+':'+Promise.reject.length"),
        "1:1"
    );
    assert_eq!(
        rendered(
            "Promise.prototype.then.length+':'+Promise.prototype.catch.length+':'+Promise.prototype.finally.length+':'+Promise.prototype.finally.name"
        ),
        "2:1:1:finally"
    );
    assert_eq!(
        rendered("(function(){try{Promise(function(){});}catch(e){return e.name;}})()"),
        "TypeError"
    );
    assert_eq!(
        rendered("(function(){try{new Promise(0);}catch(e){return e.name;}})()"),
        "TypeError"
    );
    assert_eq!(
        rendered("Object.prototype.toString.call(Promise.resolve())"),
        "[object Promise]"
    );
    assert_eq!(
        rendered(
            "(function(){function Sub(){};let prototype={};Sub.prototype=prototype;let promise=Reflect.construct(Promise,[function(resolve){resolve(1);}],Sub);return Object.getPrototypeOf(promise)===prototype;})()"
        ),
        "true"
    );
    assert_eq!(
        rendered(
            "(function(){function Sub(){};Sub.prototype=1;let promise=Reflect.construct(Promise,[function(resolve){resolve(1);}],Sub);return Object.getPrototypeOf(promise)===Promise.prototype;})()"
        ),
        "true"
    );
    assert_eq!(
        rendered(
            "(function(){let log='';let Sub=(function(){}).bind(null);Object.defineProperty(Sub,'prototype',{get:function(){log+='prototype|';return {};}});Reflect.construct(Promise,[function(){log+='executor';}],Sub);return log;})()"
        ),
        "prototype|executor"
    );
    assert_eq!(
        rendered(
            "(function(){let called=false;let Sub=(function(){}).bind(null);Object.defineProperty(Sub,'prototype',{get:function(){throw 'prototype';}});try{Reflect.construct(Promise,[function(){called=true;}],Sub);}catch(error){return error+':'+called;}})()"
        ),
        "prototype:false"
    );
    assert_eq!(
        turn_result(
            "try { Promise.prototype.then.call({}); } catch (error) { return error.name; }",
            "return arguments[0];",
        )
        .expect("directly caught Promise brand error"),
        "TypeError"
    );
}

#[test]
fn promise_finally_is_generic_and_observes_species_before_then() {
    assert_eq!(
        rendered(
            "(function(){let log='';function cleanup(){}let receiver={get constructor(){log+='constructor|';return {get [Symbol.species](){log+='species|';return Promise;}};},get then(){log+='then-get|';return function(onFulfilled,onRejected){log+='then-call:'+(this===receiver)+':'+onFulfilled.name+':'+onFulfilled.length+':'+onRejected.name+':'+onRejected.length;return 9;};}};let result=Promise.prototype.finally.call(receiver,cleanup);return log+'|'+result;})()"
        ),
        "constructor|species|then-get|then-call:true::1::1|9"
    );
    assert_eq!(
        rendered(
            "(function(){let log='';let receiver={constructor:undefined,then:function(onFulfilled,onRejected){log=typeof onFulfilled+':'+onFulfilled+':'+(onFulfilled===onRejected);return 4;}};let result=Promise.prototype.finally.call(receiver,7);return log+'|'+result;})()"
        ),
        "number:7:true|4"
    );
    assert_eq!(
        rendered(
            "(function(){try{Promise.prototype.finally.call(1,function(){});}catch(error){return error.name;}})()"
        ),
        "TypeError"
    );
}

#[test]
fn promise_finally_uses_the_captured_species_to_resolve_cleanup() {
    let actual = turn_result(
        "let box={log:''};\n\
         let value=Promise.resolve('value');\n\
         Object.defineProperty(value,'constructor',{get:function(){\n\
             box.log=box.log+'constructor|';\n\
             return {get [Symbol.species](){\n\
                 box.log=box.log+'species|';\n\
                 return function C(executor){box.log=box.log+'construct|';return new Promise(executor);};\n\
             }};\n\
         }});\n\
         let result=value.finally(function(){\n\
             'use strict';\n\
             box.log=box.log+'cleanup:'+arguments.length+':'+(this===undefined)+'|';\n\
             return {then:function(resolve){box.log=box.log+'thenable|';resolve('ignored');}};\n\
         });\n\
         result.then(function(settled){box.log=box.log+'settled:'+settled+'|';});\n\
         box.log=box.log+'sync|';\n\
         return box;",
        "return arguments[0].log;",
    )
    .expect("Promise finally species turn");
    assert_eq!(
        actual,
        "constructor|species|constructor|species|construct|sync|cleanup:0:true|construct|thenable|settled:value|"
    );
}

#[test]
fn promise_finally_preserves_or_overrides_the_original_completion() {
    let actual = turn_result(
        "let box={log:''};\n\
         Promise.resolve('keep').finally(function(){return 'ignored';}).then(function(value){box.log=box.log+'f:'+value+'|';});\n\
         Promise.reject('reason').finally(function(){return undefined;}).catch(function(reason){box.log=box.log+'r:'+reason+'|';});\n\
         Promise.resolve('old').finally(function(){throw 'new';}).catch(function(reason){box.log=box.log+'t:'+reason+'|';});\n\
         Promise.reject('old-reason').finally(function(){return Promise.reject('new-reason');}).catch(function(reason){box.log=box.log+'p:'+reason+'|';});\n\
         return box;",
        "return arguments[0].log;",
    )
    .expect("Promise finally completion turn");
    assert_eq!(actual, "t:new|f:keep|r:reason|p:new-reason|");
}

#[test]
fn promise_jobs_are_fifo_and_drain_nested_work_to_a_fixed_point() {
    let actual = turn_result(
        "let box={log:''};\n\
         let first=new Promise(function(resolve){box.log=box.log+'executor|';resolve(1);});\n\
         first.then(function(value){\n\
             box.log=box.log+'first'+value+'|';\n\
             Promise.resolve(3).then(function(value){box.log=box.log+'third'+value+'|';});\n\
         }).then(function(){box.log=box.log+'second|';});\n\
         box.log=box.log+'sync|';\n\
         return box;",
        "return arguments[0].log;",
    )
    .expect("two-turn Promise observation");
    assert_eq!(actual, "executor|sync|first1|third3|second|");
}

#[test]
fn promise_rejection_propagates_and_catch_recovers_asynchronously() {
    let actual = turn_result(
        "let box={value:'sync'};\n\
         Promise.reject('x')\n\
             .catch(function(reason){return reason+'y';})\n\
             .then(function(value){box.value=value;});\n\
         return box;",
        "return arguments[0].value;",
    )
    .expect("catch recovery");
    assert_eq!(actual, "xy");
}

#[test]
fn promise_resolution_reads_then_synchronously_but_calls_it_in_a_job() {
    let actual = turn_result(
        "let box={log:'before|'};\n\
         let thenable={get then(){\n\
             box.log=box.log+'get|';\n\
             return function(resolve,reject){box.log=box.log+'call|';resolve(4);reject(5);};\n\
         }};\n\
         Promise.resolve(thenable).then(function(value){box.log=box.log+'value'+value+'|';});\n\
         box.log=box.log+'after|';\n\
         return box;",
        "return arguments[0].log;",
    )
    .expect("thenable assimilation");
    assert_eq!(actual, "before|get|after|call|value4|");
}

#[test]
fn resolving_with_self_rejects_and_resolving_twice_is_ignored() {
    let actual = turn_result(
        "let box={self:'',once:''};\n\
         let resolveSelf;\n\
         let promise=new Promise(function(resolve){resolveSelf=resolve;});\n\
         resolveSelf(promise);\n\
         promise.catch(function(error){box.self=error.name;});\n\
         new Promise(function(resolve,reject){resolve(1);reject(2);resolve(3);})\n\
             .then(function(value){box.once=String(value);},function(reason){box.once='bad'+reason;});\n\
         return box;",
        "return arguments[0].self+'|'+arguments[0].once;",
    )
    .expect("one-shot resolution");
    assert_eq!(actual, "TypeError|1");
}

#[test]
fn promise_resolve_preserves_intrinsic_promise_identity() {
    assert_eq!(
        rendered(
            "(function(){let log='';let promise=Promise.resolve(1);Object.defineProperty(promise,'constructor',{get:function(){log+='get|';return Promise;}});let same=Promise.resolve(promise)===promise;return log+same;})()"
        ),
        "get|true"
    );
    assert_eq!(
        rendered(
            "(function(){let promise=Promise.resolve(1);Object.defineProperty(promise,'constructor',{value:function Other(){}});return Promise.resolve(promise)!==promise;})()"
        ),
        "true"
    );
    assert_eq!(
        rendered(
            "(function(){let promise=Promise.resolve(1);Object.defineProperty(promise,'constructor',{get:function(){throw 'constructor';}});try{Promise.resolve(promise);}catch(error){return error;}})()"
        ),
        "constructor"
    );
    let actual = turn_result(
        "let box={same:false,async:false};\n\
         let promise=Promise.resolve(7);\n\
         box.same=Promise.resolve(promise)===promise;\n\
         promise.then(function(){box.async=true;});\n\
         return box;",
        "return String(arguments[0].same)+'|'+String(arguments[0].async);",
    )
    .expect("Promise.resolve identity");
    assert_eq!(actual, "true|true");
}

#[test]
fn promise_static_methods_create_and_drive_generic_capabilities() {
    assert_eq!(
        rendered(
            "(function(){let log='';function C(executor){let result={};executor(function(value){'use strict';log+='resolve:'+value+':'+(this===undefined);result.value=value;},function(reason){result.reason=reason;});return result;}let result=Promise.resolve.call(C,7);return log+'|'+result.value;})()"
        ),
        "resolve:7:true|7"
    );
    assert_eq!(
        rendered(
            "(function(){let log='';function C(executor){let result={};executor(function(value){result.value=value;},function(reason){'use strict';log+='reject:'+reason+':'+(this===undefined);result.reason=reason;});return result;}let result=Promise.reject.call(C,'x');return log+'|'+result.reason;})()"
        ),
        "reject:x:true|x"
    );
    assert_eq!(
        rendered(
            "(function(){function C(executor){let result={};function settle(){}executor(settle,settle);try{executor(settle,settle);}catch(error){result.second=error.name;}return result;}return Promise.resolve.call(C,1).second;})()"
        ),
        "TypeError"
    );
    assert_eq!(
        rendered(
            "(function(){function C(executor){executor(0,function(){});return {};}try{Promise.resolve.call(C,1);}catch(error){return error.name;}})()"
        ),
        "TypeError"
    );
}

#[test]
fn promise_then_constructs_and_settles_the_selected_species_capability() {
    let actual = turn_result(
        "let box={log:''};\n\
         function C(executor){\n\
             box.log=box.log+'construct|';\n\
             let result={};\n\
             executor(\n\
                 function(value){box.log=box.log+'resolve:'+value+'|';result.value=value;},\n\
                 function(reason){box.log=box.log+'reject:'+reason+'|';result.reason=reason;}\n\
             );\n\
             return result;\n\
         }\n\
         let promise=Promise.resolve(1);\n\
         Object.defineProperty(promise,'constructor',{get:function(){\n\
             box.log=box.log+'constructor|';\n\
             return {get [Symbol.species](){box.log=box.log+'species|';return C;}};\n\
         }});\n\
         box.derived=promise.then(function(value){box.log=box.log+'handler|';return value+1;});\n\
         box.log=box.log+'sync|';\n\
         return box;",
        "return arguments[0].log+'#'+arguments[0].derived.value;",
    )
    .expect("species capability turn");
    assert_eq!(
        actual,
        "constructor|species|construct|sync|handler|resolve:2|#2"
    );
}

#[test]
fn promise_species_surface_fallback_and_validation_follow_the_specification() {
    assert_eq!(
        rendered(
            "(function(){let descriptor=Object.getOwnPropertyDescriptor(Promise,Symbol.species);return descriptor.get.name+':'+descriptor.get.length+':'+descriptor.enumerable+':'+descriptor.configurable+':'+(descriptor.set===undefined)+':'+(descriptor.get.call(7)===7);})()"
        ),
        "get [Symbol.species]:0:false:true:true:true"
    );
    assert_eq!(
        rendered(
            "(function(){let promise=Promise.resolve(1);promise.constructor={[Symbol.species]:null};return promise.then() instanceof Promise;})()"
        ),
        "true"
    );
    assert_eq!(
        rendered(
            "(function(){let promise=Promise.resolve(1);promise.constructor=null;try{promise.then();}catch(error){return error.name;}})()"
        ),
        "TypeError"
    );
    assert_eq!(
        rendered(
            "(function(){let promise=Promise.resolve(1);promise.constructor={[Symbol.species]:{}};try{promise.then();}catch(error){return error.name;}})()"
        ),
        "TypeError"
    );
}

#[test]
fn abrupt_executor_thenable_and_reaction_completions_reject_without_escaping_the_job_queue() {
    let actual = turn_result(
        "let box={log:''};\n\
         new Promise(function(resolve){resolve('executor');throw 'late';})\n\
             .then(function(value){box.log=box.log+value+'|';});\n\
         Promise.resolve({get then(){throw 'getter';}})\n\
             .catch(function(reason){box.log=box.log+reason+'|';});\n\
         Promise.resolve({then:function(resolve){resolve('thenable');throw 'ignored';}})\n\
             .then(function(value){box.log=box.log+value+'|';});\n\
         Promise.resolve('reaction')\n\
             .then(function(value){throw value;})\n\
             .catch(function(reason){box.log=box.log+reason+'|';});\n\
         return box;",
        "return arguments[0].log;",
    )
    .expect("abrupt Promise completions");
    assert_eq!(actual, "executor|getter|thenable|reaction|");
}

#[test]
fn escaping_javascript_exception_still_reaches_the_promise_job_checkpoint() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let setup = dynamic_function(
        &mut context,
        "globalThis.promiseTurnLog='sync';\n\
         Promise.resolve(1).then(function(){globalThis.promiseTurnLog='job';});\n\
         throw 'boom';",
    );
    assert!(matches!(
        context.call(&setup, &[], ExecutionLimits::default()),
        Err(ExecutionError::Exception(_))
    ));
    let project = dynamic_function(&mut context, "return globalThis.promiseTurnLog;");
    let actual = context
        .call(&project, &[], ExecutionLimits::default())
        .expect("post-abrupt turn observation")
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8");
    assert_eq!(actual, "job");
}

#[test]
fn promise_job_queue_limit_is_inclusive_and_drains_to_zero() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default().with_max_pending_promise_jobs(0))
        .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    {
        let mut context = runtime.context(&realm).expect("context");
        let run = dynamic_function(
            &mut context,
            "Promise.resolve(1).then(function(){}); return 0;",
        );
        assert!(matches!(
            context.call(&run, &[], ExecutionLimits::default()),
            Err(ExecutionError::LimitExceeded {
                resource: RuntimeResource::PromiseJobs,
                limit: 0,
                observed: 1,
            })
        ));
    }
    assert_eq!(runtime.usage().pending_promise_jobs(), 0);

    let mut runtime = Runtime::try_new(RuntimeLimits::default().with_max_pending_promise_jobs(1))
        .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    {
        let mut context = runtime.context(&realm).expect("context");
        let run = dynamic_function(
            &mut context,
            "let box={done:false}; Promise.resolve(1).then(function(){box.done=true;}); return box;",
        );
        let _ = context
            .call(&run, &[], ExecutionLimits::default())
            .expect("inclusive single job");
    }
    assert_eq!(runtime.usage().pending_promise_jobs(), 0);
}
