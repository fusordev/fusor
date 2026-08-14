//! The shutdown sequence (§7.4): ordered cancellation, resource closing,
//! state teardown, the no-drain pin, and the reported exit code.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use fusor_bytecode::VerifiedBytecode;
use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_host::ops::{OpError, OpRuntime, Resource, install_op, install_op_runtime};
use fusor_host::process::{ExitCode, Signal, SignalState, spawn_signal_forwarder};
use fusor_host::r#loop::HostLoop;
use fusor_ops::op;
use fusor_runtime::{
    Context, ExecutionError, ExecutionLimits, GlobalScriptError, Runtime, RuntimeLimits,
};

/// Compiles one Global Script into the authority `execute_global_script`
/// consumes.
fn compile(source: &str) -> Arc<VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new_with_source_name(unit, Arc::from("shutdown.js"))
                .expect("storage plan");
            let tree = context
                .compile_global_script(fusor_bytecode::VerificationLimits::default())
                .expect("verified Global Script");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

/// Evaluates one Global Script, mapping both failure arms onto
/// [`ExecutionError`].
fn eval_script(
    context: &mut Context<'_>,
    authority: Arc<VerifiedBytecode>,
) -> Result<(), ExecutionError> {
    match context.execute_global_script(authority, ExecutionLimits::default()) {
        Ok(_) => Ok(()),
        Err(GlobalScriptError::Install(source)) => Err(source.into()),
        Err(GlobalScriptError::Execution(source)) => Err(source),
    }
}

/// A loop with the namespace, timers, and process ops installed.
fn fixture() -> HostLoop {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    {
        let mut context = runtime.context(&realm).expect("context");
        fusor_host::ops::install_namespace(&mut context).expect("namespace");
        fusor_host::ops::install_timers(&mut context).expect("timers");
        fusor_host::ops::install_process(&mut context).expect("process");
    }
    HostLoop::new(runtime, realm).expect("host loop")
}

/// Runs one turn whose single custom event evaluates `source`.
fn eval(host: &mut HostLoop, source: &str) {
    let authority = compile(source);
    host.post_event(Box::new(move |context| eval_script(context, authority)));
    host.run_one_turn().expect("turn");
}

struct TestResource {
    closed: Rc<RefCell<bool>>,
}

impl Resource for TestResource {
    fn name(&self) -> &'static str {
        "shutdown-test"
    }

    fn close(self: Rc<Self>) {
        *self.closed.borrow_mut() = true;
    }
}

thread_local! {
    static BUMPS: Cell<u32> = const { Cell::new(0) };
}

#[op]
fn op_bump() -> Result<(), OpError> {
    BUMPS.with(|counter| counter.set(counter.get() + 1));
    Ok(())
}

#[op(async)]
async fn op_hang() -> Result<(), OpError> {
    std::future::pending::<()>().await;
    Ok(())
}

#[test]
fn shutdown_without_an_exit_request_reports_clean() {
    let host = fixture();
    assert_eq!(host.shutdown(), ExitCode::Clean);
}

#[test]
fn shutdown_reports_the_pending_exit_code() {
    let mut host = fixture();
    host.post_signal(Signal::Terminate);
    assert_eq!(host.shutdown(), ExitCode::Requested(143));
}

#[test]
fn shutdown_closes_table_exclusive_resources() {
    let closed = Rc::new(RefCell::new(false));
    let resource = Rc::new(TestResource {
        closed: Rc::clone(&closed),
    });
    fusor_host::ops::install_resource_table(fusor_host::ops::ResourceTable::new())
        .expect("table installed");
    fusor_host::ops::add_resource(resource.clone()).expect("added");
    drop(resource);
    let host = fixture();
    host.shutdown();
    assert!(
        *closed.borrow(),
        "step ③: close_all ran for the table-exclusive resource"
    );
}

#[test]
fn shutdown_cancels_pending_async_ops() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    {
        let mut context = runtime.context(&realm).expect("context");
        fusor_host::ops::install_namespace(&mut context).expect("namespace");
        install_op(
            &mut context,
            __fusor_op_declaration_op_hang(),
            __fusor_op_call_op_hang,
        )
        .expect("hang op");
    }
    let op_runtime = OpRuntime::new().expect("op runtime");
    install_op_runtime(op_runtime).expect("installed");
    let mut host = HostLoop::new(runtime, realm).expect("host loop");
    eval(&mut host, "Fusor.ops.op_hang();");
    assert_eq!(
        fusor_host::ops::pending_op_count().expect("installed"),
        1,
        "the op future is pending"
    );
    host.shutdown();
    assert!(
        fusor_host::ops::pending_op_count().is_err(),
        "step ②: the op runtime was dropped, cancelling the future"
    );
}

#[test]
fn shutdown_does_not_drain_microtasks() {
    BUMPS.with(|counter| counter.set(0));
    let mut host = fixture();
    eval(
        &mut host,
        "Promise.resolve().then(function () { Fusor.ops.op_bump(); });",
    );
    host.shutdown();
    assert_eq!(
        BUMPS.with(|counter| counter.get()),
        0,
        "no microtask ran during shutdown (§7.4)"
    );
}

#[test]
fn a_new_loop_can_install_after_shutdown() {
    {
        let host = fixture();
        host.shutdown();
    }
    // The shutdown took every thread-local state: a fresh loop on the
    // same thread installs and runs cleanly.
    let mut host = fixture();
    eval(&mut host, "globalThis.installed = true;");
    assert!(
        !host.alive(),
        "the fresh loop ran its turn; nothing is pending"
    );
}

#[test]
fn the_signal_forwarder_stops_and_joins() {
    let forwarder = spawn_signal_forwarder(SignalState::default()).expect("forwarder");
    forwarder.shutdown().expect("joined");
}
