use std::sync::Arc;

use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    ExceptionKind, ExecutionError, ExecutionLimits, JsNumber, JsString, JsValue, Runtime,
    RuntimeLimits,
};

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new_with_source_name(
                unit,
                Arc::from("dynamic-operator-edges.js"),
            )
            .expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("root function");
            let tree = context
                .compile_tree(&root, quickjs_bytecode::VerificationLimits::default())
                .expect("verified function tree");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn runtime() -> Runtime {
    Runtime::try_new(RuntimeLimits::default()).expect("runtime")
}

fn number(value: &JsValue) -> f64 {
    value
        .as_number()
        .expect("live value")
        .expect("Number")
        .as_f64()
}

fn assert_number(value: &JsValue, expected: f64) {
    assert_eq!(number(value).to_bits(), expected.to_bits());
}

fn boolean(value: &JsValue) -> bool {
    value.as_boolean().expect("live value").expect("Boolean")
}

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

fn string(context: &quickjs_runtime::Context<'_>, value: &str) -> JsValue {
    context.string(JsString::from_utf8(value).expect("host string"))
}

fn escaping_exception(result: Result<JsValue, ExecutionError>) -> quickjs_runtime::JsException {
    match result {
        Err(ExecutionError::Exception(exception)) => exception,
        Err(error) => panic!("expected escaping JavaScript exception, found {error:?}"),
        Ok(value) => panic!("expected escaping JavaScript exception, returned {value:?}"),
    }
}

fn assert_type_error(result: Result<JsValue, ExecutionError>, expected_message: &str) {
    let exception = escaping_exception(result);
    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
    assert_eq!(
        exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("UTF-8"),
        expected_message
    );
}

fn assert_thrown_number(result: Result<JsValue, ExecutionError>, expected: f64) {
    let exception = escaping_exception(result);
    assert_eq!(exception.kind(), None);
    let thrown = exception.thrown_value().expect("explicit throw");
    assert_number(thrown, expected);
}

