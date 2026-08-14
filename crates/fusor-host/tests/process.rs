//! The `process` global and its ops (§7.1): `process.on("SIGINT", ...)`
//! registration, handler delivery through the loop, and the fail-closed
//! rejection paths.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use fusor_bytecode::VerifiedBytecode;
use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_host::process::Signal;
use fusor_host::r#loop::HostLoop;
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
            let context = CompilationContext::new_with_source_name(unit, Arc::from("process.js"))
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

/// Evaluates one Global Script and returns its completion value as a
/// JavaScript String.
fn eval_string(
    context: &mut Context<'_>,
    authority: Arc<VerifiedBytecode>,
) -> Result<String, ExecutionError> {
    let value = match context.execute_global_script(authority, ExecutionLimits::default()) {
        Ok(value) => value,
        Err(GlobalScriptError::Install(source)) => return Err(source.into()),
        Err(GlobalScriptError::Execution(source)) => return Err(source),
    };
    Ok(value
        .as_string()
        .expect("live string")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8"))
}

/// A loop with the `process` global installed, driven one custom event at
/// a time.
struct Fixture {
    host: HostLoop,
}

impl Fixture {
    fn new() -> Self {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        {
            let mut context = runtime.context(&realm).expect("context");
            fusor_host::ops::install_process(&mut context).expect("process");
        }
        let host = HostLoop::new(runtime, realm).expect("host loop");
        Self { host }
    }

    /// Queues one script as the next custom event and runs one turn,
    /// failing the test if the turn fails.
    fn eval(&mut self, source: &str) {
        let authority = compile(source);
        self.host
            .post_event(Box::new(move |context| eval_script(context, authority)));
        self.host.run_one_turn().expect("turn");
    }

    /// Runs one turn whose single custom event evaluates `source` and
    /// returns the completion value.
    fn observe(&mut self, source: &str) -> String {
        let authority = compile(source);
        let result = Rc::new(RefCell::new(String::new()));
        let result_in = Rc::clone(&result);
        self.host.post_event(Box::new(move |context| {
            *result_in.borrow_mut() = eval_string(context, authority)?;
            Ok(())
        }));
        self.host.run_one_turn().expect("turn");
        result.borrow().clone()
    }
}

#[test]
fn a_registered_sigint_handler_receives_deliveries_and_disables_the_default_exit() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "globalThis.count = 0; \
         process.on('SIGINT', function () { globalThis.count++; });",
    );
    fixture.host.post_signal(Signal::Interrupt);
    fixture.host.post_signal(Signal::Interrupt);
    fixture.host.run_one_turn().expect("turn");
    assert_eq!(
        fixture.observe("String(globalThis.count);"),
        "2",
        "each delivery reaches the handler once"
    );
    assert!(
        fixture.host.pending_exit_code().is_none(),
        "a registered handler disables the second-SIGINT force exit (§7.1)"
    );
}

#[test]
fn a_registered_handler_prevents_script_interruption() {
    let mut fixture = Fixture::new();
    fixture.eval("process.on('SIGINT', function () { globalThis.handler_ran = true; });");
    fixture.host.post_signal(Signal::Interrupt);
    // The delivery must not arm the interrupt request: the long script
    // completes instead of aborting at the poll.
    fixture.eval(
        "for (let i = 0; i < 100000; i++) { globalThis.acc = i; } \
         globalThis.done = true;",
    );
    assert_eq!(
        fixture.observe(
            "String(globalThis.done === true) + '/' + String(globalThis.handler_ran === true);"
        ),
        "true/true"
    );
}

#[test]
fn pending_deliveries_keep_the_loop_alive_until_the_handler_runs() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "globalThis.count = 0; \
         process.on('SIGINT', function () { globalThis.count++; });",
    );
    fixture.host.post_signal(Signal::Interrupt);
    assert!(fixture.host.alive(), "a pending delivery is alive work");
    fixture.host.run_until_idle().expect("idle");
    assert_eq!(fixture.observe("String(globalThis.count);"), "1");
    assert!(!fixture.host.alive(), "nothing pending after the handler ran");
}

