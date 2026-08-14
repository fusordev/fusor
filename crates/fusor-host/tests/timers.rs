//! Timer ops and the virtual clock (§6.4): delay normalization, heap
//! ordering, `setInterval` re-arm, `setImmediate` turn placement, the
//! per-event drain pin (§6.2), the `run_main` drive API (§6.5), and the
//! exit-condition matrix.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use fusor_bytecode::VerifiedBytecode;
use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_host::overlay::{CoreOverlay, HostRuntime};
use fusor_host::r#loop::HostLoop;
use fusor_runtime::{
    Context, ExecutionError, ExecutionLimits, GlobalScriptError,
};

/// Compiles one Global Script into the authority `run_main` and
/// `execute_global_script` consume.
fn compile(source: &str) -> Arc<VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new_with_source_name(unit, Arc::from("timers.js"))
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

    /// Queues one script as the next custom event and runs one turn.
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
fn a_timeout_fires_when_the_virtual_clock_reaches_its_deadline() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "globalThis.fired = 0; \
         Fusor.ops.op_set_timeout(function () { globalThis.fired++; }, 10);",
    );
    assert!(fixture.host.alive(), "a pending timer keeps the loop alive");
    fixture
        .host
        .advance_time(Duration::from_millis(9))
        .expect("advance");
    assert_eq!(
        fixture.observe("String(globalThis.fired);"),
        "0",
        "9ms: not due yet"
    );
    assert!(fixture.host.alive(), "still pending");
    fixture
        .host
        .advance_time(Duration::from_millis(1))
        .expect("advance");
    assert_eq!(
        fixture.observe("String(globalThis.fired);"),
        "1",
        "10ms: fired exactly once"
    );
    assert!(!fixture.host.alive(), "nothing pending: the loop is no longer alive");
}

#[test]
fn same_deadline_timers_fire_in_creation_order() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "globalThis.order = []; \
         Fusor.ops.op_set_timeout(function () { globalThis.order.push('a'); }, 5); \
         Fusor.ops.op_set_timeout(function () { globalThis.order.push('b'); }, 5); \
         Fusor.ops.op_set_timeout(function () { globalThis.order.push('c'); }, 5);",
    );
    fixture
        .host
        .advance_time(Duration::from_millis(5))
        .expect("advance");
    assert_eq!(fixture.observe("globalThis.order.join(',');"), "a,b,c");
}

#[test]
fn delays_truncate_toward_zero_and_negative_delays_clamp_to_zero() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "globalThis.neg = false; globalThis.far = false; \
         Fusor.ops.op_set_timeout(function () { globalThis.neg = true; }, -5); \
         Fusor.ops.op_set_timeout(function () { globalThis.far = true; }, 10.9);",
    );
    // A negative delay clamps to 0: it fires on the very next turn.
    assert_eq!(
        fixture.observe("String(globalThis.neg) + '/' + String(globalThis.far);"),
        "true/false"
    );
    // 10.9ms truncates to 10ms.
    fixture
        .host
        .advance_time(Duration::from_millis(10))
        .expect("advance");
    assert_eq!(fixture.observe("String(globalThis.far);"), "true");
    assert!(!fixture.host.alive());
}

#[test]
fn set_interval_rearms_and_clear_interval_stops_it() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "globalThis.ticks = 0; \
         globalThis.handle = Fusor.ops.op_set_interval(function () { globalThis.ticks++; }, 10);",
    );
    assert!(fixture.host.alive(), "a pending interval keeps the loop alive");
    fixture
        .host
        .advance_time(Duration::from_millis(10))
        .expect("advance");
    assert_eq!(fixture.observe("String(globalThis.ticks);"), "1", "first period");
    fixture
        .host
        .advance_time(Duration::from_millis(20))
        .expect("advance");
    assert_eq!(
        fixture.observe("String(globalThis.ticks);"),
        "2",
        "the clock jumps to 30ms: the 20ms deadline fired once and re-armed \
         from the firing time (no catch-up burst, §6.4)"
    );
    fixture
        .host
        .advance_time(Duration::from_millis(10))
        .expect("advance");
    assert_eq!(
        fixture.observe("String(globalThis.ticks);"),
        "3",
        "40ms: the re-armed deadline fires"
    );
    fixture.eval("Fusor.ops.op_clear_interval(globalThis.handle);");
    assert!(!fixture.host.alive(), "a cleared interval no longer keeps the loop alive");
    fixture
        .host
        .advance_time(Duration::from_millis(100))
        .expect("advance");
    assert_eq!(fixture.observe("String(globalThis.ticks);"), "3", "no firing after clear");
}

