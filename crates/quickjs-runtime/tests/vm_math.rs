//! `%Math%` object shape and the first specification-order method tranche.
//!
//! The special-value, signed-zero, coercion-order, descriptor, and identity
//! expectations are pinned against ECMA-262 and `QuickJS` 2026-06-04.

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExecutionError, ExecutionLimits, Function, JsString,
    JsValue, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource, Runtime,
    RuntimeLimits,
};

#[derive(Debug)]
struct TestCompileError(String);

impl fmt::Display for TestCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for TestCompileError {}

struct TestCompiler;

impl OrdinaryDynamicFunctionCompiler for TestCompiler {
    fn compile(
        &self,
        source: OrdinaryDynamicFunctionSource,
    ) -> Result<Arc<VerifiedBytecode>, DynamicFunctionCompileFailure> {
        let body = source.body().to_utf8_lossy().map_err(engine_failure)?;
        let dynamic_source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &[],
            SourceFragment::new(&body),
        );
        with_dynamic_function_source(
            dynamic_source,
            FrontendLimits::default(),
            |unit, _prepared| {
                let context =
                    CompilationContext::new_with_source_name(unit, Arc::from("<runtime Math>"))
                        .map_err(engine_failure)?;
                context
                    .compile_dynamic_function_script(VerificationLimits::default())
                    .map(|tree| Arc::new(tree.verified_bytecode().clone()))
                    .map_err(engine_failure)
            },
        )
        .map_err(engine_failure)?
    }
}

fn engine_failure(error: impl Error + Send + Sync + 'static) -> DynamicFunctionCompileFailure {
    DynamicFunctionCompileFailure::Engine {
        source: Arc::new(TestCompileError(error.to_string())),
    }
}

fn dynamic_function(context: &mut Context<'_>, body: &str) -> Function {
    let authority = TestCompiler
        .compile(OrdinaryDynamicFunctionSource::new(
            Arc::from([]),
            JsString::from_utf8(body).expect("body"),
        ))
        .expect("dynamic Function authority");
    context
        .execute_dynamic_function_script(authority, ExecutionLimits::default())
        .expect("dynamic Function Script")
        .into_function()
        .expect("dynamic Function")
}

fn evaluate<T>(body: &str, project: impl FnOnce(Result<JsValue, ExecutionError>) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, body);
    let result = context.call(&run, &[], ExecutionLimits::default());
    project(result)
}

fn rendered(expression: &str) -> String {
    evaluate(&format!("return String({expression});"), |result| {
        result
            .expect("completed")
            .as_string()
            .expect("live value")
            .expect("String")
            .to_utf8_lossy()
            .expect("UTF-8")
    })
}

fn assert_all(cases: &[(&str, &str)]) {
    for (expression, expected) in cases {
        assert_eq!(rendered(expression), *expected, "{expression}");
    }
}

#[test]
fn math_is_an_ordinary_tagged_object_with_exact_prefix_order() {
    assert_all(&[
        ("Object.getPrototypeOf(Math)===Object.prototype", "true"),
        ("Object.prototype.toString.call(Math)", "[object Math]"),
        (
            "Object.getOwnPropertyNames(Math).join(',')",
            "min,max,abs,floor,ceil,round,sqrt,acos,asin,atan,atan2,cos,exp,log,pow,sin,tan,trunc,sign,cosh,sinh,tanh,acosh,asinh,atanh,expm1,log1p,log2,log10,cbrt,hypot,random,f16round,fround,imul,clz32,sumPrecise",
        ),
        ("Object.getOwnPropertySymbols(Math).length", "1"),
        (
            "Object.getOwnPropertySymbols(Math)[0]===Symbol.toStringTag",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Math,Symbol.toStringTag).value",
            "Math",
        ),
        (
            "Object.getOwnPropertyDescriptor(Math,Symbol.toStringTag).writable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Math,Symbol.toStringTag).configurable",
            "true",
        ),
        (
            "(function(){const d=Object.getOwnPropertyDescriptor(this,'Math');return d.writable+'|'+d.enumerable+'|'+d.configurable})()",
            "true|false|true",
        ),
        (
            "(function(){try{Math()}catch(e){return e instanceof TypeError}})()",
            "true",
        ),
        (
            "(function(){try{Reflect.construct(Math,[])}catch(e){return e instanceof TypeError}})()",
            "true",
        ),
    ]);
}

