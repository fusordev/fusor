//! Host event loop (§6.1, §6.2): turn structure, custom events, and the
//! normative no-interleaving pin.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_host::r#loop::HostLoop;
use fusor_host::ops::OpRuntime;
use fusor_runtime::{Context, ExecutionLimits, Runtime, RuntimeLimits};

fn script(context: &mut Context<'_>, source: &str) -> String {
    let authority = {
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context =
                    CompilationContext::new_with_source_name(unit, Arc::from("loop.js"))
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
fn an_empty_loop_exits_immediately() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let op_runtime = OpRuntime::new().expect("op runtime");
    fusor_host::ops::install_op_runtime(op_runtime).expect("installed");
    let mut host_loop = HostLoop::new(runtime, realm).expect("host loop");
    assert!(!host_loop.alive(), "no events and no pending ops: not alive");
    host_loop.run_until_idle().expect("idle run");
}

#[test]
fn custom_events_run_in_order_with_a_drain_after_each() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let op_runtime = OpRuntime::new().expect("op runtime");
    fusor_host::ops::install_op_runtime(op_runtime).expect("installed");
    let mut host_loop = HostLoop::new(runtime, realm).expect("host loop");

    let order = Rc::new(RefCell::new(Vec::new()));
    let order_a = Rc::clone(&order);
    let order_b = Rc::clone(&order);
    host_loop.post_event(Box::new(move |context| {
        order_a.borrow_mut().push("a");
        script(context, "Promise.resolve().then(function () { globalThis.afterA = true; }); 'ok';");
        Ok(())
    }));
    host_loop.post_event(Box::new(move |context| {
        // The microtask checkpoint must run before the next host event (§6.2).
        order_b.borrow_mut().push("b");
        let observed = script(context, "String(globalThis.afterA === true);");
        assert_eq!(observed, "true", "jobs drain between host callbacks");
        Ok(())
    }));
    host_loop.run_until_idle().expect("events");
    assert_eq!(*order.borrow(), vec!["a", "b"]);
}

#[test]
fn custom_events_keep_the_loop_alive_until_drained() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let op_runtime = OpRuntime::new().expect("op runtime");
    fusor_host::ops::install_op_runtime(op_runtime).expect("installed");
    let mut host_loop = HostLoop::new(runtime, realm).expect("host loop");
    host_loop.post_event(Box::new(|context| {
        script(context, "globalThis.ran = true; 'ran';");
        Ok(())
    }));
    assert!(host_loop.alive());
    host_loop.run_until_idle().expect("drain");
    assert!(!host_loop.alive());
}

#[test]
fn run_one_turn_executes_exactly_one_batch_of_events() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let op_runtime = OpRuntime::new().expect("op runtime");
    fusor_host::ops::install_op_runtime(op_runtime).expect("installed");
    let mut host_loop = HostLoop::new(runtime, realm).expect("host loop");

    let count = Rc::new(RefCell::new(0));
    let count_a = Rc::clone(&count);
    let count_b = Rc::clone(&count);
    host_loop.post_event(Box::new(move |_context| {
        *count_a.borrow_mut() += 1;
        Ok(())
    }));
    host_loop.post_event(Box::new(move |_context| {
        *count_b.borrow_mut() += 1;
        Ok(())
    }));

    host_loop.run_one_turn().expect("one turn");
    assert_eq!(*count.borrow(), 2, "one turn drains the event queue");
    host_loop.run_one_turn().expect("empty turn is fine");
    assert_eq!(*count.borrow(), 2);
}

#[test]
fn a_host_callback_error_fails_the_turn_closed() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let op_runtime = OpRuntime::new().expect("op runtime");
    fusor_host::ops::install_op_runtime(op_runtime).expect("installed");
    let mut host_loop = HostLoop::new(runtime, realm).expect("host loop");
    host_loop.post_event(Box::new(|context| {
        let foreign = {
            // A value from a different runtime must be rejected, not smuggled.
            let mut other = Runtime::try_new(RuntimeLimits::default()).expect("other");
            let other_realm = other.create_realm().expect("realm");
            let mut other_context = other.context(&other_realm).expect("context");
            other_context.number(fusor_runtime::JsNumber::from_i32(1))
        };
        let key = context.property_key("boom")?;
        context
            .global_object()?
            .into_object()?
            .set(context, key, foreign)
    }));
    let result = host_loop.run_until_idle();
    assert!(result.is_err(), "foreign values fail closed");
}
