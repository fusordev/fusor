use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExecutionError, ExecutionLimits, Function, JsNumber,
    JsString, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource,
    OwnedPromiseRejectionEvent, PromiseRejectionEvent, PromiseRejectionOperation,
    PromiseRejectionTracker, Runtime, RuntimeLimits, RuntimeResource, ValueKind,
};
use std::cell::RefCell;
use std::rc::Rc;
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

#[derive(Default)]
struct RetainingRejectionTracker {
    events: RefCell<Vec<OwnedPromiseRejectionEvent>>,
}

impl PromiseRejectionTracker for RetainingRejectionTracker {
    fn promise_rejection(&self, mut event: PromiseRejectionEvent<'_>) {
        self.events
            .borrow_mut()
            .push(event.retain().expect("retain rejection event"));
    }
}

#[derive(Default)]
struct BorrowingRejectionTracker {
    events: RefCell<Vec<(PromiseRejectionOperation, ValueKind)>>,
}

impl PromiseRejectionTracker for BorrowingRejectionTracker {
    fn promise_rejection(&self, event: PromiseRejectionEvent<'_>) {
        self.events
            .borrow_mut()
            .push((event.operation(), event.reason().kind()));
    }
}

#[derive(Default)]
struct FailedRetainTracker {
    failed: RefCell<Vec<PromiseRejectionOperation>>,
}

impl PromiseRejectionTracker for FailedRetainTracker {
    fn promise_rejection(&self, mut event: PromiseRejectionEvent<'_>) {
        let operation = event.operation();
        if event.retain().is_err() {
            self.failed.borrow_mut().push(operation);
        }
    }
}

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
fn promise_rejection_tracker_reports_reject_then_only_the_first_handle() {
    let tracker = Rc::new(RetainingRejectionTracker::default());
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    assert!(!runtime.has_promise_rejection_tracker());
    runtime.set_promise_rejection_tracker(Rc::clone(&tracker) as Rc<dyn PromiseRejectionTracker>);
    assert!(runtime.has_promise_rejection_tracker());
    let realm = runtime.create_realm().expect("realm");
    {
        let mut context = runtime.context(&realm).expect("context");

        let reject = dynamic_function(
            &mut context,
            "let promise=Promise.reject({tag:'reason'});return promise;",
        );
        let promise = context
            .call(&reject, &[], ExecutionLimits::default())
            .expect("rejected Promise")
            .into_object()
            .expect("Promise object");
        {
            let events = tracker.events.borrow();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].operation(), PromiseRejectionOperation::Reject);
            assert!(!events[0].operation().is_handled());
            assert!(
                events[0]
                    .promise()
                    .same_identity(&promise)
                    .expect("identity")
            );
            assert_eq!(
                events[0].reason().kind().expect("live reason"),
                ValueKind::Object
            );
        }

        let handle = dynamic_function(
            &mut context,
            "arguments[0].catch(function(){});arguments[0].catch(function(){});return arguments[0];",
        );
        let returned = context
            .call(&handle, &[promise.as_value()], ExecutionLimits::default())
            .expect("handled Promise")
            .into_object()
            .expect("Promise object");
        {
            let events = tracker.events.borrow();
            assert_eq!(events.len(), 2);
            assert_eq!(events[1].operation(), PromiseRejectionOperation::Handle);
            assert!(events[1].operation().is_handled());
            assert!(
                events[0]
                    .promise()
                    .same_identity(events[1].promise())
                    .expect("event identity")
            );
            assert!(
                events[1]
                    .promise()
                    .same_identity(&returned)
                    .expect("returned identity")
            );
            let first_reason = events[0]
                .reason()
                .clone()
                .into_object()
                .expect("first reason object");
            let second_reason = events[1]
                .reason()
                .clone()
                .into_object()
                .expect("second reason object");
            assert!(
                first_reason
                    .same_identity(&second_reason)
                    .expect("reason identity")
            );
        }
    }

    runtime.collect_cycles().expect("collect retained events");
    assert_eq!(
        tracker.events.borrow()[0].reason().kind(),
        Ok(ValueKind::Object)
    );
    assert_eq!(runtime.usage().public_roots(), 4);
    runtime.clear_promise_rejection_tracker();
    assert!(!runtime.has_promise_rejection_tracker());
    tracker.events.borrow_mut().clear();
    runtime.collect_cycles().expect("collect released events");
    assert_eq!(runtime.usage().public_roots(), 0);
}