#[test]
fn math_method_identities_and_descriptors_are_exact() {
    assert_all(&[
        ("Math.min.length+'|'+Math.min.name", "2|min"),
        ("Math.max.length+'|'+Math.max.name", "2|max"),
        ("Math.abs.length+'|'+Math.abs.name", "1|abs"),
        ("Math.atan.length+'|'+Math.atan.name", "1|atan"),
        ("Math.atan2.length+'|'+Math.atan2.name", "2|atan2"),
        ("Math.pow.length+'|'+Math.pow.name", "2|pow"),
        ("Math.sign.length+'|'+Math.sign.name", "1|sign"),
        ("Math.cosh.length+'|'+Math.cosh.name", "1|cosh"),
        ("Math.cbrt.length+'|'+Math.cbrt.name", "1|cbrt"),
        ("Math.hypot.length+'|'+Math.hypot.name", "2|hypot"),
        ("Math.random.length+'|'+Math.random.name", "0|random"),
        ("Math.f16round.length+'|'+Math.f16round.name", "1|f16round"),
        ("Math.fround.length+'|'+Math.fround.name", "1|fround"),
        ("Math.imul.length+'|'+Math.imul.name", "2|imul"),
        ("Math.clz32.length+'|'+Math.clz32.name", "1|clz32"),
        (
            "Math.sumPrecise.length+'|'+Math.sumPrecise.name",
            "1|sumPrecise",
        ),
        (
            "Object.getOwnPropertyNames(Math.min).join(',')",
            "length,name",
        ),
        (
            "(function(){const d=Object.getOwnPropertyDescriptor(Math,'min');return d.writable+'|'+d.enumerable+'|'+d.configurable})()",
            "true|false|true",
        ),
        (
            "(function(){try{Reflect.construct(Math.min,[])}catch(e){return e instanceof TypeError}})()",
            "true",
        ),
        ("Math.abs.call(null,-2)", "2"),
    ]);
}

#[test]
fn math_min_and_max_preserve_nan_infinity_and_zero_sign_rules() {
    assert_all(&[
        ("Math.min()", "Infinity"),
        ("Math.max()", "-Infinity"),
        ("Math.min(3,-2,7,0)", "-2"),
        ("Math.max(3,-2,7,0)", "7"),
        ("Number.isNaN(Math.min(1,NaN,2))", "true"),
        ("Number.isNaN(Math.max(NaN))", "true"),
        ("1/Math.min(0,-0)", "-Infinity"),
        ("1/Math.min(-0,0)", "-Infinity"),
        ("1/Math.max(0,-0)", "Infinity"),
        ("1/Math.max(-0,0)", "Infinity"),
        ("Math.min('4',true,null)", "0"),
        ("Math.max('4',true,null)", "4"),
    ]);
}

#[test]
fn math_extrema_convert_every_argument_in_left_to_right_order() {
    assert_all(&[
        (
            "(function(){let log=[];const a={valueOf(){log.push('a');return NaN}};const b={valueOf(){log.push('b');return 1}};const r=Math.min(a,b);return log.join(',')+'|'+Number.isNaN(r)})()",
            "a,b|true",
        ),
        (
            "(function(){let log=[];const a={valueOf(){log.push('a');return 3}};const b={valueOf(){log.push('b');return 2}};const c={valueOf(){log.push('c');return 1}};Math.max(a,b,c);return log.join(',')})()",
            "a,b,c",
        ),
        (
            "(function(){try{Math.min(NaN,{valueOf(){throw 'later'}})}catch(e){return e}})()",
            "later",
        ),
        (
            "(function(){try{Math.max(1,Symbol())}catch(e){return e instanceof TypeError}})()",
            "true",
        ),
        (
            "(function(){try{Math.min(1n)}catch(e){return e instanceof TypeError}})()",
            "true",
        ),
    ]);
}