#[test]
fn a_zero_delay_interval_fires_once_per_turn() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "globalThis.ticks = 0; \
         globalThis.handle = Fusor.ops.op_set_interval(function () { globalThis.ticks++; }, 0);",
    );
    // A zero-delay re-arm lands at `now` and must wait for the next sweep;
    // this must terminate instead of re-firing forever in one turn. Each
    // `observe` runs one turn: the sweep fires the interval once, then the
    // observation script reads the counter.
    assert_eq!(
        fixture.observe("String(globalThis.ticks);"),
        "1",
        "one firing in the first turn after scheduling"
    );
    assert_eq!(
        fixture.observe("String(globalThis.ticks);"),
        "2",
        "exactly one more firing in the next turn"
    );
    fixture.eval("Fusor.ops.op_clear_interval(globalThis.handle);");
    assert!(!fixture.host.alive());
}

#[test]
fn set_immediate_runs_after_this_turns_events_and_before_the_drain() {
    let mut fixture = Fixture::new();
    // Turn 1 schedules only the zero-delay timer; it fires at the start of
    // turn 2. Turn 2's event then schedules the immediate, which runs at
    // the end of turn 2, before the turn-final checkpoint (§6.4).
    fixture.eval(
        "globalThis.order = []; \
         Fusor.ops.op_set_timeout(function () { \
             globalThis.order.push('timer'); \
             Promise.resolve().then(function () { globalThis.order.push('job1'); }); \
         }, 0);",
    );
    fixture.eval(
        "Fusor.ops.op_set_immediate(function () { \
             globalThis.order.push('immediate'); \
             Promise.resolve().then(function () { globalThis.order.push('job2'); }); \
         }); \
         globalThis.order.push('custom');",
    );
    assert_eq!(
        fixture.observe("globalThis.order.join(',');"),
        "timer,job1,custom,immediate,job2",
        "each host event drains immediately (§6.2); immediates run after the \
         turn's events and before the turn-final checkpoint (§6.4)"
    );
}

#[test]
fn an_immediate_scheduled_by_an_event_runs_at_the_end_of_that_turn() {
    let mut fixture = Fixture::new();
    // The setup script is itself a turn-1 event, so the immediate it
    // schedules runs at the end of turn 1 — before turn 2's zero-delay
    // timer (§6.4: the queue runs after the current turn's events).
    fixture.eval(
        "globalThis.order = []; \
         Fusor.ops.op_set_timeout(function () { globalThis.order.push('timer'); }, 0); \
         Fusor.ops.op_set_immediate(function () { globalThis.order.push('immediate'); });",
    );
    assert_eq!(
        fixture.observe("globalThis.order.join(',');"),
        "immediate,timer"
    );
}

#[test]
fn set_immediate_keeps_the_loop_alive_and_runs_before_clock_advance() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "globalThis.order = []; \
         Fusor.ops.op_set_timeout(function () { globalThis.order.push('timer'); }, 5); \
         Fusor.ops.op_set_immediate(function () { globalThis.order.push('immediate'); });",
    );
    assert!(fixture.host.alive());
    fixture.host.run_until_idle().expect("idle");
    assert_eq!(
        fixture.observe("globalThis.order.join(',');"),
        "immediate,timer",
        "ready immediates run before the virtual clock advances to the timer"
    );
    assert!(!fixture.host.alive(), "nothing pending after both fire");
}

#[test]
fn run_main_evaluates_the_main_script_then_drives_the_loop_to_idle() {
    let mut fixture = Fixture::new();
    let authority = compile(
        "globalThis.ran = 0; \
         Fusor.ops.op_set_timeout(function () { globalThis.ran = 1; }, 3);",
    );
    fixture
        .host
        .run_main(authority, ExecutionLimits::default())
        .expect("main");
    assert_eq!(fixture.observe("String(globalThis.ran);"), "1");
    assert!(!fixture.host.alive());
}

#[test]
fn run_main_routes_a_throwing_main_script_to_the_default_exit_path() {
    let mut fixture = Fixture::new();
    let authority = compile("throw new Error('boom');");
    fixture
        .host
        .run_main(authority, ExecutionLimits::default())
        .expect("default path");
    assert_eq!(
        fixture.host.pending_exit_code(),
        Some(1),
        "a throwing main script fails closed with the documented exit 1 (§7.3)"
    );
    assert!(!fixture.host.alive());
}
