use std::sync::Arc;

use quickjs_bytecode::FunctionTemplateId;
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    ExceptionKind, ExecutionError, ExecutionLimits, JsNumber, Runtime, RuntimeLimits, ValueKind,
};

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("runtime-calls.js"))
                    .expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("root function");
            let tree = context
                .compile_tree(&root, quickjs_bytecode::VerificationLimits::default())
                .expect("verified function tree");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn runtime() -> Runtime {
    Runtime::try_new(RuntimeLimits::default()).expect("runtime")
}

fn assert_number(value: &quickjs_runtime::JsValue, expected: i32) {
    let actual = value.as_number().expect("live value").expect("number");
    assert!(actual.strict_equals(JsNumber::from_i32(expected)));
}

fn reserved_frame_values(
    authority: &quickjs_bytecode::VerifiedBytecode,
    function: FunctionTemplateId,
) -> u64 {
    let control_flow = authority
        .function(function)
        .expect("function")
        .function()
        .control_flow();
    let domains = control_flow.domains();
    u64::from(domains.argument_count())
        + u64::from(domains.local_count())
        + u64::from(control_flow.computed_stack_size())
}

#[test]
fn direct_closure_calls_execute_on_iterative_frames() {
    let authority = compile(
        "function outer(value){\
            function identity(input){return input;}\
            return identity(value);\
        }",
        "outer",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(authority).expect("outer");
    let argument = context.number(JsNumber::from_i32(42));

    let result = context
        .call(&outer, &[argument], ExecutionLimits::default())
        .expect("nested call");
    assert_number(&result, 42);
}

#[test]
fn every_direct_call_encoding_executes_and_can_feed_the_next_callee() {
    let authority = compile(
        "function run(){\
            function zero(){return one;}\
            function one(a){return two;}\
            function two(a,b){return three;}\
            function three(a,b,c){return four;}\
            function four(a,b,c,d){return d;}\
            return zero()(1)(1,2)(1,2,3)(1,2,3,4);\
        }",
        "run",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("call0 through call3 and full call");
    assert_number(&result, 4);
}

#[test]
fn higher_order_calls_cross_installed_code_and_realms_in_one_runtime() {
    let apply = compile(
        "function apply(callback,value){return callback(value);}",
        "apply",
    );
    let identity = compile("function identity(value){return value;}", "identity");
    let mut runtime = runtime();
    let apply_realm = runtime.create_realm().expect("apply realm");
    let identity_realm = runtime.create_realm().expect("identity realm");
    let apply = runtime
        .context(&apply_realm)
        .expect("apply context")
        .instantiate(apply)
        .expect("apply");
    let identity = runtime
        .context(&identity_realm)
        .expect("identity context")
        .instantiate(identity)
        .expect("identity");
    let mut context = runtime.context(&apply_realm).expect("context");
    let argument = context.number(JsNumber::from_i32(7));

    let result = context
        .call(
            &apply,
            &[identity.as_value(), argument],
            ExecutionLimits::default(),
        )
        .expect("higher-order call");
    assert_number(&result, 7);
}

#[test]
fn callee_and_arguments_follow_javascript_evaluation_order() {
    let authority = compile(
        "function run(){\
            let callee=first;\
            let firstFinished=false;\
            function first(){return 1;}\
            function second(){return 2;}\
            function changeCallee(){callee=second;firstFinished=true;return 0;}\
            function observe(){return firstFinished;}\
            function takeSecond(firstValue,secondValue){return secondValue;}\
            let selected=callee(changeCallee());\
            if(selected===1){return takeSecond(selected,observe());}\
            return false;\
        }",
        "run",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("ordered calls");
    assert_eq!(result.as_boolean().expect("live value"), Some(true));
}

#[test]
fn nested_calls_fill_missing_formals_and_discard_unobservable_extras() {
    let authority = compile(
        "function run(selectMissing){\
            function missing(value){return value;}\
            function extra(first){return first;}\
            if(selectMissing){return missing();}\
            return extra(9,8,7,6);\
        }",
        "run",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let select_missing = context.boolean(true);
    let missing = context
        .call(&run, &[select_missing], ExecutionLimits::default())
        .expect("missing argument");
    assert_eq!(missing.kind().expect("live value"), ValueKind::Undefined);

    let select_extra = context.boolean(false);
    let extra = context
        .call(&run, &[select_extra], ExecutionLimits::default())
        .expect("extra arguments");
    assert_number(&extra, 9);
}

#[test]
fn non_callable_throws_exact_type_error_at_call_site() {
    let source = "function fail(){return (1)();}";
    let authority = compile(source, "fail");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let fail = context.instantiate(authority).expect("fail");

    let ExecutionError::Exception(exception) = context
        .call(&fail, &[], ExecutionLimits::default())
        .expect_err("number is not callable")
    else {
        panic!("expected JavaScript exception");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
    assert_eq!(
        exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("message"),
        "not a function"
    );
    assert_eq!(exception.function(), FunctionTemplateId::new(0));
    assert_eq!(exception.source_name(), "runtime-calls.js");
    assert_eq!(exception.source_text(), source);
    assert!(exception.caller_frames().is_empty());
    let span = exception.source_span();
    assert_eq!(&source[span.start() as usize..span.end() as usize], "(1)()");
}

#[test]
fn non_callable_check_happens_after_argument_evaluation() {
    let authority = compile(
        "function make(){\
            let changed=false;\
            function set(){changed=true;return 0;}\
            function fail(){return (1)(set());}\
            function get(){return changed;}\
            function choose(wantFail){if(wantFail){return fail;}return get;}\
            return choose;\
        }",
        "make",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let make = context.instantiate(authority).expect("make");
    let choose = context
        .call(&make, &[], ExecutionLimits::default())
        .expect("choose")
        .into_function()
        .expect("choose function");
    let want_fail = context.boolean(true);
    let fail = context
        .call(&choose, &[want_fail], ExecutionLimits::default())
        .expect("fail")
        .into_function()
        .expect("fail function");
    let want_get = context.boolean(false);
    let get = context
        .call(&choose, &[want_get], ExecutionLimits::default())
        .expect("get")
        .into_function()
        .expect("get function");

    assert!(matches!(
        context
            .call(&fail, &[], ExecutionLimits::default())
            .expect_err("number remains non-callable"),
        ExecutionError::Exception(ref exception)
            if exception.kind() == Some(ExceptionKind::TypeError)
    ));
    let changed = context
        .call(&get, &[], ExecutionLimits::default())
        .expect("argument side effect");
    assert_eq!(changed.as_boolean().expect("live value"), Some(true));
}

#[test]
fn child_exceptions_keep_origin_and_record_parked_callers() {
    let source = "function outer(){\
            function child(){return lexical;let lexical;}\
            return child();\
        }";
    let authority = compile(source, "outer");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(authority).expect("outer");

    let ExecutionError::Exception(exception) = context
        .call(&outer, &[], ExecutionLimits::default())
        .expect_err("child TDZ")
    else {
        panic!("expected JavaScript exception");
    };
    assert_eq!(exception.kind(), Some(ExceptionKind::ReferenceError));
    assert_eq!(exception.function(), FunctionTemplateId::new(1));
    assert_eq!(exception.source_text(), source);
    let callers = exception.caller_frames();
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0].function(), FunctionTemplateId::new(0));
    assert_eq!(callers[0].source_name(), "runtime-calls.js");
    assert_eq!(callers[0].source_text(), source);
    let span = callers[0].source_span();
    assert_eq!(
        &source[span.start() as usize..span.end() as usize],
        "child()"
    );
}

#[test]
fn three_level_exception_trace_orders_callers_immediate_to_outermost() {
    let source = "function outer(){\
        function middle(){\
            function inner(){return lexical;let lexical;}\
            return inner();\
        }\
        return middle();\
    }";
    let authority = compile(source, "outer");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(authority).expect("outer");

    let ExecutionError::Exception(exception) = context
        .call(&outer, &[], ExecutionLimits::default())
        .expect_err("inner TDZ")
    else {
        panic!("expected JavaScript exception");
    };
    assert_eq!(exception.function(), FunctionTemplateId::new(2));
    let callers = exception.caller_frames();
    assert_eq!(callers.len(), 2);
    let immediate = callers[0].source_span();
    assert_eq!(
        &source[immediate.start() as usize..immediate.end() as usize],
        "inner()"
    );
    let outermost = callers[1].source_span();
    assert_eq!(
        &source[outermost.start() as usize..outermost.end() as usize],
        "middle()"
    );
}

#[test]
fn cross_code_exception_frames_retain_unambiguous_source_artifacts() {
    let invoker_source = "function invoke(callback){return callback();}";
    let failing_source = "function fail(){return lexical;let lexical;}";
    let invoker_authority = compile(invoker_source, "invoke");
    let failing_authority = compile(failing_source, "fail");
    let mut runtime = runtime();
    let invocation_realm = runtime.create_realm().expect("invocation realm");
    let failure_realm = runtime.create_realm().expect("failure realm");
    let invoke = runtime
        .context(&invocation_realm)
        .expect("invocation context")
        .instantiate(invoker_authority)
        .expect("invoke");
    let fail = runtime
        .context(&failure_realm)
        .expect("failure context")
        .instantiate(failing_authority)
        .expect("fail");
    let mut context = runtime.context(&invocation_realm).expect("context");

    let ExecutionError::Exception(exception) = context
        .call(&invoke, &[fail.as_value()], ExecutionLimits::default())
        .expect_err("callee TDZ")
    else {
        panic!("expected JavaScript exception");
    };
    assert_eq!(exception.function(), FunctionTemplateId::new(0));
    assert_eq!(exception.source_name(), "runtime-calls.js");
    assert_eq!(exception.source_text(), failing_source);
    assert_eq!(exception.caller_frames().len(), 1);
    let caller_frame = &exception.caller_frames()[0];
    assert_eq!(caller_frame.function(), FunctionTemplateId::new(0));
    assert_eq!(caller_frame.source_name(), "runtime-calls.js");
    assert_eq!(caller_frame.source_text(), invoker_source);
}

#[test]
fn nested_calls_charge_cumulative_frame_limit() {
    let authority = compile(
        "function outer(){function child(){return 1;}return child();}",
        "outer",
    );
    let constant = compile("function constant(){return 3;}", "constant");
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_active_frames(1)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(authority).expect("outer");
    let constant = context.instantiate(constant).expect("constant");

    assert!(matches!(
        context
            .call(&outer, &[], ExecutionLimits::default())
            .expect_err("second frame exceeds limit"),
        ExecutionError::LimitExceeded {
            resource: quickjs_runtime::RuntimeResource::Frames,
            limit: 1,
            observed: 2,
        }
    ));
    let result = context
        .call(&constant, &[], ExecutionLimits::default())
        .expect("runtime remains reusable after frame-limit failure");
    assert_number(&result, 3);
}

#[test]
fn nested_calls_charge_cumulative_frame_value_limit() {
    let authority = compile(
        "function outer(value){\
            function child(input){return input;}\
            return child(value);\
        }",
        "outer",
    );
    let constant = compile("function constant(){return 3;}", "constant");
    let outer_values = reserved_frame_values(&authority, FunctionTemplateId::new(0));
    let child_values = reserved_frame_values(&authority, FunctionTemplateId::new(1));
    let limit = outer_values
        .checked_add(child_values)
        .expect("small frame usage")
        - 1;
    assert!(outer_values <= limit);
    assert!(child_values <= limit);

    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_active_frame_values(limit))
            .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(authority).expect("outer");
    let constant = context.instantiate(constant).expect("constant");
    let argument = context.number(JsNumber::from_i32(1));

    assert!(matches!(
        context
            .call(&outer, &[argument], ExecutionLimits::default())
            .expect_err("aggregate frame values exceed limit"),
        ExecutionError::LimitExceeded {
            resource: quickjs_runtime::RuntimeResource::FrameValues,
            limit: actual_limit,
            observed,
        } if actual_limit == limit && observed == outer_values + child_values
    ));
    let result = context
        .call(&constant, &[], ExecutionLimits::default())
        .expect("runtime remains reusable after frame-value-limit failure");
    assert_number(&result, 3);
}

#[test]
fn recursive_calls_share_fuel_without_using_the_rust_stack() {
    let authority = compile(
        "function outer(){\
            function recurse(){return recurse();}\
            return recurse();\
        }",
        "outer",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();
    let recurse = {
        let mut context = runtime.context(&realm).expect("context");
        let recurse = context.instantiate(authority).expect("outer");
        let limits = ExecutionLimits::default().with_instruction_fuel(64);

        assert!(matches!(
            context
                .call(&recurse, &[], limits)
                .expect_err("shared fuel interrupts recursion"),
            ExecutionError::InstructionLimitExceeded {
                limit: 64,
                executed: 64,
            }
        ));
        recurse
    };
    drop(recurse);
    runtime
        .collect_cycles()
        .expect("failed recursive execution leaves a collectable graph");
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn abrupt_call_errors_leave_function_and_cell_graphs_collectable() {
    let cases = [
        (
            "function outer(){function child(){return (1)();}return child();}",
            ExceptionKind::TypeError,
        ),
        (
            "function outer(){\
                function child(){return lexical;let lexical;}\
                return child();\
            }",
            ExceptionKind::ReferenceError,
        ),
    ];

    for (source, expected_kind) in cases {
        let authority = compile(source, "outer");
        let mut runtime = runtime();
        let realm = runtime.create_realm().expect("realm");
        let baseline = runtime.usage();
        {
            let mut context = runtime.context(&realm).expect("context");
            let outer = context.instantiate(authority).expect("outer");
            let ExecutionError::Exception(exception) = context
                .call(&outer, &[], ExecutionLimits::default())
                .expect_err("abrupt child call")
            else {
                panic!("expected JavaScript exception");
            };
            assert_eq!(exception.kind(), Some(expected_kind));
        }
        runtime
            .collect_cycles()
            .expect("abrupt call graph remains collectable");
        assert_eq!(runtime.usage(), baseline);
    }
}

#[test]
fn call_results_remain_ordinary_javascript_values() {
    let authority = compile(
        "function outer(){\
            function make(){function result(){return 1;}return result;}\
            return make();\
        }",
        "outer",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let outer = context.instantiate(authority).expect("outer");

    let result = context
        .call(&outer, &[], ExecutionLimits::default())
        .expect("function result");
    assert_eq!(result.kind().expect("live value"), ValueKind::Function);
    let function = result.into_function().expect("function");
    assert_number(
        &context
            .call(&function, &[], ExecutionLimits::default())
            .expect("returned function"),
        1,
    );
}