#[test]
fn unary_math_methods_match_special_values_and_signed_zero() {
    assert_all(&[
        ("1/Math.abs(-0)", "Infinity"),
        ("Math.abs(-Infinity)", "Infinity"),
        ("Number.isNaN(Math.abs())", "true"),
        ("1/Math.floor(-0)", "-Infinity"),
        ("1/Math.ceil(-0.1)", "-Infinity"),
        ("Math.floor(1.9)", "1"),
        ("Math.ceil(1.1)", "2"),
        ("1/Math.sqrt(-0)", "-Infinity"),
        ("Math.sqrt(9)", "3"),
        ("Number.isNaN(Math.sqrt(-1))", "true"),
        ("1/Math.acos(1)", "Infinity"),
        ("Number.isNaN(Math.acos(2))", "true"),
        ("1/Math.asin(-0)", "-Infinity"),
        ("Number.isNaN(Math.asin(-2))", "true"),
        ("1/Math.atan(-0)", "-Infinity"),
        ("Math.atan(Infinity)>1.5", "true"),
        ("Math.atan(-Infinity)<-1.5", "true"),
        ("Math.cos(-0)", "1"),
        ("Number.isNaN(Math.cos(Infinity))", "true"),
        ("Math.exp(-Infinity)", "0"),
        ("Math.exp(Infinity)", "Infinity"),
        ("Math.exp(-0)", "1"),
        ("1/Math.log(1)", "Infinity"),
        ("Math.log(-0)", "-Infinity"),
        ("Number.isNaN(Math.log(-1))", "true"),
        ("1/Math.sin(-0)", "-Infinity"),
        ("Number.isNaN(Math.sin(Infinity))", "true"),
        ("1/Math.tan(-0)", "-Infinity"),
        ("Number.isNaN(Math.tan(-Infinity))", "true"),
        ("Math.trunc(1.9)", "1"),
        ("Math.trunc(-1.9)", "-1"),
        ("1/Math.trunc(-0.9)", "-Infinity"),
        ("Math.sign(Infinity)", "1"),
        ("Math.sign(-Infinity)", "-1"),
        ("1/Math.sign(-0)", "-Infinity"),
        ("Number.isNaN(Math.sign(NaN))", "true"),
        ("Math.cosh(-Infinity)", "Infinity"),
        ("Math.cosh(-0)", "1"),
        ("Number.isNaN(Math.cosh(NaN))", "true"),
        ("1/Math.sinh(-0)", "-Infinity"),
        ("Math.sinh(-Infinity)", "-Infinity"),
        ("Math.sinh(Infinity)", "Infinity"),
        ("1/Math.tanh(-0)", "-Infinity"),
        ("Math.tanh(-Infinity)", "-1"),
        ("Math.tanh(Infinity)", "1"),
        ("1/Math.acosh(1)", "Infinity"),
        ("Math.acosh(Infinity)", "Infinity"),
        ("Number.isNaN(Math.acosh(0))", "true"),
        ("1/Math.asinh(-0)", "-Infinity"),
        ("Math.asinh(-Infinity)", "-Infinity"),
        ("Math.asinh(Infinity)", "Infinity"),
        ("1/Math.atanh(-0)", "-Infinity"),
        ("Math.atanh(1)", "Infinity"),
        ("Math.atanh(-1)", "-Infinity"),
        ("Number.isNaN(Math.atanh(2))", "true"),
        ("1/Math.expm1(-0)", "-Infinity"),
        ("Math.expm1(-Infinity)", "-1"),
        ("Math.expm1(Infinity)", "Infinity"),
        ("1/Math.log1p(-0)", "-Infinity"),
        ("Math.log1p(-1)", "-Infinity"),
        ("Number.isNaN(Math.log1p(-2))", "true"),
        ("1/Math.log2(1)", "Infinity"),
        ("Math.log2(-0)", "-Infinity"),
        ("Number.isNaN(Math.log2(-1))", "true"),
        ("1/Math.log10(1)", "Infinity"),
        ("Math.log10(-0)", "-Infinity"),
        ("Number.isNaN(Math.log10(-1))", "true"),
        ("1/Math.cbrt(-0)", "-Infinity"),
        ("Math.cbrt(-Infinity)", "-Infinity"),
        ("Math.cbrt(-8)", "-2"),
    ]);
}