#[test]
fn promise_rejection_tracker_stays_silent_when_handled_before_rejection() {
    let tracker = Rc::new(BorrowingRejectionTracker::default());
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    runtime.set_promise_rejection_tracker(Rc::clone(&tracker) as Rc<dyn PromiseRejectionTracker>);
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let record=Promise.withResolvers();record.promise.catch(function(){});record.reject('late');return 1;",
    );
    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("pre-handled rejection");
    let actual = result.as_number().expect("live result").expect("Number");
    assert!(actual.strict_equals(JsNumber::from_i32(1)));
    assert!(tracker.events.borrow().is_empty());
}

#[test]
fn borrowed_rejection_notifications_add_no_public_or_gc_roots() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    runtime.set_promise_rejection_tracker(Rc::new({
        let events = Rc::clone(&events);
        move |event: PromiseRejectionEvent<'_>| {
            events
                .borrow_mut()
                .push((event.operation(), event.reason().kind()));
        }
    }));
    let realm = runtime.create_realm().expect("realm");
    {
        let mut context = runtime.context(&realm).expect("context");
        let run = dynamic_function(&mut context, "Promise.reject({});return 0;");
        let before = context.runtime_usage();
        let result = context
            .call(&run, &[], ExecutionLimits::default())
            .expect("unhandled rejection");
        let actual = result.as_number().expect("live result").expect("Number");
        assert!(actual.strict_equals(JsNumber::from_i32(0)));
        let after = context.runtime_usage();
        assert_eq!(after.public_roots(), before.public_roots());
        assert_eq!(after.pending_releases(), before.pending_releases());
    }
    assert_eq!(
        events.borrow().as_slice(),
        &[(PromiseRejectionOperation::Reject, ValueKind::Object)]
    );
}

#[test]
fn failed_host_retention_does_not_change_promise_rejection_semantics() {
    let tracker = Rc::new(FailedRetainTracker::default());
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_public_roots(2)).expect("runtime");
    runtime.set_promise_rejection_tracker(Rc::clone(&tracker) as Rc<dyn PromiseRejectionTracker>);
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(
        &mut context,
        "let promise=Promise.reject({});promise.catch(function(){});return 7;",
    );
    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("tracker retention failure is host-local");
    let actual = result.as_number().expect("live result").expect("Number");
    assert!(actual.strict_equals(JsNumber::from_i32(7)));
    assert_eq!(
        tracker.failed.borrow().as_slice(),
        &[
            PromiseRejectionOperation::Reject,
            PromiseRejectionOperation::Handle,
        ]
    );
    assert_eq!(context.runtime_usage().public_roots(), 1);
    assert_eq!(context.runtime_usage().pending_releases(), 0);
}

