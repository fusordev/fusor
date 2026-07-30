use std::{fmt::Write as _, sync::Arc};

use quickjs_bytecode::{FunctionTemplateId, VerificationLimits};
use quickjs_compiler::{CompilationContext, LeafCompilationError, UnsupportedLeafFeature};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    ExceptionKind, ExecutionError, ExecutionLimits, HandleError, HandleKind, JsException, JsNumber,
    JsString, Runtime, RuntimeLimits, RuntimeResource, ValueKind,
};

fn compile(
    source: &str,
    root_name: &str,
    source_name: &str,
) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new_with_source_name(unit, Arc::from(source_name))
                .expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("root function");
            let tree = context
                .compile_tree(&root, VerificationLimits::default())
                .expect("verified function tree");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn compile_error(source: &str, root_name: &str) -> LeafCompilationError {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("root function");
            context
                .compile_tree(&root, VerificationLimits::default())
                .expect_err("unsupported handler control flow must fail closed")
        },
    )
    .expect("frontend")
}

fn runtime() -> Runtime {
    Runtime::try_new(RuntimeLimits::default()).expect("runtime")
}

fn escaping_exception(result: Result<quickjs_runtime::JsValue, ExecutionError>) -> JsException {
    match result {
        Err(ExecutionError::Exception(exception)) => exception,
        Err(error) => panic!("expected escaping JavaScript throw, found {error:?}"),
        Ok(value) => panic!("expected escaping JavaScript throw, returned {value:?}"),
    }
}

fn assert_arbitrary_throw(exception: &JsException) {
    assert_eq!(exception.kind(), None);
    assert_eq!(exception.message(), None);
    assert!(
        exception.thrown_value().is_some(),
        "an arbitrary throw must retain its exact JavaScript payload"
    );
}

fn assert_throw_origin(
    exception: &JsException,
    source_name: &str,
    source: &str,
    expected_source: &str,
) {
    assert_eq!(exception.source_name(), source_name);
    assert_eq!(exception.source_text(), source);
    let span = exception.source_span();
    assert_eq!(
        &source[span.start() as usize..span.end() as usize],
        expected_source
    );
}

#[test]
fn arbitrary_primitive_payloads_round_trip_with_throw_site_provenance() {
    let source = "function fail(value){throw value;}";
    let authority = compile(source, "fail", "runtime-throw.js");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let fail = context.instantiate(authority).expect("fail");

    let undefined = context.undefined();
    let exception =
        escaping_exception(context.call(&fail, &[undefined], ExecutionLimits::default()));
    assert_arbitrary_throw(&exception);
    assert_eq!(
        exception
            .thrown_value()
            .expect("payload")
            .kind()
            .expect("live value"),
        ValueKind::Undefined
    );
    assert_throw_origin(&exception, "runtime-throw.js", source, "throw value;");

    let null = context.null();
    let exception = escaping_exception(context.call(&fail, &[null], ExecutionLimits::default()));
    assert_arbitrary_throw(&exception);
    assert_eq!(
        exception
            .thrown_value()
            .expect("payload")
            .kind()
            .expect("live value"),
        ValueKind::Null
    );

    let boolean = context.boolean(true);
    let exception = escaping_exception(context.call(&fail, &[boolean], ExecutionLimits::default()));
    assert_arbitrary_throw(&exception);
    assert_eq!(
        exception
            .thrown_value()
            .expect("payload")
            .as_boolean()
            .expect("live value"),
        Some(true)
    );

    let number = context.number(JsNumber::from_f64(-3.5));
    let exception = escaping_exception(context.call(&fail, &[number], ExecutionLimits::default()));
    assert_arbitrary_throw(&exception);
    assert!(
        exception
            .thrown_value()
            .expect("payload")
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_f64(-3.5))
    );

    let string = context.string(JsString::from_utf8("payload").expect("string"));
    let exception = escaping_exception(context.call(&fail, &[string], ExecutionLimits::default()));
    assert_arbitrary_throw(&exception);
    assert_eq!(
        exception
            .thrown_value()
            .expect("payload")
            .as_string()
            .expect("live value")
            .expect("string")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "payload"
    );
}

#[test]
fn throw_expression_failure_remains_an_engine_error_at_the_call_site() {
    let source = "function fail(callee){throw callee();}";
    let authority = compile(source, "fail", "throw-expression-error.js");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let fail = context.instantiate(authority).expect("fail");

    let exception = escaping_exception(context.call(&fail, &[], ExecutionLimits::default()));
    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
    assert_eq!(
        exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "not a function"
    );
    assert!(exception.thrown_value().is_none());
    assert_throw_origin(&exception, "throw-expression-error.js", source, "callee()");
}

