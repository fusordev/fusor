//! `Context::set_global` defined through ECMA-262 `[[DefineOwnProperty]]`.
//!
//! Regression tests for the three reviewed defects: a foreign value is
//! admitted without `validate_owner`, a repeated key appends a shadow slot
//! (a residue survives `delete`), and a frozen global accepts new
//! properties. The rewritten entry defines
//! `{ value, writable: true, enumerable: false, configurable: true }` through
//! the ordinary descriptor authority, so every rejection raises the same
//! `TypeError` JavaScript observes.

use std::sync::Arc;

use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_runtime::{
    Context, ExceptionKind, ExecutionError, ExecutionLimits, JsNumber, JsValue, Runtime,
    RuntimeLimits, ValueKind,
};

fn compile_global_script(source: &str) -> Arc<fusor_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("host-globals.js"))
                    .expect("storage plan");
            let tree = context
                .compile_global_script(fusor_bytecode::VerificationLimits::default())
                .expect("verified Global Script");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

/// Creates one runtime with its global object handle, and runs the optional
/// setup script first.
fn with_global<T>(
    setup: Option<&str>,
    operation: impl FnOnce(&mut Context<'_>, fusor_runtime::Object) -> T,
) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    if let Some(setup) = setup {
        let authority = compile_global_script(setup);
        context
            .execute_global_script(authority, ExecutionLimits::default())
            .expect("setup script");
    }
    let global = context
        .global_object()
        .expect("global object")
        .into_object()
        .expect("global is an object");
    operation(&mut context, global)
}

fn key(context: &mut Context<'_>, name: &str) -> fusor_runtime::PropertyKey {
    context.property_key(name).expect("string property key")
}

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String value")
        .to_utf8_lossy()
        .expect("UTF-8")
}

fn assert_number(value: &JsValue, expected: i32) {
    let actual = value
        .as_number()
        .expect("live value")
        .expect("Number value");
    assert!(
        actual.strict_equals(JsNumber::from_i32(expected)),
        "expected {expected}, produced {actual:?}"
    );
}

#[test]
fn set_global_installs_a_writable_non_enumerable_configurable_data_property() {
    with_global(None, |context, _global| {
        context
            .set_global("hostX", context.number(JsNumber::from_i32(5)))
            .expect("set_global");
        let authority = compile_global_script(
            "JSON.stringify(Object.getOwnPropertyDescriptor(globalThis, 'hostX'))",
        );
        let value = context
            .execute_global_script(authority, ExecutionLimits::default())
            .expect("descriptor script");
        assert_eq!(
            text(&value),
            "{\"value\":5,\"writable\":true,\"enumerable\":false,\"configurable\":true}"
        );
    });
}

#[test]
fn set_global_overwrites_an_existing_key_without_a_shadow_slot() {
    with_global(None, |context, global| {
        let dup = key(context, "dup");
        context
            .set_global("dup", context.number(JsNumber::from_i32(1)))
            .expect("first set");
        context
            .set_global("dup", context.number(JsNumber::from_i32(2)))
            .expect("overwrite");
        assert_number(&global.get(context, dup.clone()).expect("read back"), 2);

        // No shadow slot may survive deletion of the one logical property.
        assert!(global.delete(context, dup.clone()).expect("deletable"));
        assert_eq!(
            global
                .get(context, dup)
                .expect("after delete")
                .kind()
                .expect("live kind"),
            ValueKind::Undefined
        );
    });
}

#[test]
fn set_global_rejects_a_foreign_value() {
    let mut other_runtime = Runtime::try_new(RuntimeLimits::default()).expect("other runtime");
    let other_realm = other_runtime.create_realm().expect("other realm");
    let mut other_context = other_runtime.context(&other_realm).expect("other context");
    let foreign = other_context.number(JsNumber::from_i32(1));

    with_global(None, |context, _global| {
        assert!(matches!(
            context.set_global("hostForeign", foreign),
            Err(ExecutionError::Handle(
                fusor_runtime::HandleError::ForeignRuntime {
                    kind: fusor_runtime::HandleKind::Value
                }
            ))
        ));
    });
}

#[test]
fn set_global_on_a_frozen_global_throws_a_type_error() {
    with_global(
        Some("Object.freeze(globalThis);"),
        |context, _global| match context
            .set_global("hostFrozen", context.number(JsNumber::from_i32(1)))
        {
            Err(ExecutionError::Exception(exception)) => {
                assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
            }
            other => panic!("expected a TypeError, got {other:?}"),
        },
    );
}
