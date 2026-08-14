//! Host-created promises and their resolvers (§4.4).

use std::sync::Arc;

use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_runtime::{Context, ExecutionLimits, JsNumber, Runtime, RuntimeLimits};

fn compile_global_script(source: &str) -> Arc<fusor_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("host-promises.js"))
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

fn key(context: &mut Context<'_>, name: &str) -> fusor_runtime::PropertyKey {
    context.property_key(name).expect("string property key")
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
fn host_resolve_settles_js_reactions_with_value_identity() {
    with_context(|context| {
        let (promise, resolver) = context.new_promise().expect("host promise");
        let holder = key(context, "hostPromise");
        context
            .global_object()
            .expect("global")
            .into_object()
            .expect("object")
            .set(context, holder, promise.as_value())
            .expect("store promise");

        // The settlement value the host resolves with reaches the reaction
        // with identity.
        let observed = script_text(
            context,
            "var settled = false;\
             var observed;\
             var target = { tag: 9 };\
             hostPromise.then(function (value) { settled = true; observed = value; });\
             globalThis.target = target;\
             'ok';",
        );
        assert_eq!(observed, "ok");

        let target = key(context, "target");
        let value = context
            .global_object()
            .expect("global")
            .into_object()
            .expect("object")
            .get(context, target)
            .expect("target object");
        resolver.resolve(context, value).expect("resolve");

        // Reactions queue as host jobs; drain to quiescence.
        context
            .drain_host_jobs(ExecutionLimits::default(), None)
            .expect("drain");
        assert_eq!(
            script_text(
                context,
                "String(settled && observed === target && observed.tag === 9);",
            ),
            "true"
        );
    });
}

#[test]
fn host_reject_settles_catch_reactions_with_identity() {
    with_context(|context| {
        let (promise, resolver) = context.new_promise().expect("host promise");
        let holder = key(context, "hostPromise");
        context
            .global_object()
            .expect("global")
            .into_object()
            .expect("object")
            .set(context, holder, promise.as_value())
            .expect("store promise");

        let authority = compile_global_script(
            "var rejected = false;\
             var reason;\
             hostPromise.catch(function (error) { rejected = true; reason = error; });",
        );
        context
            .execute_global_script(authority, ExecutionLimits::default())
            .expect("catch script");

        let reason = context
            .error(fusor_runtime::ErrorObjectKind::RangeError, "out of range")
            .expect("reason object");
        resolver.reject(context, reason).expect("reject");
        context
            .drain_host_jobs(ExecutionLimits::default(), None)
            .expect("drain");
        assert_eq!(
            script_text(
                context,
                "String(rejected && reason instanceof RangeError && reason.message === 'out of range');",
            ),
            "true"
        );
    });
}

#[test]
fn the_first_settlement_wins_and_later_calls_are_ignored() {
    with_context(|context| {
        let (promise, resolver) = context.new_promise().expect("host promise");
        let holder = key(context, "hostPromise");
        context
            .global_object()
            .expect("global")
            .into_object()
            .expect("object")
            .set(context, holder, promise.as_value())
            .expect("store promise");
        let authority = compile_global_script(
            "var observed;\
             hostPromise.then(function (value) { observed = value; });",
        );
        context
            .execute_global_script(authority, ExecutionLimits::default())
            .expect("then script");

        let first = context.number(JsNumber::from_i32(1));
        let second = context.number(JsNumber::from_i32(2));
        let late = context
            .error(fusor_runtime::ErrorObjectKind::Error, "late")
            .expect("reason");
        resolver.resolve(context, first).expect("first resolve");
        resolver
            .resolve(context, second)
            .expect("second resolve ignored");
        resolver.reject(context, late).expect("late reject ignored");
        context
            .drain_host_jobs(ExecutionLimits::default(), None)
            .expect("drain");
        assert_eq!(script_text(context, "String(observed === 1);"), "true");
    });
}

#[test]
fn a_pending_host_promise_survives_drains_and_finalizes_later() {
    with_context(|context| {
        let (promise, resolver) = context.new_promise().expect("host promise");
        let holder = key(context, "hostPromise");
        context
            .global_object()
            .expect("global")
            .into_object()
            .expect("object")
            .set(context, holder, promise.as_value())
            .expect("store promise");
        let authority = compile_global_script(
            "var observed;\
             hostPromise.then(function (value) { observed = value; });",
        );
        context
            .execute_global_script(authority, ExecutionLimits::default())
            .expect("then script");

        // The pending promise survives several idle drains.
        for _ in 0..3 {
            context
                .drain_host_jobs(ExecutionLimits::default(), None)
                .expect("drain");
        }
        assert_eq!(
            script_text(context, "String(observed === undefined);"),
            "true"
        );

        let five = context.number(JsNumber::from_i32(5));
        resolver.resolve(context, five).expect("resolve later");
        context
            .drain_host_jobs(ExecutionLimits::default(), None)
            .expect("drain");
        assert_eq!(script_text(context, "String(observed === 5);"), "true");
    });
}