#[test]
fn math_atan2_preserves_quadrants_infinities_and_zero_signs() {
    assert_all(&[
        ("1/Math.atan2(0,0)", "Infinity"),
        ("Math.atan2(0,-0)", "3.141592653589793"),
        ("1/Math.atan2(-0,0)", "-Infinity"),
        ("Math.atan2(-0,-0)", "-3.141592653589793"),
        ("Math.atan2(Infinity,Infinity)", "0.7853981633974483"),
        ("Math.atan2(Infinity,-Infinity)", "2.356194490192345"),
        ("Math.atan2(-Infinity,Infinity)", "-0.7853981633974483"),
        ("Math.atan2(-Infinity,-Infinity)", "-2.356194490192345"),
        ("1/Math.atan2(1,Infinity)", "Infinity"),
        ("1/Math.atan2(-1,Infinity)", "-Infinity"),
        ("Number.isNaN(Math.atan2(NaN,1))", "true"),
        ("Number.isNaN(Math.atan2(1,NaN))", "true"),
    ]);
}

#[test]
fn math_pow_uses_number_exponentiation_edge_semantics() {
    assert_all(&[
        ("Math.pow(NaN,0)", "1"),
        ("Number.isNaN(Math.pow(1,NaN))", "true"),
        ("Number.isNaN(Math.pow(1,Infinity))", "true"),
        ("Number.isNaN(Math.pow(-1,-Infinity))", "true"),
        ("1/Math.pow(-0,3)", "-Infinity"),
        ("Math.pow(-0,-3)", "-Infinity"),
        ("1/Math.pow(-0,2)", "Infinity"),
        ("Math.pow(-0,-2)", "Infinity"),
        ("Number.isNaN(Math.pow(-2,0.5))", "true"),
        ("Math.pow(2,10)", "1024"),
        ("Math.pow(0,-1)", "Infinity"),
        ("Math.pow(Infinity,-1)", "0"),
    ]);
}

#[test]
fn binary_math_converts_left_then_right_and_propagates_abruptions() {
    assert_all(&[
        (
            "(function(){let log=[];const left={valueOf(){log.push('left');return 2}};const right={valueOf(){log.push('right');return 3}};const value=Math.pow(left,right);return log.join(',')+'|'+value})()",
            "left,right|8",
        ),
        (
            "(function(){try{Math.atan2(NaN,{valueOf(){throw 'right'}})}catch(e){return e}})()",
            "right",
        ),
        (
            "(function(){let touched=false;try{Math.pow(1n,{valueOf(){touched=true;return 2}})}catch(e){return (e instanceof TypeError)+'|'+touched}})()",
            "true|false",
        ),
        (
            "(function(){try{Math.atan2(1,Symbol())}catch(e){return e instanceof TypeError}})()",
            "true",
        ),
    ]);
}

#[test]
fn math_hypot_converts_all_arguments_and_avoids_intermediate_overflow() {
    assert_all(&[
        ("1/Math.hypot()", "Infinity"),
        ("1/Math.hypot(-0)", "Infinity"),
        ("Math.hypot(-3)", "3"),
        ("Math.hypot(3,4)", "5"),
        (
            "Number.isFinite(Math.hypot(3e200,4e200))&&Math.hypot(3e200,4e200)>4.9e200",
            "true",
        ),
        ("Math.hypot(Number.MIN_VALUE,Number.MIN_VALUE)>0", "true"),
        ("Math.hypot(NaN,Infinity)", "Infinity"),
        ("Math.hypot(-Infinity,NaN)", "Infinity"),
        (
            "(function(){let log=[];const a={valueOf(){log.push('a');return 3}};const b={valueOf(){log.push('b');return 4}};const value=Math.hypot(a,b);return log.join(',')+'|'+value})()",
            "a,b|5",
        ),
        (
            "(function(){try{Math.hypot(Infinity,{valueOf(){throw 'later'}})}catch(e){return e}})()",
            "later",
        ),
        (
            "(function(){try{Math.hypot(NaN,1n)}catch(e){return e instanceof TypeError}})()",
            "true",
        ),
    ]);
}