#[test]
fn freshly_allocated_thrown_closure_keeps_its_capture_through_collection() {
    let source = "function run(){var captured=17;throw function(){return captured;};}";
    let authority = compile(source, "run", "throw-captured-closure.js");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();

    let (run, exception) = {
        let mut context = runtime.context(&realm).expect("context");
        let run = context.instantiate(authority).expect("run");
        let exception = escaping_exception(context.call(&run, &[], ExecutionLimits::default()));
        assert_arbitrary_throw(&exception);
        (run, exception)
    };

    runtime
        .collect_cycles()
        .expect("the exception roots the fresh closure and capture");
    assert_eq!(runtime.usage().public_roots(), 2);
    assert_eq!(
        runtime.usage().binding_cells(),
        baseline.binding_cells() + 1
    );

    let thrown = exception
        .thrown_value()
        .expect("capturing function payload")
        .clone()
        .into_function()
        .expect("capturing function payload");
    drop(exception);
    {
        let mut context = runtime.context(&realm).expect("context");
        let result = context
            .call(&thrown, &[], ExecutionLimits::default())
            .expect("exception-rooted closure remains callable");
        assert!(
            result
                .as_number()
                .expect("live value")
                .expect("number")
                .strict_equals(JsNumber::from_i32(17))
        );
    }

    drop(thrown);
    drop(run);
    runtime.collect_cycles().expect("final collection");
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn thrown_function_is_rooted_with_identity_until_the_last_exception_value_drops() {
    let fail = compile(
        "function fail(value){throw value;}",
        "fail",
        "throw-function.js",
    );
    let payload = compile(
        "function payload(){return 17;}",
        "payload",
        "throw-function.js",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();

    {
        let mut context = runtime.context(&realm).expect("context");
        let fail = context.instantiate(fail).expect("fail");
        let payload = context.instantiate(payload).expect("payload");
        let payload_value = payload.as_value();
        let exception = escaping_exception(context.call(
            &fail,
            std::slice::from_ref(&payload_value),
            ExecutionLimits::default(),
        ));
        assert_arbitrary_throw(&exception);

        let thrown = exception
            .thrown_value()
            .expect("function payload")
            .clone()
            .into_function()
            .expect("function payload");
        assert!(payload.same_identity(&thrown).expect("same runtime"));

        drop(payload_value);
        drop(payload);
        drop(exception);
        let result = context
            .call(&thrown, &[], ExecutionLimits::default())
            .expect("exception-owned function remains live");
        assert!(
            result
                .as_number()
                .expect("live value")
                .expect("number")
                .strict_equals(JsNumber::from_i32(17))
        );

        drop(thrown);
        drop(fail);
    }

    runtime.collect_cycles().expect("collection");
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn cloned_exceptions_share_one_function_root_until_the_last_clone_drops() {
    let fail = compile(
        "function fail(value){throw value;}",
        "fail",
        "throw-clone.js",
    );
    let payload = compile(
        "function payload(){return 17;}",
        "payload",
        "throw-clone.js",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();

    let (fail, exception, cloned) = {
        let mut context = runtime.context(&realm).expect("context");
        let fail = context.instantiate(fail).expect("fail");
        let payload = context.instantiate(payload).expect("payload");
        let payload_value = payload.as_value();
        let before_throw = context.runtime_usage().public_roots();
        let exception = escaping_exception(context.call(
            &fail,
            std::slice::from_ref(&payload_value),
            ExecutionLimits::default(),
        ));
        assert_eq!(
            context.runtime_usage().public_roots(),
            before_throw + 1,
            "escaping a function creates one independent public root"
        );
        let cloned = exception.clone();
        assert_eq!(
            context.runtime_usage().public_roots(),
            before_throw + 1,
            "cloning an exception shares its Arc root header"
        );
        drop(payload_value);
        drop(payload);
        (fail, exception, cloned)
    };

    runtime
        .collect_cycles()
        .expect("collect original payload root");
    assert_eq!(runtime.usage().public_roots(), 2);
    drop(exception);
    runtime
        .collect_cycles()
        .expect("clone keeps payload rooted");
    assert_eq!(runtime.usage().public_roots(), 2);
    drop(cloned);
    drop(fail);
    runtime
        .collect_cycles()
        .expect("last clone releases payload");
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn arbitrary_throw_display_and_payload_remain_stable_after_runtime_drop() {
    let source = "function fail(value){throw value;}";
    let exception = {
        let authority = compile(source, "fail", "throw-orphan.js");
        let mut runtime = runtime();
        let realm = runtime.create_realm().expect("realm");
        let mut context = runtime.context(&realm).expect("context");
        let fail = context.instantiate(authority).expect("fail");
        escaping_exception(context.call(&fail, &[], ExecutionLimits::default()))
    };

    assert_eq!(exception.to_string(), "uncaught JavaScript value");
    assert_throw_origin(&exception, "throw-orphan.js", source, "throw value;");
    assert!(matches!(
        exception.thrown_value().expect("payload").kind(),
        Err(HandleError::Orphaned {
            kind: HandleKind::Value,
        })
    ));
}

#[test]
fn cross_code_throw_keeps_three_level_callers_distinct_despite_equal_source_names() {
    let throw_source = "function thrower(value){throw value;}";
    let caller_source = "function outer(callback,value){\
        function middle(callback,value){return callback(value);}\
        return middle(callback,value);\
    }";
    let thrower = compile(throw_source, "thrower", "shared-display.js");
    let outer = compile(caller_source, "outer", "shared-display.js");
    let mut runtime = runtime();
    let throw_realm = runtime.create_realm().expect("throw realm");
    let caller_realm = runtime.create_realm().expect("caller realm");
    let thrower = runtime
        .context(&throw_realm)
        .expect("throw context")
        .instantiate(thrower)
        .expect("thrower");
    let outer = runtime
        .context(&caller_realm)
        .expect("caller context")
        .instantiate(outer)
        .expect("outer");
    let mut context = runtime.context(&caller_realm).expect("context");
    let payload = context.string(JsString::from_utf8("cross-code").expect("string"));

    let exception = escaping_exception(context.call(
        &outer,
        &[thrower.as_value(), payload],
        ExecutionLimits::default(),
    ));
    assert_arbitrary_throw(&exception);
    assert_throw_origin(
        &exception,
        "shared-display.js",
        throw_source,
        "throw value;",
    );
    assert_eq!(exception.function(), FunctionTemplateId::new(0));

    let callers = exception.caller_frames();
    assert_eq!(callers.len(), 2);
    assert_eq!(callers[0].source_name(), "shared-display.js");
    assert_eq!(callers[0].source_text(), caller_source);
    let immediate = callers[0].source_span();
    assert_eq!(
        &caller_source[immediate.start() as usize..immediate.end() as usize],
        "callback(value)"
    );
    assert_eq!(callers[1].source_name(), "shared-display.js");
    assert_eq!(callers[1].source_text(), caller_source);
    let outermost = callers[1].source_span();
    assert_eq!(
        &caller_source[outermost.start() as usize..outermost.end() as usize],
        "middle(callback,value)"
    );
}

#[test]
fn deep_iterative_calls_can_be_interrupted_then_preserve_every_throw_caller() {
    const DEPTH: usize = 256;
    let mut source = String::from("function dive(value){");
    for index in 0..DEPTH {
        write!(
            source,
            "function f{index}(value){{return f{}(value);}}",
            index + 1
        )
        .expect("write to String");
    }
    write!(
        source,
        "function f{DEPTH}(value){{throw value;}}return f0(value);}}"
    )
    .expect("write to String");
    let authority = compile(&source, "dive", "deep-throw.js");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let dive = context.instantiate(authority).expect("dive");
    let payload = context.number(JsNumber::from_i32(5));

    assert!(matches!(
        context.call(
            &dive,
            std::slice::from_ref(&payload),
            ExecutionLimits::default().with_instruction_fuel(64),
        ),
        Err(ExecutionError::InstructionLimitExceeded {
            limit: 64,
            executed: 64
        })
    ));

    let exception = escaping_exception(context.call(&dive, &[payload], ExecutionLimits::default()));
    assert_arbitrary_throw(&exception);
    assert_throw_origin(&exception, "deep-throw.js", &source, "throw value;");
    assert_eq!(exception.caller_frames().len(), DEPTH + 1);
    for caller in exception.caller_frames() {
        assert_eq!(caller.source_name(), "deep-throw.js");
        assert_eq!(caller.source_text(), source);
        let span = caller.source_span();
        let call = &source[span.start() as usize..span.end() as usize];
        assert!(
            call.starts_with('f') && call.ends_with("(value)"),
            "unexpected call site: {call}"
        );
    }
}

#[test]
fn thrown_function_root_limit_failure_is_atomic_and_runtime_is_reusable() {
    let source = "function run(shouldThrow){\
        if(shouldThrow){throw function(){return 9;};}\
        return 3;\
    }";
    let authority = compile(source, "run", "throw-limit.js");
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_public_roots(1)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");
    let before = context.runtime_usage();
    let should_throw = context.boolean(true);

    assert!(matches!(
        context.call(&run, &[should_throw], ExecutionLimits::default()),
        Err(ExecutionError::LimitExceeded {
            resource: RuntimeResource::PublicRoots,
            limit: 1,
            observed: 2,
        })
    ));
    assert_eq!(context.runtime_usage().public_roots(), 1);

    let should_return = context.boolean(false);
    let result = context
        .call(&run, &[should_return], ExecutionLimits::default())
        .expect("runtime remains reusable after failed exception rooting");
    assert!(
        result
            .as_number()
            .expect("live value")
            .expect("number")
            .strict_equals(JsNumber::from_i32(3))
    );
    assert_eq!(context.runtime_usage(), before);

    drop(run);
    runtime.collect_cycles().expect("collection");
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn catch_and_finally_handler_control_flow_remains_fail_closed() {
    let cases = [
        "function f(){try{throw 1;}catch(error){return error;}}",
        "function f(){try{return 1;}finally{return 2;}}",
    ];

    for source in cases {
        let LeafCompilationError::Unsupported { feature, span } = compile_error(source, "f") else {
            panic!("handler control flow must remain unsupported: {source}");
        };
        assert_eq!(feature, UnsupportedLeafFeature::UnsupportedBody);
        assert!(
            source[span.start as usize..span.end as usize].starts_with("try"),
            "{source}"
        );
    }
}
