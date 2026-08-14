//! Host function construct path (ECMA-262 `[[Construct]]` for host callbacks).
//!
//! The reviewed defects: `new f()` received no real `this` object, a
//! primitive callback result was returned as-is instead of falling back to
//! `this`, and the installed function carried no spec `prototype` own
//! property (so `instanceof` threw).

use std::{cell::RefCell, rc::Rc, sync::Arc};

use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_runtime::{Context, ExecutionLimits, JsNumber, Runtime, RuntimeLimits};

fn compile_global_script(source: &str) -> Arc<fusor_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("host-constructor.js"))
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
fn host_function_installs_the_spec_prototype_and_constructor_back_reference() {
    with_context(|context| {
        let function = context
            .create_host_function("hostCtor", |ctx, _call| {
                Ok(ctx.number(JsNumber::from_i32(1)))
            })
            .expect("host function");
        context
            .set_global("hostCtor", function.as_value())
            .expect("install global");

        assert_eq!(
            script_text(
                context,
                "var p = Object.getOwnPropertyDescriptor(hostCtor, 'prototype');\
                 JSON.stringify({ present: p !== undefined, writable: p.writable, enumerable: p.enumerable, configurable: p.configurable, constructor: p.value.constructor === hostCtor });",
            ),
            "{\"present\":true,\"writable\":true,\"enumerable\":false,\"configurable\":false,\"constructor\":true}"
        );
    });
}

#[test]
fn host_function_construct_creates_a_real_this_and_supports_instanceof() {
    with_context(|context| {
        let function = context
            .create_host_function("hostCtor", |ctx, call| {
                // The construct call receives a freshly created ordinary
                // object as `this` whose prototype is the function's own
                // `prototype` object.
                let tag = key(ctx, "tag");
                let this = call
                    .this()
                    .into_object()
                    .expect("construct this is an object");
                this.set(ctx, tag, ctx.number(JsNumber::from_i32(7)))
                    .expect("write this");
                Ok(call.this())
            })
            .expect("host function");
        context
            .set_global("hostCtor", function.as_value())
            .expect("install global");

        assert_eq!(
            script_text(
                context,
                "var instance = new hostCtor();\
                 String(instance.tag === 7 && instance instanceof hostCtor);",
            ),
            "true"
        );
    });
}

#[test]
fn host_function_construct_reports_the_new_target_identity() {
    with_context(|context| {
        let captured: Rc<RefCell<Option<fusor_runtime::Function>>> = Rc::new(RefCell::new(None));
        let captured_for_callback = Rc::clone(&captured);
        let function = context
            .create_host_function("hostCtor", move |ctx, call| {
                let target = call.new_target().expect("construct call has a new.target");
                let is_self = captured_for_callback
                    .borrow()
                    .as_ref()
                    .is_some_and(|handle| target.same_identity(handle).expect("same runtime"));
                // An object result survives the construct completion, so the
                // Boolean reaches the script instead of falling back to `this`.
                let object = ctx
                    .global_object()
                    .expect("global")
                    .into_object()
                    .expect("object");
                let key = ctx.property_key("value").expect("key");
                object.set(ctx, key, ctx.boolean(is_self)).expect("store");
                Ok(object.as_value())
            })
            .expect("host function");
        *captured.borrow_mut() = Some(function.clone());
        context
            .set_global("hostCtor", function.as_value())
            .expect("install global");

        assert_eq!(
            script_text(context, "String(new hostCtor().value);"),
            "true"
        );
    });
}

#[test]
fn host_function_construct_falls_back_to_this_for_a_primitive_result() {
    with_context(|context| {
        let function = context
            .create_host_function("primitiveCtor", |ctx, _call| {
                Ok(ctx.number(JsNumber::from_i32(42)))
            })
            .expect("host function");
        context
            .set_global("primitiveCtor", function.as_value())
            .expect("install global");

        assert_eq!(
            script_text(
                context,
                "var instance = new primitiveCtor();\
                 String(instance instanceof primitiveCtor);",
            ),
            "true"
        );
    });
}

#[test]
fn host_function_plain_call_keeps_the_ordinary_undefined_receiver() {
    with_context(|context| {
        let function = context
            .create_host_function("plainHost", |ctx, call| {
                Ok(ctx.boolean(
                    call.this().kind().expect("live") == fusor_runtime::ValueKind::Undefined,
                ))
            })
            .expect("host function");
        context
            .set_global("plainHost", function.as_value())
            .expect("install global");

        assert_eq!(script_text(context, "String(plainHost());"), "true");
    });
}
