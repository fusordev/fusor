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
            "min,max,abs,floor,ceil,round,sqrt,acos,asin,atan",
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
