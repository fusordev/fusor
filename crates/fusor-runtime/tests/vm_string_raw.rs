//! `String.raw` array-like traversal and observable coercion semantics.

use std::{error::Error, fmt, sync::Arc};

use fusor_bytecode::{VerificationLimits, VerifiedBytecode};
use fusor_compiler::CompilationContext;
use fusor_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use fusor_runtime::{
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
                let context = CompilationContext::new_with_source_name(
                    unit,
                    Arc::from("<runtime String.raw>"),
                )
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
fn string_raw_identity_descriptor_order_and_default_results_are_exact() {
    assert_all(&[
        (
            "Object.getOwnPropertyNames(String).join(',')",
            "length,name,fromCharCode,fromCodePoint,raw,prototype",
        ),
        ("String.raw.length+'|'+String.raw.name", "1|raw"),
        (
            "(function(){const d=Object.getOwnPropertyDescriptor(String,'raw');return d.writable+'|'+d.enumerable+'|'+d.configurable})()",
            "true|false|true",
        ),
        (
            "(function(){try{Reflect.construct(String.raw,[])}catch(e){return e instanceof TypeError}})()",
            "true",
        ),
        ("String.raw({raw:['a','b','c']},1,2)", "a1b2c"),
        ("String.raw({raw:['a','b','c']},1)", "a1bc"),
        ("String.raw({raw:['a']},1,2)", "a"),
        ("String.raw({raw:'xy'},1)", "x1y"),
        ("String.raw.call(null,{raw:['x','y']},1)", "x1y"),
    ]);
}

#[test]
fn string_raw_observes_raw_length_literal_and_substitution_order() {
    assert_all(&[
        (
            "(function(){let log=[];const sub={toString(){log.push('sub-conv');return 'S'}};const literals={get length(){log.push('length');return {valueOf(){log.push('length-conv');return 2}}},get 0(){log.push('lit0');return {toString(){log.push('lit0-conv');return 'A'}}},get 1(){log.push('lit1');return {toString(){log.push('lit1-conv');return 'B'}}}};const template={get raw(){log.push('raw');return literals}};const result=String.raw(template,sub);return log.join(',')+'|'+result})()",
            "raw,length,length-conv,lit0,lit0-conv,sub-conv,lit1,lit1-conv|ASB",
        ),
        (
            "(function(){let count=0;const sub={toString(){count++;return 'x'}};const result=String.raw({raw:{length:0}},sub);return result+':'+count})()",
            ":0",
        ),
        (
            "(function(){let count=0;const sub={toString(){count++;return 'x'}};const result=String.raw({raw:{0:'a',length:1}},sub);return result+':'+count})()",
            "a:0",
        ),
    ]);
}

#[test]
fn string_raw_preserves_exact_utf16_code_units() {
    assert_all(&[
        (
            "(function(){const s=String.raw({raw:[String.fromCharCode(0xD800),String.fromCharCode(0xDC00)]},String.fromCharCode(0xDFFF));return s.length+'|'+s.charCodeAt(0)+'|'+s.charCodeAt(1)+'|'+s.charCodeAt(2)})()",
            "3|55296|57343|56320",
        ),
        (
            "(function(){const s=String.raw({raw:['😀','x']},'😀');return s.length+'|'+s.codePointAt(0)+'|'+s.codePointAt(2)})()",
            "5|128512|128512",
        ),
    ]);
}

#[test]
fn string_raw_propagates_to_object_getter_and_conversion_abruptions() {
    assert_all(&[
        (
            "(function(){try{String.raw(null)}catch(e){return e instanceof TypeError}})()",
            "true",
        ),
        (
            "(function(){try{String.raw({raw:null})}catch(e){return e instanceof TypeError}})()",
            "true",
        ),
        (
            "(function(){try{String.raw({get raw(){throw 'raw'}})}catch(e){return e}})()",
            "raw",
        ),
        (
            "(function(){try{String.raw({raw:{get length(){throw 'length'}}})}catch(e){return e}})()",
            "length",
        ),
        (
            "(function(){try{String.raw({raw:{length:{valueOf(){throw 'length-conv'}}}})}catch(e){return e}})()",
            "length-conv",
        ),
        (
            "(function(){try{String.raw({raw:{length:1,get 0(){throw 'literal'}}})}catch(e){return e}})()",
            "literal",
        ),
        (
            "(function(){try{String.raw({raw:{0:{toString(){throw 'literal-conv'}},length:1}})}catch(e){return e}})()",
            "literal-conv",
        ),
        (
            "(function(){try{String.raw({raw:['a','b']},{toString(){throw 'sub-conv'}})}catch(e){return e}})()",
            "sub-conv",
        ),
    ]);
}

#[test]
fn string_raw_array_like_scans_consume_shared_instruction_fuel() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = dynamic_function(&mut context, "return String.raw({raw:{length:200}});");

    let result = context.call(
        &run,
        &[],
        ExecutionLimits::default().with_instruction_fuel(100),
    );

    assert!(matches!(
        result,
        Err(ExecutionError::InstructionLimitExceeded { limit: 100, .. })
    ));
}