#[test]
fn the_handler_receiver_is_the_process_object() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "globalThis.self_is_process = false; \
         process.on('SIGINT', function () { globalThis.self_is_process = this === process; });",
    );
    fixture.host.post_signal(Signal::Interrupt);
    fixture.host.run_one_turn().expect("turn");
    assert_eq!(fixture.observe("String(globalThis.self_is_process);"), "true");
}

#[test]
fn process_on_rejects_unknown_events() {
    let mut fixture = Fixture::new();
    assert_eq!(
        fixture.observe(
            "try { process.on('BOGUS', function () {}); 'accepted'; } \
             catch (error) { error.constructor.name + ':' + error.message; }"
        ),
        "RangeError:unsupported process event 'BOGUS' (the alpha host supports 'SIGINT')"
    );
}

#[test]
fn process_on_rejects_a_non_function_handler() {
    let mut fixture = Fixture::new();
    assert_eq!(
        fixture.observe(
            "try { process.on('SIGINT', 42); 'accepted'; } \
             catch (error) { error.constructor.name; }"
        ),
        "TypeError"
    );
}

#[test]
fn a_throwing_sigint_handler_fails_the_turn_closed() {
    let mut fixture = Fixture::new();
    fixture.eval("process.on('SIGINT', function () { throw new Error('handler boom'); });");
    fixture.host.post_signal(Signal::Interrupt);
    let result = fixture.host.run_one_turn();
    assert!(
        result.is_err(),
        "a throwing handler fails the turn closed: {result:?}"
    );
}

#[test]
fn process_exit_requests_the_code_and_stops_the_loop() {
    let mut fixture = Fixture::new();
    fixture.eval("process.exit(3);");
    assert_eq!(fixture.host.pending_exit_code(), Some(3));
    assert!(!fixture.host.alive(), "an exit request stops the loop");
    fixture.host.run_until_idle().expect("exits without error");
}

#[test]
fn process_exit_zero_is_a_real_exit_request() {
    let mut fixture = Fixture::new();
    fixture.eval("process.exit(0);");
    assert_eq!(fixture.host.pending_exit_code(), Some(0));
    assert!(!fixture.host.alive());
}

#[test]
fn process_exit_truncates_the_code_to_8_bits() {
    let mut fixture = Fixture::new();
    fixture.eval("process.exit(256);");
    assert_eq!(
        fixture.host.pending_exit_code(),
        Some(0),
        "256 truncates to 0 (Node semantics)"
    );
}

#[test]
fn process_exit_negative_truncates_to_255() {
    let mut fixture = Fixture::new();
    fixture.eval("process.exit(-1);");
    assert_eq!(fixture.host.pending_exit_code(), Some(255));
}

#[test]
fn the_first_exit_request_wins() {
    let mut fixture = Fixture::new();
    fixture.eval("process.exit(7);");
    fixture.host.post_signal(Signal::Terminate);
    assert_eq!(
        fixture.host.pending_exit_code(),
        Some(7),
        "a later SIGTERM does not replace the requested code"
    );
}

#[test]
fn a_signal_exit_wins_over_a_later_process_exit() {
    let mut fixture = Fixture::new();
    fixture.host.post_signal(Signal::Terminate);
    fixture.eval("process.exit(7);");
    assert_eq!(
        fixture.host.pending_exit_code(),
        Some(143),
        "the turn after a force exit is a no-op: process.exit never runs"
    );
}

#[test]
fn exit_codes_map_engine_errors_and_interrupts() {
    use fusor_host::process::ExitCode;
    use fusor_runtime::ExecutionError;

    let abort = ExecutionError::EngineFault(fusor_runtime::EngineFault::RuntimeInvariant {
        message: "boom",
    });
    assert_eq!(
        ExitCode::from_execution_error(&abort),
        Some(ExitCode::EngineAbort)
    );
    assert_eq!(ExitCode::EngineAbort.as_i32(), 2, "documented engine-abort code");
    let interrupt = ExecutionError::Interrupted { executed: 42 };
    assert_eq!(
        ExitCode::from_execution_error(&interrupt),
        None,
        "an interrupt is not a process exit by itself"
    );
    assert_eq!(ExitCode::Clean.as_i32(), 0);
    assert_eq!(ExitCode::UncaughtException.as_i32(), 1);
    assert_eq!(ExitCode::UnhandledRejection.as_i32(), 1);
    assert_eq!(ExitCode::Requested(130).as_i32(), 130);
}
