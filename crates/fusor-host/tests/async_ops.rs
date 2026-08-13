//! Async ops (§5.5): spawn + mpsc completion back to the owner task.

use std::future::Future;

use fusor_host::ops::{OpError, OpRuntime, install_namespace, install_op, install_op_runtime};
use fusor_ops::op;
use fusor_runtime::{Context, ExecutionLimits, Runtime, RuntimeLimits};

#[op(async)]
async fn op_async_double(value: i32) -> Result<i32, OpError> {
    // Yield to the Tokio worker so the completion genuinely crosses the
    // mpsc boundary.
    tokio::task::yield_now().await;
    Ok(value * 2)
}

#[op(async)]
async fn op_async_fail() -> Result<(), OpError> {
    tokio::task::yield_now().await;
    Err(OpError::of_class("TypeError", "async failure"))
}

/// Compile-time single-owner assertion: the future spawned off the owner
/// task is `Send + 'static` and captures no engine types.
fn assert_send_static<T: Send + 'static>(_: T) {}

fn with_host<T>(operation: impl FnOnce(&mut Context<'_>) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let op_runtime = OpRuntime::new().expect("op runtime");
    install_op_runtime(op_runtime).expect("installed");
    operation(&mut context)
}

fn script(context: &mut Context<'_>, source: &str) {
    use std::sync::Arc;
    let authority = {
        use fusor_compiler::CompilationContext;
        use fusor_frontend::{
            CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program,
        };
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context =
                    CompilationContext::new_with_source_name(unit, Arc::from("async-ops.js"))
                        .expect("storage plan");
                let tree = context
                    .compile_global_script(fusor_bytecode::VerificationLimits::default())
                    .expect("verified Global Script");
                Arc::new(tree.verified_bytecode().clone())
            },
        )
        .expect("frontend")
    };
    context
        .execute_global_script(authority, ExecutionLimits::default())
        .expect("script");
}

fn script_text(context: &mut Context<'_>, source: &str) -> String {
    use std::sync::Arc;
    let authority = {
        use fusor_compiler::CompilationContext;
        use fusor_frontend::{
            CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program,
        };
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context =
                    CompilationContext::new_with_source_name(unit, Arc::from("async-ops.js"))
                        .expect("storage plan");
                let tree = context
                    .compile_global_script(fusor_bytecode::VerificationLimits::default())
                    .expect("verified Global Script");
                Arc::new(tree.verified_bytecode().clone())
            },
        )
        .expect("frontend")
    };
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
fn async_ops_return_promises_that_settle_after_polling() {
    with_host(|context| {
        install_namespace(context).expect("namespace");
        install_op(
            context,
            __fusor_op_declaration_op_async_double(),
            __fusor_op_call_op_async_double,
        )
        .expect("async double");

        // The op returns a Promise immediately; the worker settles it later.
        script(
            context,
            "globalThis.settled = false;\
             globalThis.result = null;\
             Fusor.ops.op_async_double(21).then(function (value) {\
                 globalThis.settled = true;\
                 globalThis.result = value;\
             });",
        );
        assert_eq!(
            script_text(context, "String(settled);"),
            "false",
            "the promise is still pending before the completion poll"
        );

        // The owner task polls; the completion closure settles the promise.
        assert_eq!(
            fusor_host::ops::poll_op_completions(context).expect("installed runtime"),
            1
        );
        context
            .drain_host_jobs(ExecutionLimits::default(), None)
            .expect("drain");
        assert_eq!(script_text(context, "String(settled + '|' + result);"), "true|42");
    });
}

#[test]
fn async_op_errors_reject_the_promise_with_the_op_class() {
    with_host(|context| {
        install_namespace(context).expect("namespace");
        install_op(
            context,
            __fusor_op_declaration_op_async_fail(),
            __fusor_op_call_op_async_fail,
        )
        .expect("async fail");

        script(
            context,
            "globalThis.rejected = false;\
             globalThis.reason = null;\
             Fusor.ops.op_async_fail().catch(function (error) {\
                 globalThis.rejected = true;\
                 globalThis.reason = error.name + ':' + error.message;\
             });",
        );
        assert_eq!(
            fusor_host::ops::poll_op_completions(context).expect("installed runtime"),
            1
        );
        context
            .drain_host_jobs(ExecutionLimits::default(), None)
            .expect("drain");
        assert_eq!(
            script_text(context, "String(rejected + '|' + reason);"),
            "true|TypeError:async failure"
        );
    });
}

#[test]
fn the_spawned_future_is_send_static_and_owns_its_values() {
    // The op future captures only owned Rust values; this bound is what the
    // Tokio spawn enforces at compile time for every async op.
    let future = async { Ok::<i32, OpError>(7) };
    assert_send_static(future);
    let future = async { op_async_double(3).await };
    assert_send_static(future);
}


