//! Host error contract (§4.6): owner validation, slot release, re-entry.

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
                CompilationContext::new_with_source_name(unit, Arc::from("host-contract.js"))
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

#[test]
fn call_function_rejects_foreign_arguments_and_receivers() {
    let mut other_runtime = Runtime::try_new(RuntimeLimits::default()).expect("other runtime");
    let other_realm = other_runtime.create_realm().expect("other realm");
    let mut other_context = other_runtime.context(&other_realm).expect("other context");
    let foreign = other_context.number(JsNumber::from_i32(1));

    with_context(|context| {
        let function = context
            .create_host_function("admit", |ctx, _call| Ok(ctx.number(JsNumber::from_i32(0))))
            .expect("host function");
        assert!(matches!(
            context.call_function(
                &function,
                foreign.clone(),
                Vec::new(),
                ExecutionLimits::default(),
            ),
            Err(fusor_runtime::CallError::Execution(
                fusor_runtime::ExecutionError::Handle(fusor_runtime::HandleError::ForeignRuntime {
                    kind: fusor_runtime::HandleKind::Value
                })
            ))
        ));
        assert!(matches!(
            context.call_function(
                &function,
                context.undefined(),
                vec![foreign],
                ExecutionLimits::default(),
            ),
            Err(fusor_runtime::CallError::Execution(
                fusor_runtime::ExecutionError::Handle(fusor_runtime::HandleError::ForeignRuntime {
                    kind: fusor_runtime::HandleKind::Value
                })
            ))
        ));
    });
}

#[test]
fn host_function_return_values_from_other_runtimes_are_rejected() {
    let mut other_runtime = Runtime::try_new(RuntimeLimits::default()).expect("other runtime");
    let other_realm = other_runtime.create_realm().expect("other realm");
    let mut other_context = other_runtime.context(&other_realm).expect("other context");
    let foreign = other_context.number(JsNumber::from_i32(1));

    with_context(|context| {
        let smuggled: std::cell::RefCell<Option<fusor_runtime::JsValue>> =
            std::cell::RefCell::new(None);
        let slot = std::rc::Rc::new(smuggled);
        let slot_for_callback = std::rc::Rc::clone(&slot);
        let function = context
            .create_host_function("smuggler", move |_ctx, _call| {
                Ok(slot_for_callback
                    .borrow()
                    .as_ref()
                    .expect("filled slot")
                    .clone())
            })
            .expect("host function");
        *slot.borrow_mut() = Some(foreign.clone());
        assert!(matches!(
            context.call_function(
                &function,
                context.undefined(),
                Vec::new(),
                ExecutionLimits::default(),
            ),
            Err(fusor_runtime::CallError::Execution(
                fusor_runtime::ExecutionError::Handle(fusor_runtime::HandleError::ForeignRuntime {
                    kind: fusor_runtime::HandleKind::Value
                })
            ))
        ));
    });
}

#[test]
fn re_entering_a_host_callback_fails_closed_with_a_type_error() {
    with_context(|context| {
        // The callback invokes JavaScript that calls the callback again.
        let function = context
            .create_host_function("reenter", |ctx, call| {
                let function = call.arguments().first().expect("function argument");
                let receiver = ctx.undefined();
                let result = ctx.call_function(
                    &function
                        .clone()
                        .into_function()
                        .expect("function argument is a function"),
                    receiver,
                    vec![function.clone()],
                    ExecutionLimits::default(),
                );
                match result {
                    // The inner call raises the defined re-entry TypeError;
                    // report it so the outer callback can observe it.
                    Err(fusor_runtime::CallError::Thrown(value)) => Err(value),
                    other => Ok(ctx.boolean(other.is_ok())),
                }
            })
            .expect("host function");
        context
            .set_global("reenter", function.as_value())
            .expect("install global");

        let script = compile_global_script(
            "var kind; try { reenter(reenter); } catch (error) { kind = error.name; } String(kind);",
        );
        let result = context
            .execute_global_script(script, ExecutionLimits::default())
            .expect("re-entry script");
        assert_eq!(
            result
                .as_string()
                .expect("live string")
                .expect("String")
                .to_utf8_lossy()
                .expect("UTF-8"),
            "TypeError"
        );
    });
}

#[test]
fn host_installation_charges_the_object_property_limit() {
    // A realm-sized property limit must reject host installations exactly
    // like any other property definition.
    with_context(|context| {
        let usage = context.runtime_usage().object_properties();
        let mut runtime =
            Runtime::try_new(RuntimeLimits::default().with_max_object_properties(usage))
                .expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let mut context = runtime.context(&realm).expect("context");
        let result = context.create_host_function("overLimit", |ctx, _call| {
            Ok(ctx.number(JsNumber::from_i32(0)))
        });
        assert!(matches!(
            result,
            Err(fusor_runtime::ExecutionError::LimitExceeded {
                resource: fusor_runtime::RuntimeResource::ObjectProperties,
                ..
            })
        ));
    });
}