#[test]
fn math_random_is_bounded_stateful_and_ignores_arguments() {
    assert_all(&[
        (
            "(function(){const a=Math.random();const b=Math.random();return a>=0&&a<1&&b>=0&&b<1&&a!==b})()",
            "true",
        ),
        (
            "(function(){let touched=false;const value=Math.random({valueOf(){touched=true;throw 1}});return (value>=0&&value<1)+'|'+touched})()",
            "true|false",
        ),
    ]);
}

#[test]
fn math_random_sequences_are_distinct_across_realms() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let first_realm = runtime.create_realm().expect("first realm");
    let second_realm = runtime.create_realm().expect("second realm");

    let first = {
        let mut context = runtime.context(&first_realm).expect("first context");
        let run = dynamic_function(&mut context, "return Math.random();");
        context
            .call(&run, &[], ExecutionLimits::default())
            .expect("first random result")
            .as_number()
            .expect("live value")
            .expect("Number")
            .as_f64()
    };
    let second = {
        let mut context = runtime.context(&second_realm).expect("second context");
        let run = dynamic_function(&mut context, "return Math.random();");
        context
            .call(&run, &[], ExecutionLimits::default())
            .expect("second random result")
            .as_number()
            .expect("live value")
            .expect("Number")
            .as_f64()
    };

    assert_ne!(first.to_bits(), second.to_bits());
}

#[test]
fn math_float_rounding_uses_direct_ties_to_even_conversions() {
    assert_all(&[
        ("1/Math.f16round(-0)", "-Infinity"),
        ("Math.f16round(Infinity)", "Infinity"),
        ("Number.isNaN(Math.f16round(NaN))", "true"),
        ("Math.f16round(1.00048828125000022204)", "1.0009765625"),
        (
            "Math.f16round(5.960464477539063e-8)",
            "5.960464477539063e-8",
        ),
        ("1/Math.f16round(2.9802322387695312e-8)", "Infinity"),
        (
            "Math.f16round(2.980232238769532e-8)",
            "5.960464477539063e-8",
        ),
        ("Math.f16round(65519)", "65504"),
        ("Math.f16round(65520)", "Infinity"),
        ("1/Math.fround(-0)", "-Infinity"),
        ("Math.fround(1.337)", "1.3370000123977661"),
        ("Math.fround(Infinity)", "Infinity"),
        ("Number.isNaN(Math.fround(NaN))", "true"),
    ]);
}

#[test]
fn math_integer_methods_apply_uint32_in_specification_order() {
    assert_all(&[
        ("Math.imul()", "0"),
        ("Math.imul(0xffffffff,5)", "-5"),
        ("Math.imul(0x7fffffff,2)", "-2"),
        ("Math.imul(NaN,7)", "0"),
        ("Math.clz32()", "32"),
        ("Math.clz32(1)", "31"),
        ("Math.clz32(0x80000000)", "0"),
        ("Math.clz32(-1)", "0"),
        (
            "(function(){let log=[];const left={valueOf(){log.push('left');return 3}};const right={valueOf(){log.push('right');return 7}};const value=Math.imul(left,right);return log.join(',')+'|'+value})()",
            "left,right|21",
        ),
        (
            "(function(){let touched=false;try{Math.imul(1n,{valueOf(){touched=true;return 2}})}catch(e){return (e instanceof TypeError)+'|'+touched}})()",
            "true|false",
        ),
    ]);
}

