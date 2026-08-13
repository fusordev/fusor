//! Host interrupt handling.
//!
//! The pinned engine exposes this through `JS_SetInterruptHandler`
//! (`quickjs.c:2236`), polled on a decrementing counter in the interpreter loop
//! (`js_poll_interrupts`, `quickjs.c:7877`) with an interval of
//! `JS_INTERRUPT_COUNTER_INIT` = 10,000 (`quickjs.c:512`). Requesting
//! cancellation raises an *uncatchable* exception
//! (`JS_SetUncatchableException`, `quickjs.c:7861`), so a script cannot swallow
//! it with `try`/`catch`.
//!
//! The C API is not reachable from a script, so these behaviors are asserted
//! against the port directly rather than against `qjs` output. The uncatchable
//! property is preserved structurally: cancellation reports
//! `ExecutionError::Interrupted` rather than a `JsException`, which bypasses the
//! JavaScript unwinder entirely.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use fusor_bytecode::VerificationLimits;
use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_runtime::{
    ExecutionError, ExecutionLimits, INTERRUPT_POLL_INTERVAL, InterruptHandler, Runtime,
    RuntimeLimits,
};

fn compile(source: &str, root_name: &str) -> Arc<fusor_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("runtime-interrupt.js"))
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

/// A handler that cancels after being polled `threshold` times.
struct PollCountingHandler {
    polls: AtomicU32,
    threshold: u32,
}

impl PollCountingHandler {
    fn new(threshold: u32) -> Self {
        Self {
            polls: AtomicU32::new(0),
            threshold,
        }
    }

    fn polls(&self) -> u32 {
        self.polls.load(Ordering::Relaxed)
    }
}

impl InterruptHandler for PollCountingHandler {
    fn should_interrupt(&self) -> bool {
        let polls = self.polls.fetch_add(1, Ordering::Relaxed) + 1;
        polls >= self.threshold
    }
}

/// An unbounded loop, which only an interrupt or fuel exhaustion can stop.
const INFINITE_LOOP: &str = "function run(){let total=0;while(true){total=total+1;}return total;}";

/// A bounded loop that completes well within one poll interval.
const SHORT_LOOP: &str =
    "function run(){let total=0;for(let i=0;i<10;i=i+1){total=total+i;}return total;}";

/// A handler that never requests cancellation must not change the outcome.
#[test]
fn a_handler_that_never_interrupts_leaves_execution_alone() {
    let authority = compile(SHORT_LOOP, "run");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    runtime.set_interrupt_handler(Arc::new(|| false));
    assert!(runtime.has_interrupt_handler());

    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");
    let value = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("an uninterrupted call completes");
    let total = value
        .as_number()
        .expect("live value")
        .expect("Number")
        .as_f64();
    assert!((total - 45.0).abs() < f64::EPSILON);
}

/// A handler requesting cancellation stops an otherwise unbounded loop.
#[test]
fn a_handler_can_cancel_an_unbounded_loop() {
    let authority = compile(INFINITE_LOOP, "run");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    runtime.set_interrupt_handler(Arc::new(|| true));

    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");
    let error = context
        .call(&run, &[], ExecutionLimits::default())
        .expect_err("the handler cancels the call");
    let ExecutionError::Interrupted { executed } = error else {
        panic!("expected an interrupt, found {error:?}");
    };
    // Cancellation is observed at the first poll, so some work has run.
    assert!(executed > 0, "executed {executed}");
}

