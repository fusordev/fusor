use std::sync::Arc;

use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_runtime::{ExecutionLimits, JsNumber, JsString, Runtime, RuntimeLimits};

fn compile(source: &str, root_name: &str) -> Arc<fusor_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("values-test.js"))
                    .expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("root function");
            let tree = context
                .compile_tree(&root, fusor_bytecode::VerificationLimits::default())
                .expect("verified function tree");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn boolean(value: &fusor_runtime::JsValue) -> bool {
    value.as_boolean().expect("live value").expect("Boolean")
}

fn text(value: &fusor_runtime::JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
#[allow(clippy::too_many_lines)]
fn truthiness_typeof_and_strict_equality_cover_every_admitted_value_kind() {
    let negate = compile("function negate(value){return !value;}", "negate");
    let type_of = compile("function typeOf(value){return typeof value;}", "typeOf");
    let same = compile("function same(left,right){return left===right;}", "same");
    let identity = compile("function identity(value){return value;}", "identity");

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let negate = context.instantiate(negate).expect("negate");
    let type_of = context.instantiate(type_of).expect("typeof");
    let same = context.instantiate(same).expect("same");
    let identity = context.instantiate(identity).expect("identity");

    let undefined = context.undefined();
    let null = context.null();
    let false_value = context.boolean(false);
    let true_value = context.boolean(true);
    let zero = context.number(JsNumber::from_f64(0.0));
    let negative_zero = context.number(JsNumber::from_f64(-0.0));
    let nan = context.number(JsNumber::from_f64(f64::NAN));
    let number = context.number(JsNumber::from_i32(1));
    let empty = context.string(JsString::empty());
    let string = context.string(JsString::from_utf8("x").expect("string"));
    let function = identity.as_value();

    for value in [
        &undefined,
        &null,
        &false_value,
        &zero,
        &negative_zero,
        &nan,
        &empty,
    ] {
        let result = context
            .call(
                &negate,
                std::slice::from_ref(value),
                ExecutionLimits::default(),
            )
            .expect("falsy call");
        assert!(boolean(&result));
    }
    for value in [&true_value, &number, &string, &function] {
        let result = context
            .call(
                &negate,
                std::slice::from_ref(value),
                ExecutionLimits::default(),
            )
            .expect("truthy call");
        assert!(!boolean(&result));
    }

    for (value, expected) in [
        (&undefined, "undefined"),
        (&null, "object"),
        (&false_value, "boolean"),
        (&number, "number"),
        (&string, "string"),
        (&function, "function"),
    ] {
        let result = context
            .call(
                &type_of,
                std::slice::from_ref(value),
                ExecutionLimits::default(),
            )
            .expect("typeof call");
        assert_eq!(text(&result), expected);
    }

    let equal_zero = context
        .call(
            &same,
            &[zero.clone(), negative_zero.clone()],
            ExecutionLimits::default(),
        )
        .expect("zero equality");
    assert!(boolean(&equal_zero));
    let unequal_nan = context
        .call(&same, &[nan.clone(), nan], ExecutionLimits::default())
        .expect("NaN equality");
    assert!(!boolean(&unequal_nan));
    let same_function = context
        .call(
            &same,
            &[function.clone(), function],
            ExecutionLimits::default(),
        )
        .expect("function equality");
    assert!(boolean(&same_function));
    let different_function = context
        .call(
            &same,
            &[identity.as_value(), same.as_value()],
            ExecutionLimits::default(),
        )
        .expect("function inequality");
    assert!(!boolean(&different_function));
    let same_string = context
        .call(
            &same,
            &[
                context.string(JsString::from_utf8("same").expect("string")),
                context.string(JsString::from_utf8("same").expect("string")),
            ],
            ExecutionLimits::default(),
        )
        .expect("string equality");
    assert!(boolean(&same_string));
}

#[test]
fn nullish_coalescing_distinguishes_nullish_from_other_falsy_values() {
    let authority = compile(
        "function fallback(value){return value??\"fallback\";}",
        "fallback",
    );
    assert!(authority.functions().any(|function| {
        function
            .function()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                instruction.decoded().instruction().opcode()
                    == fusor_bytecode::FinalOpcode::IsUndefinedOrNull
            })
    }));

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let fallback = context.instantiate(authority).expect("fallback");

    for nullish in [context.undefined(), context.null()] {
        let result = context
            .call(&fallback, &[nullish], ExecutionLimits::default())
            .expect("nullish call");
        assert_eq!(text(&result), "fallback");
    }
    let false_value = context.boolean(false);
    let result = context
        .call(&fallback, &[false_value], ExecutionLimits::default())
        .expect("false call");
    assert!(!boolean(&result));
}

#[test]
fn admitted_number_and_string_constants_preserve_exact_payloads() {
    let infinity = compile("function infinity(){return 1e400;}", "infinity");
    let tagged_string = compile("function tagged(){return \"123\";}", "tagged");
    let surrogate = compile(
        "function surrogate(){return \"\\uD800x\\uDC00\";}",
        "surrogate",
    );

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let infinity = context.instantiate(infinity).expect("infinity");
    let tagged_string = context.instantiate(tagged_string).expect("tagged string");
    let surrogate = context.instantiate(surrogate).expect("surrogate");

    let value = context
        .call(&infinity, &[], ExecutionLimits::default())
        .expect("infinity result")
        .as_number()
        .expect("live value")
        .expect("Number");
    assert_eq!(value.as_f64().to_bits(), f64::INFINITY.to_bits());

    let value = context
        .call(&tagged_string, &[], ExecutionLimits::default())
        .expect("tagged string result");
    assert_eq!(text(&value), "123");

    let value = context
        .call(&surrogate, &[], ExecutionLimits::default())
        .expect("surrogate result");
    assert_eq!(
        value
            .as_string()
            .expect("live value")
            .expect("String")
            .code_units()
            .collect::<Vec<_>>(),
        [0xd800, u16::from(b'x'), 0xdc00]
    );
}