#[test]
fn math_sum_precise_rounds_once_and_preserves_special_states() {
    assert_all(&[
        ("1/Math.sumPrecise([])", "-Infinity"),
        ("1/Math.sumPrecise([-0,-0])", "-Infinity"),
        ("1/Math.sumPrecise([-0,0])", "Infinity"),
        ("Math.sumPrecise([1,2,3])", "6"),
        ("Math.sumPrecise([1e30,1,-1e30])", "1"),
        ("Math.sumPrecise([0.1,0.2,0.3])", "0.6"),
        ("Math.sumPrecise([Infinity,1])", "Infinity"),
        ("Math.sumPrecise([-Infinity,1])", "-Infinity"),
        (
            "Number.isNaN(Math.sumPrecise([Infinity,-Infinity]))",
            "true",
        ),
        ("Number.isNaN(Math.sumPrecise([NaN,Infinity]))", "true"),
    ]);
}

#[test]
fn math_sum_precise_rejects_non_numbers_and_closes_after_yield() {
    assert_all(&[
        (
            "(function(){let closed=false;const items={[Symbol.iterator](){let sent=false;return {next(){if(sent)return {done:true};sent=true;return {done:false,value:'1'}},return(){closed=true;throw 'close'}}}};try{Math.sumPrecise(items)}catch(e){return (e instanceof TypeError)+'|'+closed}})()",
            "true|true",
        ),
        (
            "(function(){let converted=false;let closed=false;const boxed={valueOf(){converted=true;return 1}};const items={[Symbol.iterator](){let sent=false;return {next(){if(sent)return {done:true};sent=true;return {done:false,value:boxed}},return(){closed=true;return {done:true}}}}};try{Math.sumPrecise(items)}catch(e){return (e instanceof TypeError)+'|'+converted+'|'+closed}})()",
            "true|false|true",
        ),
    ]);
}

#[test]
fn math_sum_precise_does_not_close_iterator_step_value_failures() {
    assert_all(&[
        (
            "(function(){let closed=false;const items={[Symbol.iterator](){return {next(){return {done:false,get value(){throw 'value'}}},return(){closed=true;return {done:true}}}}};try{Math.sumPrecise(items)}catch(e){return e+'|'+closed}})()",
            "value|false",
        ),
        (
            "(function(){let closed=false;const items={[Symbol.iterator](){return {next(){return {get done(){throw 'done'}}},return(){closed=true;return {done:true}}}}};try{Math.sumPrecise(items)}catch(e){return e+'|'+closed}})()",
            "done|false",
        ),
    ]);
}

#[test]
fn math_round_uses_ecma_ties_toward_positive_infinity() {
    assert_all(&[
        ("Math.round(1.49)", "1"),
        ("Math.round(1.5)", "2"),
        ("Math.round(-1.49)", "-1"),
        ("Math.round(-1.5)", "-1"),
        ("Math.round(-1.51)", "-2"),
        ("1/Math.round(0.1)", "Infinity"),
        ("1/Math.round(-0.1)", "-Infinity"),
        ("1/Math.round(-0.5)", "-Infinity"),
        ("Math.round(0.5)", "1"),
        ("Math.round(Infinity)", "Infinity"),
        ("Math.round(-Infinity)", "-Infinity"),
        ("Number.isNaN(Math.round(NaN))", "true"),
    ]);
}

#[test]
fn unary_math_coercion_is_observable_and_abrupt() {
    assert_all(&[
        (
            "(function(){let log=[];const x={valueOf(){log.push('valueOf');return 4}};const r=Math.sqrt(x);return log.join(',')+'|'+r})()",
            "valueOf|2",
        ),
        (
            "(function(){try{Math.floor({valueOf(){throw 'boom'}})}catch(e){return e}})()",
            "boom",
        ),
        (
            "(function(){try{Math.abs(Symbol())}catch(e){return e instanceof TypeError}})()",
            "true",
        ),
        (
            "(function(){try{Math.sqrt(1n)}catch(e){return e instanceof TypeError}})()",
            "true",
        ),
    ]);
}

#[test]
fn variadic_math_conversion_consumes_shared_instruction_fuel() {
    let arguments = vec!["1"; 200].join(",");
    let body = format!("return Math.min({arguments});");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, &body);

    let result = context.call(
        &run,
        &[],
        ExecutionLimits::default().with_instruction_fuel(300),
    );

    assert!(matches!(
        result,
        Err(ExecutionError::InstructionLimitExceeded { limit: 300, .. })
    ));
}