/// Cancellation is not a catchable JavaScript exception.
///
/// A script wrapping the loop in `try`/`catch`/`finally` must not observe or
/// suppress it, which is the property `JS_SetUncatchableException` provides
/// upstream.
#[test]
fn a_cancellation_cannot_be_caught_by_the_script() {
    let authority = compile(
        "function run(){\
            let caught=0;\
            try{let total=0;while(true){total=total+1;}}\
            catch(error){caught=1;}\
            finally{caught=caught+10;}\
            return caught;\
        }",
        "run",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    runtime.set_interrupt_handler(Arc::new(|| true));

    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");
    let error = context
        .call(&run, &[], ExecutionLimits::default())
        .expect_err("a cancellation escapes the script's handlers");
    assert!(
        matches!(error, ExecutionError::Interrupted { .. }),
        "a cancellation must not become a catchable exception: {error:?}"
    );
}

/// The handler is polled on the pinned interval rather than on every step.
///
/// Polling every step would make the handler's cost dominate execution, so the
/// counter reproduces upstream's `JS_INTERRUPT_COUNTER_INIT`.
#[test]
fn the_handler_is_polled_on_the_pinned_interval() {
    let authority = compile(INFINITE_LOOP, "run");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    // Cancel on the second poll so the first interval is observably skipped.
    let handler = Arc::new(PollCountingHandler::new(2));
    runtime.set_interrupt_handler(Arc::clone(&handler) as Arc<dyn InterruptHandler>);

    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");
    let error = context
        .call(&run, &[], ExecutionLimits::default())
        .expect_err("the handler cancels on its second poll");
    let ExecutionError::Interrupted { executed } = error else {
        panic!("expected an interrupt, found {error:?}");
    };

    assert_eq!(
        handler.polls(),
        2,
        "the handler is polled once per interval"
    );
    // Two intervals elapsed, so the step count is at least that many steps.
    let interval = u64::from(INTERRUPT_POLL_INTERVAL);
    assert!(
        executed >= interval,
        "executed {executed} steps across {} polls",
        handler.polls()
    );
}

/// A short call never reaches the first poll, so the handler is not consulted.
#[test]
fn a_short_call_does_not_reach_the_first_poll() {
    let authority = compile(SHORT_LOOP, "run");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    // The handler would cancel on its first poll, so completing proves the poll
    // never happened.
    let handler = Arc::new(PollCountingHandler::new(1));
    runtime.set_interrupt_handler(Arc::clone(&handler) as Arc<dyn InterruptHandler>);

    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");
    context
        .call(&run, &[], ExecutionLimits::default())
        .expect("a short call completes before the first poll");
    assert_eq!(handler.polls(), 0);
}

/// Clearing the handler restores uninterrupted execution.
#[test]
fn clearing_the_handler_restores_uninterrupted_execution() {
    let authority = compile(SHORT_LOOP, "run");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    runtime.set_interrupt_handler(Arc::new(|| true));
    assert!(runtime.has_interrupt_handler());
    runtime.clear_interrupt_handler();
    assert!(!runtime.has_interrupt_handler());

    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");
    context
        .call(&run, &[], ExecutionLimits::default())
        .expect("no handler means no cancellation");
}

/// Fuel and interrupts remain distinct: fuel exhaustion is still reported as
/// its own error even while a handler is installed.
#[test]
fn fuel_exhaustion_stays_distinct_from_a_cancellation() {
    let authority = compile(INFINITE_LOOP, "run");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    runtime.set_interrupt_handler(Arc::new(|| false));

    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");
    let error = context
        .call(
            &run,
            &[],
            ExecutionLimits::default().with_instruction_fuel(1_000),
        )
        .expect_err("fuel runs out");
    assert!(
        matches!(error, ExecutionError::InstructionLimitExceeded { .. }),
        "fuel exhaustion must not be reported as a cancellation: {error:?}"
    );
}

/// A cancelled runtime stays usable, so a host can cancel one call and continue.
#[test]
fn a_runtime_remains_usable_after_a_cancellation() {
    let looping = compile(INFINITE_LOOP, "run");
    let short = compile(SHORT_LOOP, "run");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let cancel = Arc::new(PollCountingHandler::new(1));
    runtime.set_interrupt_handler(Arc::clone(&cancel) as Arc<dyn InterruptHandler>);

    let realm = runtime.create_realm().expect("realm");
    {
        let mut context = runtime.context(&realm).expect("context");
        let run = context.instantiate(looping).expect("run");
        let error = context
            .call(&run, &[], ExecutionLimits::default())
            .expect_err("the first call is cancelled");
        assert!(matches!(error, ExecutionError::Interrupted { .. }));
    }

    runtime.clear_interrupt_handler();
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(short).expect("run");
    context
        .call(&run, &[], ExecutionLimits::default())
        .expect("the runtime is still usable after a cancellation");
}
