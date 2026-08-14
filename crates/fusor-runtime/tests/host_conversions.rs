//! Host value conversions (§4.5): typed number extraction and the spec
//! `ToBoolean` / `ToString` / `ToNumber` primitives.

use std::sync::Arc;

use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_runtime::{Context, ExecutionError, ExecutionLimits, JsNumber, Runtime, RuntimeLimits};

fn compile_global_script(source: &str) -> Arc<fusor_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("host-conversions.js"))
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
fn number_extraction_covers_safe_integers_nan_and_infinity() {
    with_context(|context| {
        let exact = context.number(JsNumber::from_f64(2_147_483_647.0));
        assert_eq!(exact.as_f64().expect("live"), Some(2_147_483_647.0));
        assert_eq!(exact.as_i32().expect("live"), Some(2_147_483_647_i32));
        assert_eq!(exact.as_u32().expect("live"), Some(2_147_483_647_u32));

        // Above the i32 domain the ToInt32/ToUint32 narrowing applies.
        let large = context.number(JsNumber::from_f64(4_294_967_296.0));
        assert_eq!(large.as_i32().expect("live"), Some(0));
        assert_eq!(large.as_u32().expect("live"), Some(0));

        let nan = context.number(JsNumber::from_f64(f64::NAN));
        assert!(nan.as_f64().expect("live").expect("number").is_nan());
        assert_eq!(nan.as_i32().expect("live"), Some(0));
        assert_eq!(nan.as_u32().expect("live"), Some(0));

        let infinity = context.number(JsNumber::from_f64(f64::INFINITY));
        assert_eq!(infinity.as_i32().expect("live"), Some(0));

        let negative = context.number(JsNumber::from_f64(-1.0));
        assert_eq!(negative.as_i32().expect("live"), Some(-1));
        assert_eq!(negative.as_u32().expect("live"), Some(u32::MAX));

        assert_eq!(
            context
                .string(fusor_runtime::JsString::from_utf8("x").expect("text"))
                .as_f64()
                .expect("live"),
            None
        );
    });
}

#[test]
fn bigint_extraction_returns_the_shared_payload() {
    with_context(|context| {
        let script = compile_global_script("123456789012345678901234567890n;");
        let value = context
            .execute_global_script(script, ExecutionLimits::default())
            .expect("bigint script");
        let bigint = value.as_bigint().expect("live").expect("BigInt");
        assert_eq!(bigint.to_i128(), Some(123456789012345678901234567890_i128));
    });
}

#[test]
fn to_boolean_applies_the_spec_truthiness_table() {
    with_context(|context| {
        for (value, expected) in [
            (context.undefined(), false),
            (context.null(), false),
            (context.boolean(false), false),
            (context.boolean(true), true),
            (context.number(JsNumber::from_i32(0)), false),
            (context.number(JsNumber::from_f64(f64::NAN)), false),
            (context.number(JsNumber::from_i32(1)), true),
            (
                context.string(fusor_runtime::JsString::from_utf8("").expect("empty")),
                false,
            ),
            (
                context.string(fusor_runtime::JsString::from_utf8("0").expect("zero")),
                true,
            ),
        ] {
            assert_eq!(value.to_boolean().expect("live"), expected);
        }
        // Every object (here the global) is truthy.
        assert!(
            context
                .global_object()
                .expect("global")
                .to_boolean()
                .expect("live")
        );
    });
}

#[test]
fn to_string_and_to_number_apply_the_spec_primitive_conversions() {
    with_context(|context| {
        assert_eq!(
            context
                .number(JsNumber::from_f64(1.5))
                .to_string(context)
                .expect("number string")
                .to_utf8_lossy()
                .expect("UTF-8"),
            "1.5"
        );
        assert_eq!(
            context
                .string(fusor_runtime::JsString::from_utf8("  42  ").expect("spaces"))
                .to_number(context)
                .expect("string number")
                .as_f64(),
            42.0
        );
        assert!(
            context
                .string(fusor_runtime::JsString::from_utf8("not a number").expect("invalid"))
                .to_number(context)
                .expect("string number")
                .as_f64()
                .is_nan()
        );
        assert_eq!(
            context
                .boolean(true)
                .to_number(context)
                .expect("boolean number")
                .as_f64(),
            1.0
        );
        assert_eq!(
            context
                .null()
                .to_string(context)
                .expect("null string")
                .to_utf8_lossy()
                .expect("UTF-8"),
            "null"
        );
    });
}

#[test]
fn synchronous_object_conversion_fails_closed() {
    with_context(|context| {
        let object = context.global_object().expect("global");
        match object.to_string(context) {
            Err(ExecutionError::Exception(exception)) => {
                assert_eq!(
                    exception.kind(),
                    Some(fusor_runtime::ExceptionKind::TypeError)
                );
            }
            other => panic!("expected a TypeError, got {other:?}"),
        }
        match object.to_number(context) {
            Err(ExecutionError::Exception(exception)) => {
                assert_eq!(
                    exception.kind(),
                    Some(fusor_runtime::ExceptionKind::TypeError)
                );
            }
            other => panic!("expected a TypeError, got {other:?}"),
        }
    });
}
