//! `Fusor.ops.op_queue_microtask` (§6.2): host jobs enqueue into the
//! engine's promise-job queue, so queued microtasks interleave with
//! Promise reactions in FIFO enqueue order and run at the turn's
//! microtask checkpoint (`drain_host_jobs` to quiescence).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use fusor_bytecode::VerifiedBytecode;
use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_host::r#loop::HostLoop;
use fusor_host::overlay::{CoreOverlay, HostRuntime};
use fusor_runtime::{Context, ExecutionError, ExecutionLimits, GlobalScriptError};

/// Compiles one Global Script into the authority
/// `execute_global_script` consumes.
fn compile(source: &str) -> Arc<VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("microtasks.js"))
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

/// A loop assembled through the overlay builder with the core ops (§9).
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

    /// Joins the global `order` array into `order_string` and reads the
    /// string back in a follow-up event.
    fn read_joined(&mut self) -> String {
        let captured = Rc::new(RefCell::new(String::new()));
        let slot = Rc::clone(&captured);
        let join = compile("globalThis.order_string = globalThis.order.join(',');");
        self.host.post_event(Box::new(move |context| {
            eval_script(context, join)?;
            let global = context.global_object()?.into_object()?;
            let key = context.property_key("order_string")?;
            let value = global.get(context, key)?;
            if let Ok(Some(string)) = value.as_string() {
                *slot.borrow_mut() = string.to_utf8_lossy().expect("UTF-8");
            }
            Ok(())
        }));
        self.host.run_one_turn().expect("turn");
        Rc::try_unwrap(captured).expect("single owner").into_inner()
    }
}

#[test]
fn microtasks_run_at_the_checkpoint_in_fifo_order() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "globalThis.order = [];\
         Fusor.ops.op_queue_microtask(function () { globalThis.order.push('a'); });\
         Fusor.ops.op_queue_microtask(function () { globalThis.order.push('b'); });\
         globalThis.order.push('sync');",
    );
    assert_eq!(
        fixture.read_joined(),
        "sync,a,b",
        "queued microtasks run after the synchronous script, FIFO"
    );
    assert!(
        !fixture.host.alive(),
        "the checkpoint drained every queued microtask"
    );
}

#[test]
fn microtasks_interleave_with_promise_reactions_in_enqueue_order() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "globalThis.order = [];\
         Promise.resolve().then(function () { globalThis.order.push('p1'); });\
         Fusor.ops.op_queue_microtask(function () { globalThis.order.push('m'); });\
         Promise.resolve().then(function () { globalThis.order.push('p2'); });",
    );
    assert_eq!(
        fixture.read_joined(),
        "p1,m,p2",
        "one FIFO job queue serves host microtasks and Promise reactions"
    );
}

#[test]
fn a_microtask_enqueued_before_a_reaction_runs_first() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "globalThis.order = [];\
         Fusor.ops.op_queue_microtask(function () { globalThis.order.push('m'); });\
         Promise.resolve().then(function () { globalThis.order.push('p'); });",
    );
    assert_eq!(fixture.read_joined(), "m,p");
}

#[test]
fn a_microtask_queueing_another_microtask_drains_to_quiescence() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "globalThis.order = [];\
         Fusor.ops.op_queue_microtask(function () {\
             globalThis.order.push('a');\
             Fusor.ops.op_queue_microtask(function () { globalThis.order.push('b'); });\
         });",
    );
    assert_eq!(
        fixture.read_joined(),
        "a,b",
        "the checkpoint drains to quiescence (§6.2)"
    );
}

#[test]
fn a_throwing_microtask_requests_exit_1() {
    let mut fixture = Fixture::new();
    fixture
        .eval("Fusor.ops.op_queue_microtask(function () { throw new Error('microtask boom'); });");
    assert_eq!(
        fixture.host.pending_exit_code(),
        Some(1),
        "the throwing microtask routed to the default uncaught path (§7.3)"
    );
}

#[test]
fn queue_microtask_rejects_a_non_function() {
    let mut fixture = Fixture::new();
    fixture.eval(
        "var kind;\
         try { Fusor.ops.op_queue_microtask(42); }\
         catch (error) { kind = error.name; }\
         globalThis.order = [kind];",
    );
    assert_eq!(fixture.read_joined(), "TypeError");
}