#[test]
fn arithmetic_left_symbol_stops_before_right_object_conversion() {
    let authority = compile(
        "function run(symbol){\
            let right={valueOf(){throw 91;}};\
            return symbol-right;\
        }",
        "run",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");
    let description = JsString::from_utf8("left").expect("description");
    let symbol = context.symbol(Some(&description)).expect("symbol");

    assert_type_error(
        context.call(&run, &[symbol], ExecutionLimits::default()),
        "cannot convert symbol to number",
    );
}

#[test]
fn addition_converts_both_objects_before_classifying_symbol_numeric_addition() {
    let right_abrupt = compile(
        "function rightAbrupt(symbol){\
            let left={valueOf(){return symbol;}};\
            let right={valueOf(){throw 92;}};\
            return left+right;\
        }",
        "rightAbrupt",
    );
    let numeric_error = compile(
        "function numericError(symbol){\
            let left={valueOf(){return symbol;}};\
            let right={valueOf(){return 2;}};\
            return left+right;\
        }",
        "numericError",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let right_abrupt = context.instantiate(right_abrupt).expect("right abrupt");
    let numeric_error = context.instantiate(numeric_error).expect("numeric error");
    let description = JsString::from_utf8("left").expect("description");
    let symbol = context.symbol(Some(&description)).expect("symbol");

    assert_thrown_number(
        context.call(
            &right_abrupt,
            std::slice::from_ref(&symbol),
            ExecutionLimits::default(),
        ),
        92.0,
    );
    assert_type_error(
        context.call(&numeric_error, &[symbol], ExecutionLimits::default()),
        "cannot convert symbol to number",
    );
}

#[test]
fn oversized_string_addition_throws_exact_internal_error() {
    let authority = compile("function add(left,right){return left+right;}", "add");
    let mut half = JsString::from_utf8("x").expect("seed");
    for _ in 0..29 {
        half = half.concat(&half.clone()).expect("bounded doubling");
    }
    assert_eq!(half.len(), 1_u32 << 29);

    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let add = context.instantiate(authority).expect("add");
    let left = context.string(half.clone());
    let right = context.string(half);

    let exception =
        escaping_exception(context.call(&add, &[left, right], ExecutionLimits::default()));
    assert_eq!(exception.kind(), Some(ExceptionKind::InternalError));
    assert_eq!(
        exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "string too long"
    );
}

#[test]
fn postfix_increment_returns_the_converted_old_number_and_writes_the_new_number() {
    let authority = compile(
        "function run(value){\
            let current=value;\
            let old=current++;\
            return typeof old+\":\"+old+\":\"+typeof current+\":\"+current;\
        }",
        "run",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");
    let input = string(&context, "4");

    let result = context
        .call(&run, &[input], ExecutionLimits::default())
        .expect("postfix conversion and writeback");
    assert_eq!(text(&result), "number:4:number:5");
}

#[test]
fn abrupt_compound_assignment_leaves_the_captured_binding_unchanged() {
    let make_state = compile(
        "function makeState(){\
            let current=1;\
            return function state(read,value){\
                if(read){return current;}\
                current+=value;\
                return current;\
            };\
        }",
        "makeState",
    );
    let make_rhs = compile(
        "function makeRhs(){return {valueOf(){throw 77;}};}",
        "makeRhs",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let make_state = context.instantiate(make_state).expect("state factory");
    let make_rhs = context.instantiate(make_rhs).expect("rhs factory");
    let state = context
        .call(&make_state, &[], ExecutionLimits::default())
        .expect("state closure")
        .into_function()
        .expect("Function");
    let rhs = context
        .call(&make_rhs, &[], ExecutionLimits::default())
        .expect("rhs object");
    let write = context.boolean(false);

    assert_thrown_number(
        context.call(&state, &[write, rhs], ExecutionLimits::default()),
        77.0,
    );
    let read = context.boolean(true);
    let ignored = context.undefined();
    let result = context
        .call(&state, &[read, ignored], ExecutionLimits::default())
        .expect("binding after abrupt assignment");
    assert_number(&result, 1.0);
}

#[test]
#[allow(clippy::too_many_lines)]
fn exponentiation_preserves_signed_zero_and_infinity_parity() {
    let authority = compile("function pow(base,exponent){return base**exponent;}", "pow");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let pow = context.instantiate(authority).expect("pow");

    for (base, exponent, expected) in [
        (-0.0, -3.0, f64::NEG_INFINITY),
        (-0.0, -2.0, f64::INFINITY),
        (-0.0, -1.0, f64::NEG_INFINITY),
        (-0.0, 0.0, 1.0),
        (-0.0, 1.0, -0.0),
        (-0.0, 2.0, 0.0),
        (-0.0, 3.0, -0.0),
        (-0.0, f64::INFINITY, 0.0),
        (-0.0, f64::NEG_INFINITY, f64::INFINITY),
        (f64::INFINITY, -1.0, 0.0),
        (f64::NEG_INFINITY, -1.0, -0.0),
        (f64::NEG_INFINITY, 2.0, f64::INFINITY),
        (f64::NEG_INFINITY, 3.0, f64::NEG_INFINITY),
    ] {
        let base = context.number(JsNumber::from_f64(base));
        let exponent = context.number(JsNumber::from_f64(exponent));
        let result = context
            .call(&pow, &[base, exponent], ExecutionLimits::default())
            .expect("exponentiation edge");
        assert_number(&result, expected);
    }
}

#[test]
fn shifts_mask_negative_large_fractional_and_nonfinite_counts() {
    let shl = compile("function shl(value,count){return value<<count;}", "shl");
    let sar = compile("function sar(value,count){return value>>count;}", "sar");
    let shr = compile("function shr(value,count){return value>>>count;}", "shr");
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let shl = context.instantiate(shl).expect("shl");
    let sar = context.instantiate(sar).expect("sar");
    let shr = context.instantiate(shr).expect("shr");

    for (count, expected_shl, expected_shr) in [
        (-1.0, -2_147_483_648.0, 1.0),
        (-33.0, -2_147_483_648.0, 1.0),
        (31.0, -2_147_483_648.0, 1.0),
        (32.0, 1.0, 4_294_967_295.0),
        (33.0, 2.0, 2_147_483_647.0),
        (63.0, -2_147_483_648.0, 1.0),
        (64.0, 1.0, 4_294_967_295.0),
        (65.0, 2.0, 2_147_483_647.0),
        (1.9, 2.0, 2_147_483_647.0),
        (f64::NAN, 1.0, 4_294_967_295.0),
        (f64::INFINITY, 1.0, 4_294_967_295.0),
        (f64::NEG_INFINITY, 1.0, 4_294_967_295.0),
    ] {
        let count = context.number(JsNumber::from_f64(count));
        let one = context.number(JsNumber::from_i32(1));
        let negative_one = context.number(JsNumber::from_i32(-1));
        let result = context
            .call(&shl, &[one, count.clone()], ExecutionLimits::default())
            .expect("left shift");
        assert_number(&result, expected_shl);
        let result = context
            .call(
                &sar,
                &[negative_one.clone(), count.clone()],
                ExecutionLimits::default(),
            )
            .expect("signed right shift");
        assert_number(&result, -1.0);
        let result = context
            .call(&shr, &[negative_one, count], ExecutionLimits::default())
            .expect("unsigned right shift");
        assert_number(&result, expected_shr);
    }
}

#[test]
fn relational_string_number_pairs_use_numeric_conversion_not_string_order() {
    let less = compile("function less(left,right){return left<right;}", "less");
    let less_equal = compile(
        "function lessEqual(left,right){return left<=right;}",
        "lessEqual",
    );
    let greater = compile(
        "function greater(left,right){return left>right;}",
        "greater",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let less = context.instantiate(less).expect("less");
    let less_equal = context.instantiate(less_equal).expect("less equal");
    let greater = context.instantiate(greater).expect("greater");

    for (left, right, expected) in [("2", 10, true), ("20", 3, false), ("x", 3, false)] {
        let left = string(&context, left);
        let right = context.number(JsNumber::from_i32(right));
        let result = context
            .call(&less, &[left, right], ExecutionLimits::default())
            .expect("string/Number less-than");
        assert_eq!(boolean(&result), expected);
    }
    let two = string(&context, "2");
    let one = context.number(JsNumber::from_i32(1));
    let result = context
        .call(&greater, &[two, one], ExecutionLimits::default())
        .expect("string/Number greater-than");
    assert!(boolean(&result));
    let empty = string(&context, "");
    let zero = context.number(JsNumber::from_i32(0));
    let result = context
        .call(&less_equal, &[empty, zero], ExecutionLimits::default())
        .expect("empty string/zero less-equal");
    assert!(boolean(&result));
}

#[test]
fn equality_orders_object_conversion_before_boolean_number_coercion_and_symbols_stay_false() {
    let object_boolean = compile(
        "function objectBoolean(){\
            let log=\"\";\
            let object={\
                valueOf(){log=log+\"v\";return {};},\
                toString(){log=log+\"t\";return \"0\";}\
            };\
            let result=object==false;\
            return log+\":\"+result;\
        }",
        "objectBoolean",
    );
    let equal = compile("function equal(left,right){return left==right;}", "equal");
    let unequal = compile(
        "function unequal(left,right){return left!=right;}",
        "unequal",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let object_boolean = context.instantiate(object_boolean).expect("object/Boolean");
    let equal = context.instantiate(equal).expect("equal");
    let unequal = context.instantiate(unequal).expect("unequal");

    let result = context
        .call(&object_boolean, &[], ExecutionLimits::default())
        .expect("object/Boolean conversion order");
    assert_eq!(text(&result), "vt:true");

    let description = JsString::from_utf8("token").expect("description");
    let symbol = context.symbol(Some(&description)).expect("symbol");
    let false_value = context.boolean(false);
    let result = context
        .call(
            &equal,
            &[symbol.clone(), false_value.clone()],
            ExecutionLimits::default(),
        )
        .expect("Symbol loose equality");
    assert!(!boolean(&result));
    let result = context
        .call(&unequal, &[symbol, false_value], ExecutionLimits::default())
        .expect("Symbol loose inequality");
    assert!(boolean(&result));
}
