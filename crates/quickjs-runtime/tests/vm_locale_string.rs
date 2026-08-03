//! Deterministic no-`Intl` `toLocaleString` semantics.

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    Context, DynamicFunctionCompileFailure, ExceptionKind, ExecutionError, ExecutionLimits,
    Function, JsString, JsValue, OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource,
    Runtime, RuntimeLimits,
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
        let body_text = source.body().to_utf8_lossy().map_err(engine_failure)?;
        let dynamic_source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &[],
            SourceFragment::new(&body_text),
        );
        with_dynamic_function_source(
            dynamic_source,
            FrontendLimits::default(),
            |unit, _prepared| {
                let context = CompilationContext::new_with_source_name(
                    unit,
                    Arc::from("<runtime locale strings>"),
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

fn assert_throw_kind(body: &str, kind: ExceptionKind) {
    evaluate(body, |result| {
        let Err(ExecutionError::Exception(exception)) = result else {
            panic!("expected a JavaScript throw from {body}");
        };
        assert_eq!(exception.kind().expect("engine exception kind"), kind);
    });
}

#[test]
fn object_locale_string_dynamically_invokes_to_string_and_returns_its_value() {
    assert_all(&[
        (
            "(function(){let log='';const value={get toString(){log+='get|';return function(){log+='call';return 7;};}};const result=Object.prototype.toLocaleString.call(value);return log+'|'+typeof result+'|'+result;})()",
            "get|call|number|7",
        ),
        ("Object.prototype.toLocaleString.call('ab')", "ab"),
        ("Object.prototype.toLocaleString.call(3)", "3"),
    ]);
    assert_throw_kind(
        "return Object.prototype.toLocaleString.call(null);",
        ExceptionKind::TypeError,
    );
    assert_throw_kind(
        "return Object.prototype.toLocaleString.call({toString:1});",
        ExceptionKind::TypeError,
    );
}

#[test]
fn number_and_bigint_locale_strings_use_deterministic_decimal_rendering() {
    assert_all(&[
        ("Number.prototype.toLocaleString.call(1234.5)", "1234.5"),
        ("Number.prototype.toLocaleString.call(-0)", "0"),
        ("Number.prototype.toLocaleString.call(Object(12))", "12"),
        (
            "BigInt.prototype.toLocaleString.call(BigInt(-1234))",
            "-1234",
        ),
        (
            "BigInt.prototype.toLocaleString.call(Object(BigInt(12)))",
            "12",
        ),
        (
            "(function(){let used=false;const options={valueOf(){used=true;return 1;}};Number.prototype.toLocaleString.call(3,options);return used;})()",
            "false",
        ),
    ]);
    assert_throw_kind(
        "return Number.prototype.toLocaleString.call('1');",
        ExceptionKind::TypeError,
    );
    assert_throw_kind(
        "return BigInt.prototype.toLocaleString.call(1);",
        ExceptionKind::TypeError,
    );
}

#[test]
fn array_locale_string_invokes_each_present_value_and_uses_empty_nullish_fields() {
    assert_all(&[
        (
            "(function(){let log='';const a={toLocaleString(){log+='a|';return 'A';}};const b={toLocaleString(){log+='b|';return {toString(){log+='string|';return 'B';}};}};const c={toLocaleString(){log+='c';return 7;}};const result=[a,null,,b,c].toLocaleString('ignored');return log+'#'+result;})()",
            "a|b|string|c#A,,,B,7",
        ),
        (
            "(function(){let received='unset';const value={toLocaleString(first){received=first;return 'x';}};[value].toLocaleString('locale','options');return received===undefined;})()",
            "true",
        ),
        (
            "Array.prototype.toLocaleString.call({length:3,0:1,2:2})",
            "1,,2",
        ),
        ("Array.prototype.toLocaleString.call('ab')", "a,b"),
    ]);
}

#[test]
fn array_locale_string_preserves_length_getter_and_invocation_order() {
    assert_all(&[(
        "(function(){\
            let log='';const element={get toLocaleString(){log+='method|';return function(){log+='call|';return {toString(){log+='string';return 'x';}};};}};\
            const source={get length(){log+='length|';return {valueOf(){log+='lengthValue|';return 1;}};},get 0(){log+='get0|';return element;}};\
            const result=Array.prototype.toLocaleString.call(source);return log+'#'+result;\
        })()",
        "length|lengthValue|get0|method|call|string#x",
    )]);
    assert_throw_kind(
        "return [{toLocaleString:1}].toLocaleString();",
        ExceptionKind::TypeError,
    );
    assert_throw_kind(
        "return [{toLocaleString(){return Symbol();}}].toLocaleString();",
        ExceptionKind::TypeError,
    );
}

#[test]
fn locale_string_methods_have_exact_builtin_shapes() {
    assert_all(&[
        ("Object.prototype.toLocaleString.length", "0"),
        ("Number.prototype.toLocaleString.length", "0"),
        ("BigInt.prototype.toLocaleString.length", "0"),
        ("Array.prototype.toLocaleString.length", "0"),
        ("Object.prototype.toLocaleString.name", "toLocaleString"),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'toLocaleString').enumerable",
            "false",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'toLocaleString').writable",
            "true",
        ),
        (
            "Object.getOwnPropertyDescriptor(Array.prototype,'toLocaleString').configurable",
            "true",
        ),
        (
            "Object.prototype.hasOwnProperty.call(Number.prototype.toLocaleString,'prototype')",
            "false",
        ),
        (
            "(function(){try{new Array.prototype.toLocaleString();}catch(error){return error instanceof TypeError;}})()",
            "true",
        ),
    ]);
}

#[test]
fn array_locale_string_scan_consumes_shared_instruction_fuel() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let scan = dynamic_function(
        &mut context,
        "return Array.prototype.toLocaleString.call({length:1000});",
    );
    let result = context.call(
        &scan,
        &[],
        ExecutionLimits::default().with_instruction_fuel(100),
    );
    assert!(matches!(
        result,
        Err(ExecutionError::InstructionLimitExceeded { limit: 100, .. })
    ));
}
