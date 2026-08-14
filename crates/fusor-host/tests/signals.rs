//! Signal event source (§7.1): the injectable delivery path, the
//! first-SIGINT interrupt semantics, the second-SIGINT/SIGTERM force-exit
//! policy, and interrupt consumption.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use fusor_bytecode::VerifiedBytecode;
use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_host::r#loop::HostLoop;
use fusor_host::overlay::{CoreOverlay, HostRuntime};
use fusor_host::process::Signal;
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
            let context = CompilationContext::new_with_source_name(unit, Arc::from("signals.js"))
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

/// A loop with the timer ops installed, driven one custom event at a time.
struct Fixture {
    host: HostLoop,
}

impl Fixture {
    fn new() -> Self {
        let host = HostRuntime::builder()
            .with_overlay(CoreOverlay)
            .build()
            .expect("built")
            .into_loop()
            .expect("host loop");
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

/// A script long enough to cross the engine's 10k-step interrupt poll but
/// short enough to finish well inside the default 10M-unit fuel budget.
/// The stores keep the loop observable so the compiler cannot fold it.
const LONG_LOOP: &str = "for (let i = 0; i < 100000; i++) { globalThis.acc = i; }";

#[test]
fn a_sigint_interrupts_a_running_script_and_is_consumed() {
    let mut fixture = Fixture::new();
    fixture.eval("globalThis.ran = false;");
    fixture.host.post_signal(Signal::Interrupt);
    let authority = compile(&format!("{LONG_LOOP} globalThis.ran = true;"));
    fixture
        .host
        .post_event(Box::new(move |context| eval_script(context, authority)));
    let result = fixture.host.run_one_turn();
    assert!(
        matches!(result, Err(ExecutionError::Interrupted { .. })),
        "the running script is cancelled at the next interrupt poll: {result:?}"
    );
    assert_eq!(
        fixture.observe("String(globalThis.ran);"),
        "false",
        "the script aborted before completing"
    );
    assert_eq!(
        fixture.observe("String(1 + 1);"),
        "2",
        "the interrupt was consumed: later turns run normally"
    );
    assert!(
        fixture.host.pending_exit_code().is_none(),
        "a single SIGINT does not force an exit"
    );
}

#[test]
fn timers_due_in_an_interrupted_turn_fire_in_the_next_turn() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "globalThis.order = []; \
         Fusor.ops.op_set_timeout(function () { \
             for (let i = 0; i < 100000; i++) { globalThis.acc = i; } \
             globalThis.order.push('a'); \
         }, 0); \
         Fusor.ops.op_set_timeout(function () { globalThis.order.push('b'); }, 0);",
    );
    fixture.host.post_signal(Signal::Interrupt);
    let result = fixture.host.run_one_turn();
    assert!(
        matches!(result, Err(ExecutionError::Interrupted { .. })),
        "the first timer callback is interrupted: {result:?}"
    );
    assert_eq!(
        fixture.observe("globalThis.order.join(',');"),
        "a,b",
        "the aborted timer re-fires and completes, then the second fires"
    );
}

#[test]
fn an_idle_sigint_is_consumed_at_turn_end() {
    let mut fixture = Fixture::new();
    fixture.host.post_signal(Signal::Interrupt);
    fixture.host.run_one_turn().expect("idle turn");
    assert!(
        fixture.host.pending_exit_code().is_none(),
        "an idle SIGINT does not force an exit"
    );
    assert!(
        !fixture.host.alive(),
        "nothing is pending after the idle turn: the loop is not alive"
    );
    assert_eq!(
        fixture.observe("String(1 + 1);"),
        "2",
        "nothing was interrupted: later turns run normally"
    );
}

#[test]
fn a_second_sigint_force_exits_with_130() {
    let mut fixture = Fixture::new();
    fixture.host.post_signal(Signal::Interrupt);
    fixture.host.post_signal(Signal::Interrupt);
    assert_eq!(fixture.host.pending_exit_code(), Some(130));
    assert!(!fixture.host.alive(), "a force exit stops the loop");
    fixture.host.run_until_idle().expect("exits without error");
    let ran = Rc::new(RefCell::new(false));
    let ran_in = Rc::clone(&ran);
    fixture.host.post_event(Box::new(move |_context| {
        *ran_in.borrow_mut() = true;
        Ok(())
    }));
    fixture.host.run_one_turn().expect("turn is a no-op");
    assert!(!*ran.borrow(), "no work runs after a force exit");
}

#[test]
fn a_sigterm_force_exits_with_143() {
    let mut fixture = Fixture::new();
    fixture.host.post_signal(Signal::Terminate);
    assert_eq!(fixture.host.pending_exit_code(), Some(143));
    assert!(!fixture.host.alive());
    fixture.host.run_until_idle().expect("exits without error");
}

#[test]
fn a_sigint_interrupts_run_main() {
    let mut fixture = Fixture::new();
    let authority = compile(&format!(
        "globalThis.ran = false; {LONG_LOOP} globalThis.ran = true;"
    ));
    fixture.host.post_signal(Signal::Interrupt);
    let result = fixture.host.run_main(authority, ExecutionLimits::default());
    assert!(
        matches!(result, Err(ExecutionError::Interrupted { .. })),
        "the main script is cancelled at the next interrupt poll: {result:?}"
    );
    assert_eq!(fixture.observe("String(globalThis.ran);"), "false");
    assert_eq!(
        fixture.observe("String(2 + 2);"),
        "4",
        "the interrupt was consumed: later turns run normally"
    );
}
