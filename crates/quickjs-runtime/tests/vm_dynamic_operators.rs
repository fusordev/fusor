use std::sync::Arc;

use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    ExceptionKind, ExecutionError, ExecutionLimits, Function, JsNumber, JsString, JsValue,
    PredefinedAtom, Runtime, RuntimeLimits,
};

const SOURCE_NAME: &str = "dynamic-operators.js";

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new_with_source_name(unit, Arc::from(SOURCE_NAME))
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

fn instantiate(
    context: &mut quickjs_runtime::Context<'_>,
    source: &str,
    root_name: &str,
) -> Function {
    context
        .instantiate(compile(source, root_name))
        .expect("dynamic operator installation")
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

fn assert_nan(value: &JsValue) {
    assert!(number(value).is_nan());
}

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

fn boolean(value: &JsValue) -> bool {
    value.as_boolean().expect("live value").expect("Boolean")
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
fn optional_member_chains_skip_keys_and_the_remaining_syntactic_chain() {
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = instantiate(
        &mut context,
        "function run(){\
            let hits=0;\
            let missing=null?.[hits++].x;\
            let base={nested:{value:7},0:9};\
            let found=base?.nested.value;\
            let computed=base?.[hits++];\
            let nested=({a:null})?.a?.b;\
            return (missing===void 0?1000:0)+(nested===void 0?100:0)+\
                   found*10+computed+hits;\
        }",
        "run",
    );

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("optional member chain");

    assert_number(&result, 1180.0);
}

#[test]
fn parentheses_end_optional_member_short_circuiting() {
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = instantiate(&mut context, "function run(){return (null?.a).b;}", "run");

    let exception = escaping_exception(context.call(&run, &[], ExecutionLimits::default()));
    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
}

#[test]
fn parenthesized_optional_members_can_supply_constructor_values() {
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let construct = instantiate(
        &mut context,
        "function run(){\
            function Constructor(){return {value:7};}\
            return new ({Constructor:Constructor}?.Constructor)().value;\
        }",
        "run",
    );
    let reject = instantiate(
        &mut context,
        "function reject(){return new (null?.Constructor)();}",
        "reject",
    );

    assert_number(
        &context
            .call(&construct, &[], ExecutionLimits::default())
            .expect("optional member constructor value"),
        7.0,
    );
    let exception = escaping_exception(context.call(&reject, &[], ExecutionLimits::default()));
    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
}

#[test]
fn unary_numeric_operators_preserve_exact_number_edges() {
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let plus = instantiate(&mut context, "function plus(value){return +value;}", "plus");
    let negate = instantiate(
        &mut context,
        "function negate(value){return -value;}",
        "negate",
    );
    let bit_not = instantiate(
        &mut context,
        "function bitNot(value){return ~value;}",
        "bitNot",
    );

    let negative_zero = context.number(JsNumber::from_f64(-0.0));
    let positive_zero = context.number(JsNumber::from_f64(0.0));
    let nan = context.number(JsNumber::from_f64(f64::NAN));
    let infinity = context.number(JsNumber::from_f64(f64::INFINITY));
    let three = context.number(JsNumber::from_i32(3));

    let result = context
        .call(
            &plus,
            std::slice::from_ref(&negative_zero),
            ExecutionLimits::default(),
        )
        .expect("unary plus");
    assert_number(&result, -0.0);
    let result = context
        .call(&negate, &[negative_zero], ExecutionLimits::default())
        .expect("negated negative zero");
    assert_number(&result, 0.0);
    let result = context
        .call(&negate, &[positive_zero], ExecutionLimits::default())
        .expect("negated positive zero");
    assert_number(&result, -0.0);
    let result = context
        .call(&plus, &[nan], ExecutionLimits::default())
        .expect("unary plus NaN");
    assert_nan(&result);
    let result = context
        .call(&bit_not, &[infinity], ExecutionLimits::default())
        .expect("bitwise not infinity");
    assert_number(&result, -1.0);
    let result = context
        .call(&bit_not, &[three], ExecutionLimits::default())
        .expect("bitwise not integer");
    assert_number(&result, -4.0);
}

#[test]
fn string_and_primitive_to_number_follow_quickjs_grammar() {
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let plus = instantiate(&mut context, "function plus(value){return +value;}", "plus");

    for (source, expected) in [
        ("", 0.0),
        (" \t\n", 0.0),
        ("0x10", 16.0),
        ("0Xf", 15.0),
        ("0b11", 3.0),
        ("0B10", 2.0),
        ("0o10", 8.0),
        ("0O7", 7.0),
        ("Infinity", f64::INFINITY),
        ("+Infinity", f64::INFINITY),
        ("-Infinity", f64::NEG_INFINITY),
        ("-0", -0.0),
    ] {
        let input = string(&context, source);
        let result = context
            .call(&plus, &[input], ExecutionLimits::default())
            .expect("string ToNumber");
        assert_number(&result, expected);
    }
    for source in ["infinity", "12junk"] {
        let input = string(&context, source);
        let result = context
            .call(&plus, &[input], ExecutionLimits::default())
            .expect("invalid string ToNumber");
        assert_nan(&result);
    }

    for (input, expected) in [
        (context.boolean(false), 0.0),
        (context.boolean(true), 1.0),
        (context.null(), 0.0),
    ] {
        let result = context
            .call(&plus, &[input], ExecutionLimits::default())
            .expect("primitive ToNumber");
        assert_number(&result, expected);
    }
    let undefined = context.undefined();
    let result = context
        .call(&plus, &[undefined], ExecutionLimits::default())
        .expect("undefined ToNumber");
    assert_nan(&result);
}

#[test]
fn prefix_and_postfix_updates_convert_and_write_back_numbers() {
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let pre_inc = instantiate(
        &mut context,
        "function preInc(value){let current=value;return ++current;}",
        "preInc",
    );
    let pre_dec = instantiate(
        &mut context,
        "function preDec(value){let current=value;return --current;}",
        "preDec",
    );
    let post_inc = instantiate(
        &mut context,
        "function postInc(value){let current=value;let old=current++;return old*10+current;}",
        "postInc",
    );
    let post_dec = instantiate(
        &mut context,
        "function postDec(value){let current=value;let old=current--;return old*10+current;}",
        "postDec",
    );
    let input = string(&context, "4");

    let result = context
        .call(
            &pre_inc,
            std::slice::from_ref(&input),
            ExecutionLimits::default(),
        )
        .expect("prefix increment");
    assert_number(&result, 5.0);
    let result = context
        .call(
            &pre_dec,
            std::slice::from_ref(&input),
            ExecutionLimits::default(),
        )
        .expect("prefix decrement");
    assert_number(&result, 3.0);
    let result = context
        .call(
            &post_inc,
            std::slice::from_ref(&input),
            ExecutionLimits::default(),
        )
        .expect("postfix increment");
    assert_number(&result, 45.0);
    let result = context
        .call(&post_dec, &[input], ExecutionLimits::default())
        .expect("postfix decrement");
    assert_number(&result, 43.0);
}

#[test]
fn addition_selects_string_concatenation_only_after_primitive_conversion() {
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let add = instantiate(
        &mut context,
        "function add(left,right){return left+right;}",
        "add",
    );
    let one = context.number(JsNumber::from_i32(1));
    let two = context.number(JsNumber::from_i32(2));

    let result = context
        .call(&add, &[one.clone(), two], ExecutionLimits::default())
        .expect("numeric add");
    assert_number(&result, 3.0);
    let left_text = string(&context, "1");
    let result = context
        .call(&add, &[left_text, one.clone()], ExecutionLimits::default())
        .expect("left string concatenation");
    assert_eq!(text(&result), "11");
    let right_text = string(&context, "2");
    let result = context
        .call(&add, &[one.clone(), right_text], ExecutionLimits::default())
        .expect("right string concatenation");
    assert_eq!(text(&result), "12");
    let null = context.null();
    let result = context
        .call(&add, &[null, one.clone()], ExecutionLimits::default())
        .expect("null numeric add");
    assert_number(&result, 1.0);
    let truth = context.boolean(true);
    let result = context
        .call(&add, &[truth, one], ExecutionLimits::default())
        .expect("Boolean numeric add");
    assert_number(&result, 2.0);
}

#[test]
#[allow(clippy::too_many_lines)]
fn arithmetic_operators_cover_finite_and_ieee_754_edge_results() {
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let sub = instantiate(
        &mut context,
        "function sub(left,right){return left-right;}",
        "sub",
    );
    let mul = instantiate(
        &mut context,
        "function mul(left,right){return left*right;}",
        "mul",
    );
    let div = instantiate(
        &mut context,
        "function div(left,right){return left/right;}",
        "div",
    );
    let rem = instantiate(
        &mut context,
        "function rem(left,right){return left%right;}",
        "rem",
    );
    let pow = instantiate(
        &mut context,
        "function pow(left,right){return left**right;}",
        "pow",
    );
    let seven = context.number(JsNumber::from_i32(7));
    let three = context.number(JsNumber::from_i32(3));

    let result = context
        .call(
            &sub,
            &[seven.clone(), three.clone()],
            ExecutionLimits::default(),
        )
        .expect("subtraction");
    assert_number(&result, 4.0);
    let result = context
        .call(
            &mul,
            &[seven.clone(), three.clone()],
            ExecutionLimits::default(),
        )
        .expect("multiplication");
    assert_number(&result, 21.0);
    let result = context
        .call(
            &rem,
            &[context.number(JsNumber::from_i32(-7)), three.clone()],
            ExecutionLimits::default(),
        )
        .expect("remainder");
    assert_number(&result, -1.0);
    let result = context
        .call(
            &pow,
            &[context.number(JsNumber::from_i32(2)), three.clone()],
            ExecutionLimits::default(),
        )
        .expect("exponentiation");
    assert_number(&result, 8.0);

    let positive_zero = context.number(JsNumber::from_f64(0.0));
    let negative_zero = context.number(JsNumber::from_f64(-0.0));
    let one = context.number(JsNumber::from_i32(1));
    let result = context
        .call(
            &div,
            &[one.clone(), positive_zero],
            ExecutionLimits::default(),
        )
        .expect("positive infinity");
    assert_number(&result, f64::INFINITY);
    let result = context
        .call(
            &div,
            &[one, negative_zero.clone()],
            ExecutionLimits::default(),
        )
        .expect("negative infinity");
    assert_number(&result, f64::NEG_INFINITY);
    let result = context
        .call(
            &rem,
            &[negative_zero.clone(), three.clone()],
            ExecutionLimits::default(),
        )
        .expect("signed zero remainder");
    assert_number(&result, -0.0);
    let result = context
        .call(&pow, &[negative_zero, three], ExecutionLimits::default())
        .expect("signed zero exponentiation");
    assert_number(&result, -0.0);
    let zero = context.number(JsNumber::from_f64(0.0));
    let result = context
        .call(
            &div,
            &[zero.clone(), zero.clone()],
            ExecutionLimits::default(),
        )
        .expect("zero divided by zero");
    assert_nan(&result);
    let infinity = context.number(JsNumber::from_f64(f64::INFINITY));
    let result = context
        .call(&mul, &[infinity, zero.clone()], ExecutionLimits::default())
        .expect("infinity multiplied by zero");
    assert_nan(&result);
    let nan = context.number(JsNumber::from_f64(f64::NAN));
    let result = context
        .call(&pow, &[nan, zero], ExecutionLimits::default())
        .expect("NaN to the zeroth power");
    assert_number(&result, 1.0);
}

#[test]
fn shifts_and_bitwise_operators_use_int32_and_uint32_conversions() {
    let source = "function run(value,count){\
        return (value<<count)===-2&&\
            (value>>count)===-1&&\
            (value>>>count)===2147483647&&\
            (value&6)===6&&\
            (value|8)===-1&&\
            (value^3)===-4;\
    }";
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = instantiate(&mut context, source, "run");
    let value = context.number(JsNumber::from_i32(-1));
    let count = context.number(JsNumber::from_i32(1));

    let result = context
        .call(&run, &[value, count], ExecutionLimits::default())
        .expect("shift and bitwise operators");
    assert!(boolean(&result));
}

#[test]
fn relational_operators_cover_numbers_nan_and_utf16_string_order() {
    let ordered_source = "function ordered(left,right){\
        return left<right&&left<=right&&right>left&&right>=left;\
    }";
    let unordered_source = "function unordered(value,nan){\
        return !(nan<value)&&!(nan<=value)&&!(nan>value)&&!(nan>=value);\
    }";
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let ordered = instantiate(&mut context, ordered_source, "ordered");
    let unordered = instantiate(&mut context, unordered_source, "unordered");

    let two = context.number(JsNumber::from_i32(2));
    let three = context.number(JsNumber::from_i32(3));
    let result = context
        .call(&ordered, &[two, three], ExecutionLimits::default())
        .expect("numeric ordering");
    assert!(boolean(&result));
    let nan = context.number(JsNumber::from_f64(f64::NAN));
    let one = context.number(JsNumber::from_i32(1));
    let result = context
        .call(&unordered, &[one, nan], ExecutionLimits::default())
        .expect("NaN is unordered");
    assert!(boolean(&result));

    let surrogate_pair = string(&context, "\u{1f4a9}");
    let private_use = string(&context, "\u{e000}");
    let result = context
        .call(
            &ordered,
            &[surrogate_pair, private_use],
            ExecutionLimits::default(),
        )
        .expect("UTF-16 ordering");
    assert!(boolean(&result));
    let high_surrogate =
        context.string(JsString::from_code_units([0xd800_u16]).expect("high surrogate"));
    let low_surrogate =
        context.string(JsString::from_code_units([0xdc00_u16]).expect("low surrogate"));
    let result = context
        .call(
            &ordered,
            &[high_surrogate, low_surrogate],
            ExecutionLimits::default(),
        )
        .expect("lone-surrogate ordering");
    assert!(boolean(&result));
}

#[test]
fn loose_equality_and_inequality_cover_primitive_coercions_and_symbols() {
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let equal = instantiate(
        &mut context,
        "function equal(left,right){return left==right;}",
        "equal",
    );
    let unequal = instantiate(
        &mut context,
        "function unequal(left,right){return left!=right;}",
        "unequal",
    );
    let object_equal = instantiate(
        &mut context,
        "function objectEqual(){return ({valueOf(){return 7;}})==7;}",
        "objectEqual",
    );

    let cases = [
        (context.null(), context.undefined()),
        (
            context.boolean(false),
            context.number(JsNumber::from_i32(0)),
        ),
        (string(&context, ""), context.number(JsNumber::from_i32(0))),
        (string(&context, "0"), context.boolean(false)),
        (context.boolean(true), context.number(JsNumber::from_i32(1))),
        (string(&context, "1"), context.number(JsNumber::from_i32(1))),
    ];
    for (left, right) in cases {
        let result = context
            .call(&equal, &[left, right], ExecutionLimits::default())
            .expect("loose equality");
        assert!(boolean(&result));
    }
    let nan = context.number(JsNumber::from_f64(f64::NAN));
    let result = context
        .call(&unequal, &[nan.clone(), nan], ExecutionLimits::default())
        .expect("NaN loose inequality");
    assert!(boolean(&result));

    let description = JsString::from_utf8("token").expect("description");
    let symbol = context.symbol(Some(&description)).expect("symbol");
    let same_symbol = symbol.clone();
    let other_symbol = context.symbol(Some(&description)).expect("other symbol");
    let result = context
        .call(
            &equal,
            &[symbol.clone(), same_symbol],
            ExecutionLimits::default(),
        )
        .expect("same Symbol equality");
    assert!(boolean(&result));
    let result = context
        .call(
            &unequal,
            &[symbol.clone(), other_symbol],
            ExecutionLimits::default(),
        )
        .expect("different Symbol inequality");
    assert!(boolean(&result));
    let token = string(&context, "token");
    let result = context
        .call(&unequal, &[symbol, token], ExecutionLimits::default())
        .expect("Symbol/string inequality");
    assert!(boolean(&result));
    let result = context
        .call(&object_equal, &[], ExecutionLimits::default())
        .expect("object/Number equality");
    assert!(boolean(&result));
}

#[test]
fn ordinary_object_to_primitive_uses_value_of_then_to_string() {
    let fallback_source = "function fallback(){\
        let log=\"\";\
        let value={\
            valueOf(){log=log+\"v\";return {};},\
            toString(){log=log+\"t\";return \"7\";}\
        };\
        let converted=+value;\
        return log+\":\"+converted;\
    }";
    let short_source = "function shortCircuit(){\
        let log=\"\";\
        let value={\
            valueOf(){log=log+\"v\";return 8;},\
            toString(){throw 99;}\
        };\
        let converted=+value;\
        return log+\":\"+converted;\
    }";
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let fallback = instantiate(&mut context, fallback_source, "fallback");
    let short = instantiate(&mut context, short_source, "shortCircuit");

    let result = context
        .call(&fallback, &[], ExecutionLimits::default())
        .expect("ordinary fallback");
    assert_eq!(text(&result), "vt:7");
    let result = context
        .call(&short, &[], ExecutionLimits::default())
        .expect("valueOf short circuit");
    assert_eq!(text(&result), "v:8");
}

#[test]
fn symbol_to_primitive_precedes_ordinary_methods_and_receives_exact_hints() {
    let number_source = "function numberHint(key){\
        let log=\"\";\
        let value={\
            get [key](){\
                log=log+\"g\";\
                return function(hint){log=log+\"c\"+hint;return 7;};\
            },\
            valueOf(){throw 91;},\
            toString(){throw 92;}\
        };\
        let converted=+value;\
        return log+\":\"+converted;\
    }";
    let default_source = "function defaultHint(key){\
        let log=\"\";\
        let value={\
            get [key](){\
                log=log+\"g\";\
                return function(hint){log=log+\"c\"+hint;return 7;};\
            },\
            valueOf(){throw 93;},\
            toString(){throw 94;}\
        };\
        let converted=value+1;\
        return log+\":\"+converted;\
    }";
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let number_hint = instantiate(&mut context, number_source, "numberHint");
    let default_hint = instantiate(&mut context, default_source, "defaultHint");
    let key = context
        .well_known_symbol(PredefinedAtom::SymbolToPrimitive)
        .expect("Symbol.toPrimitive");

    let result = context
        .call(
            &number_hint,
            std::slice::from_ref(&key),
            ExecutionLimits::default(),
        )
        .expect("number hint");
    assert_eq!(text(&result), "gcnumber:7");
    let result = context
        .call(&default_hint, &[key], ExecutionLimits::default())
        .expect("default hint");
    assert_eq!(text(&result), "gcdefault:8");
}

#[test]
fn object_to_primitive_abrupt_completions_stop_fallback_exactly() {
    let getter = compile(
        "function getter(key){\
            let value={\
                get [key](){throw 31;},\
                valueOf(){throw 41;}\
            };\
            return +value;\
        }",
        "getter",
    );
    let exotic_call = compile(
        "function exoticCall(key){\
            let value={\
                [key](){throw 32;},\
                valueOf(){throw 42;}\
            };\
            return +value;\
        }",
        "exoticCall",
    );
    let value_of = compile(
        "function valueOfAbrupt(){\
            let value={valueOf(){throw 33;},toString(){throw 43;}};\
            return +value;\
        }",
        "valueOfAbrupt",
    );
    let to_string = compile(
        "function toStringAbrupt(){\
            let value={valueOf(){return {};},toString(){throw 34;}};\
            return +value;\
        }",
        "toStringAbrupt",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let getter = context.instantiate(getter).expect("getter installation");
    let exotic_call = context
        .instantiate(exotic_call)
        .expect("exotic call installation");
    let value_of = context.instantiate(value_of).expect("valueOf installation");
    let to_string = context
        .instantiate(to_string)
        .expect("toString installation");
    let key = context
        .well_known_symbol(PredefinedAtom::SymbolToPrimitive)
        .expect("Symbol.toPrimitive");

    assert_thrown_number(
        context.call(
            &getter,
            std::slice::from_ref(&key),
            ExecutionLimits::default(),
        ),
        31.0,
    );
    assert_thrown_number(
        context.call(&exotic_call, &[key], ExecutionLimits::default()),
        32.0,
    );
    assert_thrown_number(
        context.call(&value_of, &[], ExecutionLimits::default()),
        33.0,
    );
    assert_thrown_number(
        context.call(&to_string, &[], ExecutionLimits::default()),
        34.0,
    );
}

#[test]
fn nonprimitive_object_conversion_results_throw_exact_type_error() {
    let exotic_source = "function exotic(key){\
        return +({[key](){return {};}});\
    }";
    let ordinary_source = "function ordinary(){\
        return +({valueOf(){return {};},toString(){return {};}});\
    }";
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let exotic = instantiate(&mut context, exotic_source, "exotic");
    let ordinary = instantiate(&mut context, ordinary_source, "ordinary");
    let key = context
        .well_known_symbol(PredefinedAtom::SymbolToPrimitive)
        .expect("Symbol.toPrimitive");

    assert_type_error(
        context.call(&exotic, &[key], ExecutionLimits::default()),
        "toPrimitive",
    );
    assert_type_error(
        context.call(&ordinary, &[], ExecutionLimits::default()),
        "toPrimitive",
    );
}

#[test]
fn binary_operators_convert_the_left_operand_before_the_right_operand() {
    let add_source = "function addOrder(){\
        let log=\"\";\
        let left={valueOf(){log=log+\"L\";return 1;}};\
        let right={valueOf(){log=log+\"R\";return 2;}};\
        let result=left+right;\
        return log+\":\"+result;\
    }";
    let relation_source = "function relationOrder(){\
        let log=\"\";\
        let left={valueOf(){log=log+\"L\";return 1;}};\
        let right={valueOf(){log=log+\"R\";return 2;}};\
        let result=left<right;\
        return log+\":\"+result;\
    }";
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let add = instantiate(&mut context, add_source, "addOrder");
    let relation = instantiate(&mut context, relation_source, "relationOrder");

    let result = context
        .call(&add, &[], ExecutionLimits::default())
        .expect("addition conversion order");
    assert_eq!(text(&result), "LR:3");
    let result = context
        .call(&relation, &[], ExecutionLimits::default())
        .expect("relational conversion order");
    assert_eq!(text(&result), "LR:true");
}

#[test]
fn symbol_numeric_and_string_coercions_throw_exact_type_errors() {
    let plus = compile("function plus(value){return +value;}", "plus");
    let negate = compile("function negate(value){return -value;}", "negate");
    let bit_not = compile("function bitNot(value){return ~value;}", "bitNot");
    let add = compile("function add(left,right){return left+right;}", "add");
    let sub = compile("function sub(left,right){return left-right;}", "sub");
    let shift = compile("function shift(left,right){return left<<right;}", "shift");
    let relation = compile(
        "function relation(left,right){return left<right;}",
        "relation",
    );
    let mut runtime = runtime();
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let plus = context.instantiate(plus).expect("plus");
    let negate = context.instantiate(negate).expect("negate");
    let bit_not = context.instantiate(bit_not).expect("bitNot");
    let add = context.instantiate(add).expect("add");
    let sub = context.instantiate(sub).expect("sub");
    let shift = context.instantiate(shift).expect("shift");
    let relation = context.instantiate(relation).expect("relation");
    let description = JsString::from_utf8("token").expect("description");
    let symbol = context.symbol(Some(&description)).expect("symbol");
    let one = context.number(JsNumber::from_i32(1));

    assert_type_error(
        context.call(
            &plus,
            std::slice::from_ref(&symbol),
            ExecutionLimits::default(),
        ),
        "cannot convert symbol to number",
    );
    assert_type_error(
        context.call(
            &negate,
            std::slice::from_ref(&symbol),
            ExecutionLimits::default(),
        ),
        "cannot convert symbol to number",
    );
    assert_type_error(
        context.call(
            &bit_not,
            std::slice::from_ref(&symbol),
            ExecutionLimits::default(),
        ),
        "cannot convert symbol to number",
    );
    assert_type_error(
        context.call(
            &add,
            &[symbol.clone(), one.clone()],
            ExecutionLimits::default(),
        ),
        "cannot convert symbol to number",
    );
    let prefix = string(&context, "x");
    assert_type_error(
        context.call(&add, &[prefix, symbol.clone()], ExecutionLimits::default()),
        "cannot convert symbol to string",
    );
    assert_type_error(
        context.call(
            &sub,
            &[symbol.clone(), one.clone()],
            ExecutionLimits::default(),
        ),
        "cannot convert symbol to number",
    );
    assert_type_error(
        context.call(
            &shift,
            &[symbol.clone(), one.clone()],
            ExecutionLimits::default(),
        ),
        "cannot convert symbol to number",
    );
    assert_type_error(
        context.call(&relation, &[symbol, one], ExecutionLimits::default()),
        "cannot convert symbol to number",
    );
}
