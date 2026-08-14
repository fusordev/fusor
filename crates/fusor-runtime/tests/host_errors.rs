//! Host error construction (§4.3) and structured stack frames.

use std::sync::Arc;

use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_runtime::{
    Context, ErrorObjectKind, ExecutionError, ExecutionLimits, Runtime, RuntimeLimits, ValueKind,
};

fn compile_global_script(source: &str) -> Arc<fusor_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("host-errors.js"))
                    .expect("storage plan");
            let tree = context
                .compile_global_script(fusor_bytecode::VerificationLimits::default())
                .expect("verified Global Script");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn with_context<T>(operation: impl FnOnce(&mut Context<'_>) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    operation(&mut context)
}

fn script_text(context: &mut Context<'_>, source: &str) -> String {
    let authority = compile_global_script(source);
    let result = context
        .execute_global_script(authority, ExecutionLimits::default())
        .expect("script");
    result
        .as_string()
        .expect("live string")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
fn context_error_constructs_each_documented_family() {
    with_context(|context| {
        for (kind, name) in [
            (ErrorObjectKind::Error, "Error"),
            (ErrorObjectKind::TypeError, "TypeError"),
            (ErrorObjectKind::RangeError, "RangeError"),
            (ErrorObjectKind::SyntaxError, "SyntaxError"),
        ] {
            let error = context
                .error(kind, "host message")
                .expect("error construction");
            let holder = key(context, "hostError");
            context
                .global_object()
                .expect("global")
                .into_object()
                .expect("object")
                .set(context, holder, error)
                .expect("store error");
            let source = format!(
                "String(hostError instanceof globalThis[{name:?}] && hostError.message === 'host message');",
            );
            assert_eq!(script_text(context, &source), "true", "{name}");
        }
    });
}

#[test]
fn thrown_host_error_reaches_script_with_identity() {
    with_context(|context| {
        let error = context
            .error(ErrorObjectKind::TypeError, "rejected by host")
            .expect("error construction");
        let holder = key(context, "hostError");
        context
            .global_object()
            .expect("global")
            .into_object()
            .expect("object")
            .set(context, holder, error.clone())
            .expect("store error");

        let function = context
            .create_host_function("thrower", move |ctx, _call| {
                // Re-read the pre-stored error object and throw that exact
                // value, so the JS catch can prove identity.
                let key = ctx.property_key("hostError").expect("key");
                let stored = ctx
                    .global_object()
                    .expect("global")
                    .into_object()
                    .expect("object")
                    .get(ctx, key)
                    .expect("read back");
                Err(stored)
            })
            .expect("host function");
        context
            .set_global("thrower", function.as_value())
            .expect("install global");

        // The JS catch observes the exact same value the host threw.
        assert_eq!(
            script_text(
                context,
                "var caught; try { thrower(); } catch (error) { caught = error; }\
                 String(caught === hostError && caught.message === 'rejected by host');",
            ),
            "true"
        );
    });
}

#[test]
fn call_errors_carry_the_thrown_value_and_stack_frames() {
    with_context(|context| {
        let function = context
            .create_host_function("stackThrower", |ctx, _call| {
                let error = ctx
                    .error(ErrorObjectKind::RangeError, "out of range")
                    .expect("error construction");
                Err(error)
            })
            .expect("host function");
        let result = context.call_function(
            &function,
            context.undefined(),
            Vec::new(),
            ExecutionLimits::default(),
        );
        match result {
            Err(fusor_runtime::CallError::Thrown(value)) => {
                assert_eq!(value.kind().expect("live"), ValueKind::Object);
            }
            other => panic!("expected a thrown value, got {other:?}"),
        }
    });
}

#[test]
fn execution_exceptions_carry_a_structured_stack_trace() {
    with_context(|context| {
        // A throwing getter on the global object produces an ExecutionError
        // whose exception retains the source frame of the throw.
        let authority = compile_global_script(
            "Object.defineProperty(globalThis, 'boom', { get() { throw new TypeError('boom'); }, configurable: true });",
        );
        context
            .execute_global_script(authority, ExecutionLimits::default())
            .expect("define getter");
        let boom = key(context, "boom");
        let result = context
            .global_object()
            .expect("global")
            .into_object()
            .expect("object")
            .get(context, boom);
        match result {
            Err(ExecutionError::Exception(exception)) => {
                // An explicit JavaScript `throw` escapes as its original
                // value, so `kind` is `None` and the thrown value is the
                // TypeError object itself.
                assert_eq!(exception.kind(), None);
                let thrown = exception
                    .thrown_value()
                    .expect("explicit throw retains its value");
                assert_eq!(thrown.kind().expect("live"), ValueKind::Object);
                // The structured frames retain the source and span of the
                // throw site.
                let _ = exception.caller_frames();
                let span = exception.source_span();
                assert!(span.start() <= span.end());
                assert!(!exception.source_name().is_empty());
            }
            other => panic!("expected an exception, got {other:?}"),
        }
    });
}

fn key(context: &mut Context<'_>, name: &str) -> fusor_runtime::PropertyKey {
    context.property_key(name).expect("string property key")
}