#[test]
fn promise_core_surface_and_constructor_validation_match_the_specification() {
    assert_eq!(
        rendered("Object.getOwnPropertyNames(Promise).join('|')"),
        "length|name|resolve|reject|all|allSettled|any|try|race|withResolvers|prototype"
    );
    assert_eq!(
        rendered("Object.getOwnPropertyNames(Promise.prototype).join('|')"),
        "then|catch|finally|constructor"
    );
    assert_eq!(rendered("Promise.length+':'+Promise.name"), "1:Promise");
    assert_eq!(
        rendered(
            "Promise.resolve.length+':'+Promise.reject.length+':'+Promise.all.length+':'+Promise.allSettled.length+':'+Promise.any.length+':'+Promise.try.length+':'+Promise.race.length+':'+Promise.withResolvers.length"
        ),
        "1:1:1:1:1:1:1:0"
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
fn promise_try_resolves_callback_results_and_rejects_abrupt_completions() {
    let actual = turn_result(
        "let box={log:''};\n\
         Promise.try(function(a,b){\n\
             'use strict';\n\
             box.log=box.log+'call:'+(this===undefined)+':'+a+':'+b+'|';\n\
             return a+b;\n\
         },2,3).then(function(value){box.log=box.log+'ok:'+value+'|';});\n\
         Promise.try(function(){throw 'boom';})\n\
             .catch(function(reason){box.log=box.log+'bad:'+reason+'|';});\n\
         return box;",
        "return arguments[0].log;",
    )
    .expect("Promise.try turn");
    assert_eq!(actual, "call:true:2:3|ok:5|bad:boom|");

    let invalid = turn_result(
        "let box={sync:true,name:''};\n\
         Promise.try(0).catch(function(error){box.name=error.name;});\n\
         box.sync=box.name==='';\n\
         return box;",
        "return arguments[0].sync+':'+arguments[0].name;",
    )
    .expect("Promise.try non-callable callback");
    assert_eq!(invalid, "true:TypeError");
}

#[test]
fn promise_with_resolvers_returns_one_generic_capability_record() {
    assert_eq!(
        rendered(
            "(function(){let record=Promise.withResolvers();let descriptor=Object.getOwnPropertyDescriptor(record,'promise');return Object.keys(record).join('|')+':'+record.promise.constructor.name+':'+record.resolve.length+':'+record.reject.length+':'+descriptor.writable+':'+descriptor.enumerable+':'+descriptor.configurable;})()"
        ),
        "promise|resolve|reject:Promise:1:1:true:true:true"
    );

    let actual = turn_result(
        "let box={value:'pending'};\n\
         let record=Promise.withResolvers();\n\
         record.promise.then(function(value){box.value=value;});\n\
         record.resolve(9);\n\
         record.reject('late');\n\
         return box;",
        "return String(arguments[0].value);",
    )
    .expect("Promise.withResolvers turn");
    assert_eq!(actual, "9");
}

#[test]
fn promise_combinators_settle_in_input_order_and_empty_cases_are_exact() {
    let actual = turn_result(
        "let box={log:'',race:false};\n\
         Promise.all([Promise.resolve(1),2])\n\
             .then(function(values){box.log=box.log+'all:'+values.join(',')+'|';});\n\
         Promise.allSettled([Promise.resolve(3),Promise.reject('x')])\n\
             .then(function(values){box.log=box.log+'settled:'+values[0].status+':'+values[0].value+':'+values[1].status+':'+values[1].reason+'|';});\n\
         Promise.any([Promise.reject('a'),Promise.resolve(4)])\n\
             .then(function(value){box.log=box.log+'any:'+value+'|';});\n\
         Promise.race([Promise.resolve(5),Promise.resolve(6)])\n\
             .then(function(value){box.log=box.log+'race:'+value+'|';});\n\
         Promise.race([]).then(function(){box.race=true;},function(){box.race=true;});\n\
         return box;",
        "return arguments[0].log+'#'+arguments[0].race;",
    )
    .expect("Promise combinator turn");
    assert_eq!(
        actual,
        "all:1,2|settled:fulfilled:3:rejected:x|any:4|race:5|#false"
    );

    let empty = turn_result(
        "let box={all:'',settled:'',any:''};\n\
         Promise.all([]).then(function(value){box.all=String(value.length);});\n\
         Promise.allSettled([]).then(function(value){box.settled=String(value.length);});\n\
         Promise.any([]).catch(function(error){\n\
             let descriptor=Object.getOwnPropertyDescriptor(error,'errors');\n\
             box.any=error.name+':'+Object.keys(error).join(',')+':'+descriptor.writable+':'+descriptor.enumerable+':'+descriptor.configurable+':'+error.errors.length;\n\
         });\n\
         return box;",
        "return arguments[0].all+'|'+arguments[0].settled+'|'+arguments[0].any;",
    )
    .expect("empty Promise combinators");
    assert_eq!(empty, "0|0|AggregateError::true:false:true:0");
}

#[test]
fn promise_all_settled_element_pair_shares_one_already_called_record() {
    assert_eq!(
        rendered(
            "(function(){function C(executor){let out={};executor(function(value){out.value=value;},function(reason){out.reason=reason;});return out;}C.resolve=function(value){return {then:function(onFulfilled,onRejected){onFulfilled(value);onRejected('late');}};};let out=Promise.allSettled.call(C,[1]);let item=out.value[0];let descriptor=Object.getOwnPropertyDescriptor(item,'status');return item.status+':'+item.value+':'+Object.keys(item).join(',')+':'+descriptor.writable+':'+descriptor.enumerable+':'+descriptor.configurable+':'+String(out.reason);})()"
        ),
        "fulfilled:1:status,value:true:true:true:undefined"
    );
}

#[test]
fn promise_combinator_element_metadata_and_any_error_order_follow_the_specification() {
    assert_eq!(
        rendered(
            "(function(){function C(executor){let out={};executor(function(value){out.value=value;},function(reason){out.reason=reason;});return out;}let log='';C.resolve=function(value){let promise={then:function(onFulfilled,onRejected){log=log+(this===promise)+':'+onFulfilled.name+':'+onFulfilled.length+':'+onRejected.name+':'+onRejected.length;onFulfilled(value);}};return promise;};Promise.allSettled.call(C,[1]);return log;})()"
        ),
        "true::1::1"
    );

    let actual = turn_result(
        "let box={value:''};\n\
         Promise.any([Promise.reject('first'),Promise.reject('second')])\n\
             .catch(function(error){\n\
                 let descriptor=Object.getOwnPropertyDescriptor(error,'errors');\n\
                 box.value=error.name+':'+error.errors.join(',')+':'+Object.getOwnPropertyNames(error).join(',')+':'+descriptor.writable+':'+descriptor.enumerable+':'+descriptor.configurable;\n\
             });\n\
         return box;",
        "return arguments[0].value;",
    )
    .expect("Promise.any rejection order");
    assert_eq!(actual, "AggregateError:first,second:errors:true:false:true");
}

#[test]
fn promise_combinators_get_resolve_before_the_iterator_and_close_on_abrupt() {
    assert_eq!(
        rendered(
            "(function(){let log='';function C(executor){log=log+'construct|';let out={};executor(function(value){log=log+'resolve-result|';out.value=value;},function(reason){log=log+'reject-result:'+reason+'|';out.reason=reason;});return out;}Object.defineProperty(C,'resolve',{get:function(){log=log+'resolve-get|';return function(value){log=log+'resolve-call:'+value+'|';throw 'boom';};}});let iterable={get [Symbol.iterator](){log=log+'iterator-get|';return function(){log=log+'iterator-call|';let done=false;return {next:function(){log=log+'next|';if(done)return {done:true};done=true;return {value:1,done:false};},return:function(){log=log+'return|';return {};}};};}};let out=Promise.all.call(C,iterable);return log+'#'+out.reason;})()"
        ),
        "construct|resolve-get|iterator-get|iterator-call|next|resolve-call:1|return|reject-result:boom|#boom"
    );
    assert_eq!(
        rendered(
            "(function(){let log='';function C(executor){let out={};executor(function(value){out.value=value;},function(reason){log=log+'reject:'+reason+'|';out.reason=reason;});return out;}C.resolve=function(){return {get then(){log=log+'then-get|';throw 'then';}};};let iterable={[Symbol.iterator]:function(){let done=false;return {next:function(){if(done)return {done:true};done=true;return {value:1,done:false};},return:function(){log=log+'return|';return {};}};}};let out=Promise.all.call(C,iterable);return log+'#'+out.reason;})()"
        ),
        "then-get|return|reject:then|#then"
    );
    assert_eq!(
        rendered(
            "(function(){let log='';function C(executor){let out={};executor(function(){throw 'resolve-throw';},function(reason){out.reason=reason;});return out;}C.resolve=function(value){return value;};let empty=Promise.all.call(C,[]);let iterable={[Symbol.iterator]:function(){let done=false;return {next:function(){if(done)return {done:true};done=true;return {value:1,done:false};},get return(){log=log+'return-get|';throw 'close-throw';}};}};C.resolve=function(){throw 'original';};let abrupt=Promise.all.call(C,iterable);return empty.reason+'|'+log+abrupt.reason;})()"
        ),
        "resolve-throw|return-get|original"
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
fn default_job_callbacks_capture_the_exact_callable_and_use_ordinary_call() {
    let actual = turn_result(
        "let box={log:''};\n\
         let thenable;\n\
         let selected=function(resolve){'use strict';box.log=box.log+'captured:'+(this===thenable)+'|';resolve(4);};\n\
         thenable={get then(){box.log=box.log+'get|';return selected;}};\n\
         Promise.resolve(thenable).then(function(value){'use strict';box.log=box.log+'value'+value+':'+(this===undefined)+'|';});\n\
         selected=function(resolve){box.log=box.log+'replacement|';resolve(5);};\n\
         let pair=Proxy.revocable(function(value){box.log=box.log+'proxy|';return value;},{});\n\
         Promise.resolve(1).then(pair.proxy).catch(function(error){box.log=box.log+'revoked:'+error.name+'|';});\n\
         pair.revoke();\n\
         box.log=box.log+'sync|';\n\
         return box;",
        "return arguments[0].log;",
    )
    .expect("default JobCallback semantics");
    assert_eq!(
        actual,
        "get|sync|captured:true|value4:true|revoked:TypeError|"
    );
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
